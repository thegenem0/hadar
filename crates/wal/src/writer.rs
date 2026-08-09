use std::path::PathBuf;

use mvcc::{KvStore, Mutation, Revision};
use storage_api::StorageEngine;
use tokio::sync::{mpsc, oneshot};

use crate::{error::Error, record, segment::Segment, wal::compact};

/// Most writes folded into a single flush.
///
/// Every write in a batch shares one fsync, so a larger cap means better
/// throughput under load and a longer worst-case wait for the writer that
/// opened the batch. The cap only binds when writers arrive faster than the
/// disk retires them; below that the batch closes as soon as the queue drains,
/// so latency at low load is unaffected by this value.
const MAX_BATCH: usize = 256;

/// How many writes may be queued before callers are made to wait.
///
/// Bounded so a burst cannot grow the queue without limit: once it fills, the
/// backpressure reaches the caller rather than becoming memory the process
/// cannot reclaim.
const QUEUE_DEPTH: usize = 1024;

/// One caller's write, and the channel to answer it on.
struct Request {
    mutations: Vec<Mutation>,
    respond: oneshot::Sender<Result<Vec<Revision>, Error>>,
}

/// What the writer thread accepts.
///
/// Checkpoints travel the same queue as writes so that the two are ordered against
/// each other by the queue itself, with no lock and no second path to the segment.
enum Message {
    Write(Request),
    Checkpoint(oneshot::Sender<Result<usize, Error>>),
}

/// Accepts writes and folds concurrent ones into shared flushes.
///
/// The batching buffer belongs to a single writer thread and is reached only
/// through a channel, so no lock is held across the flush — there is no shared
/// buffer to lock. Callers await a response rather than blocking, while the
/// thread that touches the disk is a real thread, so a slow flush never
/// occupies a runtime worker.
#[derive(Debug)]
pub(crate) struct Writer {
    submit: mpsc::Sender<Message>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Writer {
    /// Starts the writer thread, appending to `segment` under `dir`.
    pub(crate) fn start<E>(
        store: std::sync::Arc<KvStore<E>>,
        dir: PathBuf,
        segment: Segment,
        checkpoint_interval: u64,
    ) -> Self
    where
        E: StorageEngine,
    {
        let (submit, queue) = mpsc::channel(QUEUE_DEPTH);
        let thread = std::thread::Builder::new()
            .name("hadar-wal".to_owned())
            .spawn(move || run(&store, &dir, segment, queue, checkpoint_interval))
            .ok();

        Self { submit, thread }
    }

    /// Records `mutations` durably, then applies them, returning their revisions.
    ///
    /// # Errors
    ///
    /// Returns an error if the log cannot be written or the store cannot apply the batch.
    ///
    /// In both cases the mutations are not acknowledged,
    /// and a partial record left in the log is discarded on replay.
    pub(crate) async fn write(&self, mutations: Vec<Mutation>) -> Result<Vec<Revision>, Error> {
        let (respond, response) = oneshot::channel();
        self.submit
            .send(Message::Write(Request { mutations, respond }))
            .await
            .map_err(|_| Error::shutdown())?;

        response.await.map_err(|_| Error::shutdown())?
    }

    /// Flushes the store and discards the log records it no longer needs.
    ///
    /// # Errors
    ///
    /// Returns an error if the log has shut down, or if the flush or a removal fails.
    pub(crate) async fn checkpoint(&self) -> Result<usize, Error> {
        let (respond, response) = oneshot::channel();
        self.submit
            .send(Message::Checkpoint(respond))
            .await
            .map_err(|_| Error::shutdown())?;

        response.await.map_err(|_| Error::shutdown())?
    }

    /// Stops accepting writes and waits for those already queued to finish.
    pub(crate) fn shutdown(&mut self) {
        // Dropping the sender ends the thread's receive loop once the queue is
        // drained, so in-flight writes are answered rather than abandoned.
        let (idle, _) = mpsc::channel(1);
        drop(std::mem::replace(&mut self.submit, idle));

        if let Some(thread) = self.thread.take() {
            drop(thread.join());
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Receives writes, folds them into batches, and retires each batch.
fn run<E: StorageEngine>(
    store: &KvStore<E>,
    dir: &std::path::Path,
    mut segment: Segment,
    mut queue: mpsc::Receiver<Message>,
    checkpoint_interval: u64,
) {
    let mut batch: Vec<Request> = Vec::with_capacity(MAX_BATCH);
    let mut requested: Option<oneshot::Sender<Result<usize, Error>>> = None;
    let mut appended = 0_u64;

    // Everything already queued joins this batch and shares its flush.
    // Taking only what is waiting keeps a lone writer from paying for a
    // timeout that no second writer was going to arrive within.
    while let Some(message) = queue.blocking_recv() {
        match message {
            Message::Write(request) => batch.push(request),
            Message::Checkpoint(respond) => requested = Some(respond),
        }

        while batch.len() < MAX_BATCH {
            match queue.try_recv() {
                Ok(Message::Write(request)) => batch.push(request),
                Ok(Message::Checkpoint(respond)) => requested = Some(respond),
                Err(_) => break,
            }
        }

        if !batch.is_empty() {
            let outcome = retire(store, dir, &mut segment, &batch, &mut appended);
            answer(&mut batch, outcome);
        }

        if let Some(respond) = requested.take() {
            let outcome = compact(store, dir);
            if outcome.is_ok() {
                appended = 0;
            }
            drop(respond.send(outcome));
        } else if appended >= checkpoint_interval {
            match compact(store, dir) {
                Ok(_) => appended = 0,
                Err(error) => tracing::warn!(%error, "automatic checkpoint failed"),
            }
        }
    }
}

/// Writes a batch to the log, flushes it, and applies it to the store.
fn retire<E: StorageEngine>(
    store: &KvStore<E>,
    dir: &std::path::Path,
    segment: &mut Segment,
    batch: &[Request],
    appended: &mut u64,
) -> Result<Vec<Revision>, Error> {
    let mut bytes = Vec::new();
    let mut mutations = Vec::new();
    for request in batch {
        for mutation in &request.mutations {
            record::encode(mutation, &mut bytes)?;
            mutations.push(mutation.clone());
        }
    }

    // Roll over first, so a batch is never split across two segments and
    // recovery never has to stitch one back together.
    if segment.is_full() {
        let next = segment.roll_over(dir, store.applied())?;
        *segment = next;
    }
    segment.append(&bytes)?;
    *appended += bytes.len() as u64;

    // Only now is the batch durable, so only now may it become visible.
    // The applied position advances past every record written so far,
    // which is what recovery resumes from.
    let applied = store.applied() + mutations.len() as u64;

    store
        .apply(&mutations, applied)
        .map_err(|e| Error::apply(e.to_string()))
}

/// Answers every caller in a batch, splitting the batch's revisions among them.
fn answer(batch: &mut Vec<Request>, outcome: Result<Vec<Revision>, Error>) {
    match outcome {
        Ok(revisions) => {
            let mut revisions = revisions.into_iter();
            for request in batch.drain(..) {
                let mine = revisions.by_ref().take(request.mutations.len()).collect();
                drop(request.respond.send(Ok(mine)));
            }
        }
        Err(error) => {
            for request in batch.drain(..) {
                drop(request.respond.send(Err(error.batch_failed())));
            }
        }
    }
}

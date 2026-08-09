use std::path::PathBuf;

use mvcc::{KvStore, Mutation, Revision};
use storage_api::StorageEngine;
use tokio::sync::{mpsc, oneshot};

use crate::{error::Error, record, segment::Segment};

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

/// Accepts writes and folds concurrent ones into shared flushes.
///
/// The batching buffer belongs to a single writer thread and is reached only
/// through a channel, so no lock is held across the flush — there is no shared
/// buffer to lock. Callers await a response rather than blocking, while the
/// thread that touches the disk is a real thread, so a slow flush never
/// occupies a runtime worker.
#[derive(Debug)]
pub(crate) struct Writer {
    submit: mpsc::Sender<Request>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Writer {
    /// Starts the writer thread, appending to `segment` under `dir`.
    pub(crate) fn start<E>(
        store: std::sync::Arc<KvStore<E>>,
        dir: PathBuf,
        segment: Segment,
    ) -> Self
    where
        E: StorageEngine,
    {
        let (submit, queue) = mpsc::channel(QUEUE_DEPTH);
        let thread = std::thread::Builder::new()
            .name("hadar-wal".to_owned())
            .spawn(move || run(&store, &dir, segment, queue))
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
            .send(Request { mutations, respond })
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
    mut queue: mpsc::Receiver<Request>,
) {
    let mut batch: Vec<Request> = Vec::with_capacity(MAX_BATCH);

    // Everything already queued joins this batch and shares its flush.
    // Taking only what is waiting keeps a lone writer from paying for a
    // timeout that no second writer was going to arrive within.
    while let Some(first) = queue.blocking_recv() {
        batch.push(first);

        while batch.len() < MAX_BATCH {
            match queue.try_recv() {
                Ok(request) => batch.push(request),
                Err(_) => break,
            }
        }

        let outcome = retire(store, dir, &mut segment, &batch);
        answer(&mut batch, outcome);
    }
}

/// Writes a batch to the log, flushes it, and applies it to the store.
fn retire<E: StorageEngine>(
    store: &KvStore<E>,
    dir: &std::path::Path,
    segment: &mut Segment,
    batch: &[Request],
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
        *segment = Segment::create(dir, store.applied(), segment.limit())?;
    }
    segment.append(&bytes)?;

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
            let message = error.to_string();
            for request in batch.drain(..) {
                drop(request.respond.send(Err(Error::batch_failed(&message))));
            }
        }
    }
}

use redb::{
    CommitError, DatabaseError, SetDurabilityError, StorageError, TableError, TransactionError,
};
use storage_api::Error;

pub(crate) fn storage(context: &'static str, source: StorageError) -> Error {
    match source {
        StorageError::Corrupted(_) => Error::corrupt(context).with_source(source),
        StorageError::Io(_) => Error::io(context).with_source(source),
        other => Error::other(context).with_source(other),
    }
}

pub(crate) fn database(context: &'static str, source: DatabaseError) -> Error {
    match source {
        DatabaseError::Storage(inner) => storage(context, inner),
        DatabaseError::RepairAborted | DatabaseError::UpgradeRequired(_) => {
            Error::corrupt(context).with_source(source)
        }
        other => Error::other(context).with_source(other),
    }
}

pub(crate) fn transaction(context: &'static str, source: TransactionError) -> Error {
    match source {
        TransactionError::Storage(inner) => storage(context, inner),
        other => Error::other(context).with_source(other),
    }
}

pub(crate) fn table(context: &'static str, source: TableError) -> Error {
    match source {
        TableError::Storage(inner) => storage(context, inner),
        TableError::TableDoesNotExist(_) => Error::corrupt(context).with_source(source),
        other => Error::other(context).with_source(other),
    }
}

pub(crate) fn commit(context: &'static str, source: CommitError) -> Error {
    match source {
        CommitError::Storage(inner) => storage(context, inner),
        other => Error::other(context).with_source(other),
    }
}

pub(crate) fn durability(context: &'static str, source: SetDurabilityError) -> Error {
    Error::other(context).with_source(source)
}

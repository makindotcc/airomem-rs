use std::{ffi::OsString, sync::PoisonError};

pub type StoreResult<T> = std::result::Result<T, StoreError>;

pub type AnyError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(thiserror::Error, Debug)]
pub enum StoreError {
    #[error("could not encode journal entry")]
    EncodeJournalEntry(AnyError),
    #[error("could not decode journal entry")]
    DecodeJournalEntry(AnyError),
    #[error("Could not encode snapshot: {0:?}")]
    EncodeSnapshot(AnyError),
    #[error("Could not restore snapshot: {0:?}. Remove it to rebuild it from journal logs")]
    DecodeSnapshot(AnyError),
    #[error("io error ocurred while accessing journal file: {0}")]
    JournalIO(std::io::Error),
    #[error("io error ocurred while accessing snapshot file: {0}")]
    SnapshotIO(std::io::Error),
    #[error("general store file related io error: {0}")]
    FileIO(std::io::Error),
    #[error("invalid journal file name: {0:?}")]
    JournalInvalidFileName(Option<OsString>),
    #[error("state poisoned")]
    StatePoisoned,
    #[error("Join task error")]
    JoinError(tokio::task::JoinError),
}

impl<T> From<PoisonError<T>> for StoreError {
    fn from(_: PoisonError<T>) -> Self {
        Self::StatePoisoned
    }
}

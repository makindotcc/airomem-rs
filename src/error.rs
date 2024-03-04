use std::{ffi::OsString, sync::PoisonError};

pub type StoreResult<T> = std::result::Result<T, StoreError>;

#[derive(thiserror::Error, Debug)]
pub enum StoreError {
    #[error("could not encode journal entry")]
    EncodeJournalEntry(Box<dyn std::error::Error>),
    #[error("could not decode journal entry")]
    DecodeJournalEntry(Box<dyn std::error::Error>),
    #[error("io error ocurred while accessing journal file: {0}")]
    JournalIO(std::io::Error),
    #[error("general store file related io error: {0}")]
    FileIO(std::io::Error),
    #[error("invalid journal file name: {0:?}")]
    JournalInvalidFileName(Option<OsString>),
    #[error("state poisoned")]
    StatePoisoned,
}

impl<T> From<PoisonError<T>> for StoreError {
    fn from(_: PoisonError<T>) -> Self {
        Self::StatePoisoned
    }
}

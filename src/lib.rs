use error::{StoreError, StoreResult};
use std::ffi::OsStr;
use std::num::NonZeroUsize;
use std::ops::Deref;
use std::path::Path;
use std::path::PathBuf;
use tokio::fs::{self, File};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;
use tokio::sync::RwLockReadGuard;

pub mod error;

pub type JsonStore<D> = Store<D, JsonSerializer>;

pub struct Store<T, S>
where
    T: State,
{
    inner: RwLock<StoreInner<T>>,
    serializer: S,
    options: StoreOptions,
}

impl<T, S, C> Store<T, S>
where
    T: State<Command = C>,
{
    pub async fn open(
        serializer: S,
        options: StoreOptions,
        dir: impl Into<PathBuf>,
    ) -> StoreResult<Self>
    where
        S: Serializer<T> + Serializer<C>,
    {
        let dir: PathBuf = dir.into();
        fs::create_dir_all(&dir).await.map_err(StoreError::FileIO)?;
        let old_journals = JournalFile::read_dir(&dir).await?;
        let latest_journal_id: JournalId =
            old_journals.last().map(|(id, _)| *id).unwrap_or_default();

        let mut store = Self {
            inner: RwLock::new(StoreInner {
                state: Default::default(),
                journal: JournalFile::open(&dir, latest_journal_id + 1).await?,
            }),
            serializer,
            options,
        };
        store.rebuild(&old_journals).await?;
        Ok(store)
    }

    pub async fn commit<'a>(&self, command: C) -> StoreResult<()>
    where
        S: Serializer<C>,
    {
        let serialized: Vec<u8> = self
            .serializer
            .serialize(&command)
            .map_err(|err| StoreError::EncodeJournalEntry(err.into()))?;
        let mut inner = self.inner.write().await;
        let journal_file = inner
            .writable_journal(self.options.max_journal_entries)
            .await?;
        journal_file
            .append(&serialized)
            .await
            .map_err(StoreError::JournalIO)?;
        inner.state.execute(command);
        Ok(())
    }

    pub async fn query(&self) -> StoreResult<Query<'_, T>> {
        Ok(Query(self.inner.read().await))
    }

    async fn rebuild(&mut self, journals: &Vec<(JournalId, PathBuf)>) -> StoreResult<()>
    where
        S: Serializer<C>,
    {
        let mut inner = self.inner.write().await;
        for (_, journal) in journals {
            let mut journal_file = File::open(journal).await.map_err(StoreError::JournalIO)?;
            for command in JournalFile::parse(&self.serializer, &mut journal_file).await? {
                inner.state.execute(command);
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct StoreOptions {
    max_journal_entries: NonZeroUsize,
}

impl StoreOptions {
    pub fn max_journal_entries(&mut self, value: NonZeroUsize) {
        self.max_journal_entries = value;
    }
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            max_journal_entries: NonZeroUsize::new(16384).unwrap(),
        }
    }
}

struct StoreInner<T> {
    state: T,
    journal: JournalFile,
}

impl<T> StoreInner<T> {
    async fn writable_journal(
        &mut self,
        max_entries: NonZeroUsize,
    ) -> StoreResult<&mut JournalFile> {
        if self.journal.written_entries >= max_entries.into() || self.journal.file.is_none() {
            self.create_new_journal().await?;
        }
        Ok(&mut self.journal)
    }

    async fn create_new_journal(&mut self) -> StoreResult<()> {
        self.journal = JournalFile::open(self.journal.dir.clone(), self.journal.id + 1).await?;
        Ok(())
    }
}

pub struct Query<'a, T>(RwLockReadGuard<'a, StoreInner<T>>);

impl<'a, T> Deref for Query<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.state
    }
}

type JournalId = u32;

#[derive(Debug)]
struct JournalFile {
    id: JournalId,
    dir: PathBuf,
    file: Option<File>,
    written_entries: usize,
}

impl JournalFile {
    pub async fn open(dir: impl Into<PathBuf>, id: JournalId) -> StoreResult<Self> {
        let dir: PathBuf = dir.into();
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("{:0>10}.journal", id)))
            .await
            .map_err(StoreError::JournalIO)?;
        Ok(Self {
            id,
            dir,
            file: Some(file),
            written_entries: 0,
        })
    }

    async fn append(&mut self, command: &[u8]) -> std::io::Result<()> {
        let file = match &mut self.file {
            Some(file) => file,
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "file poisoned",
                ))
            }
        };
        if let Err(err) = Self::write_data_and_sync(file, command).await {
            self.file = None;
            return Err(err);
        }
        self.written_entries += 1;
        Ok(())
    }

    async fn write_data_and_sync(file: &mut File, data: &[u8]) -> std::io::Result<()> {
        file.write_all(data).await?;
        file.sync_all().await?;
        Ok(())
    }

    async fn parse<S, C>(serializer: &S, file: &mut File) -> StoreResult<Vec<C>>
    where
        S: Serializer<C>,
    {
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .await
            .map_err(StoreError::JournalIO)?;

        let mut cursor = &buf[..];
        let mut commands: Vec<C> = Vec::new();
        while let Some(entry) = serializer.deserialize(&mut cursor).transpose() {
            let command = entry.map_err(|err| StoreError::DecodeJournalEntry(err.into()))?;
            commands.push(command);
        }
        Ok(commands)
    }

    async fn read_dir(path: impl AsRef<Path>) -> StoreResult<Vec<(JournalId, PathBuf)>> {
        let mut journals = Vec::new();
        let mut read_dir = fs::read_dir(&path).await.map_err(StoreError::JournalIO)?;
        while let Some(entry) = read_dir.next_entry().await.map_err(StoreError::JournalIO)? {
            let entry_path = entry.path();
            if entry_path.extension().is_some_and(|ext| ext == "journal") {
                let journal_id = entry_path
                    .file_stem()
                    .and_then(OsStr::to_str)
                    .and_then(|path| path.parse::<JournalId>().ok())
                    .ok_or_else(|| {
                        StoreError::JournalInvalidFileName(
                            entry_path.file_stem().map(OsStr::to_owned),
                        )
                    })?;
                journals.push((journal_id, entry_path));
            }
        }
        journals.sort_by_key(|(id, _)| *id);
        Ok(journals)
    }
}

pub trait State: Default {
    type Command;

    fn execute(&mut self, command: Self::Command);
}

pub trait Serializer<C> {
    type Error: std::error::Error + 'static;

    fn serialize(&self, command: &C) -> Result<Vec<u8>, Self::Error>;

    fn deserialize<R>(&self, reader: R) -> Result<Option<C>, Self::Error>
    where
        R: std::io::Read;
}

pub struct JsonSerializer;

impl<D> Serializer<D> for JsonSerializer
where
    D: serde::Serialize,
    D: for<'a> serde::Deserialize<'a>,
{
    type Error = serde_json::Error;

    fn serialize(&self, data: &D) -> Result<Vec<u8>, Self::Error> {
        serde_json::to_vec(data).map(|mut bytes| {
            // separate logs by enter for human readers ;]
            bytes.push(b'\n');
            bytes
        })
    }

    fn deserialize<'a, R>(&self, reader: R) -> Result<Option<D>, Self::Error>
    where
        R: std::io::Read,
    {
        let mut deserializer = serde_json::Deserializer::from_reader(reader);
        match serde::de::Deserialize::deserialize(&mut deserializer) {
            Ok(data) => Ok(data),
            Err(err) if err.is_eof() => {
                // ignore partially saved entry, but
                // is eof always equal to interrupted write?
                // todo:
                // return error for half-written command and give option to api to handle it
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use tempfile::tempdir;

    #[derive(Serialize, Deserialize, Default, Debug)]
    struct Counter {
        value: usize,
    }

    impl super::State for Counter {
        type Command = CounterCommand;

        fn execute(&mut self, command: CounterCommand) {
            match command {
                CounterCommand::Increase => {
                    self.value += 1;
                }
            }
        }
    }

    #[derive(Serialize, Deserialize, Debug)]
    enum CounterCommand {
        Increase,
    }

    #[tokio::test]
    async fn test_journal_chunking() {
        let dir = tempdir().unwrap();
        let mut options = StoreOptions::default();
        options.max_journal_entries(NonZeroUsize::new(2).unwrap());
        let store: Store<Counter, _> = Store::open(JsonSerializer, options, dir.path())
            .await
            .unwrap();
        let first_id = store.inner.read().await.journal.id;
        store.commit(CounterCommand::Increase).await.unwrap();
        store.commit(CounterCommand::Increase).await.unwrap();
        assert_eq!(store.inner.read().await.journal.id, first_id);
        store.commit(CounterCommand::Increase).await.unwrap();
        assert_eq!(store.inner.read().await.journal.id, first_id + 1);
    }
}

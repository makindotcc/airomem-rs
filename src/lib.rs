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
    S: Serializer<T> + Serializer<C>,
{
    pub async fn open(
        serializer: S,
        options: StoreOptions,
        dir: impl Into<PathBuf>,
    ) -> StoreResult<Self> {
        let dir: PathBuf = dir.into();
        fs::create_dir_all(&dir).await.map_err(StoreError::FileIO)?;
        let persistence_actions = PersistenceAction::rebuild(&dir).await?;
        let recovered_version: SnapshotVersion = persistence_actions
            .last()
            .map(|action| action.snapshot_version())
            .unwrap_or_default();

        let next_version = recovered_version + 1;
        let mut store = Self {
            inner: RwLock::new(StoreInner {
                state: Default::default(),
                journal: JournalFile::open(&dir, next_version).await?,
                dir,
                snapshot_version: next_version,
            }),
            serializer,
            options,
        };
        store.rebuild(persistence_actions).await?;
        Ok(store)
    }

    pub async fn commit<'a>(&self, command: C) -> StoreResult<()> {
        let serialized: Vec<u8> = self
            .serializer
            .serialize(&command)
            .map_err(|err| StoreError::EncodeJournalEntry(err.into()))?;
        let mut inner = self.inner.write().await;
        let journal_file = inner
            .writable_journal(self.options.max_journal_entries, &self.serializer)
            .await?;
        journal_file
            .append(&serialized)
            .await
            .map_err(StoreError::JournalIO)?;
        inner.state.execute(command);
        Ok(())
    }

    /// Returns immutable, read only store data.
    /// While [QueryGuard] is not dropped, any store updates are locked.
    pub async fn query(&self) -> QueryGuard<'_, T> {
        QueryGuard(self.inner.read().await)
    }

    async fn rebuild(&mut self, persistence_actions: Vec<PersistenceAction>) -> StoreResult<()> {
        let mut inner = self.inner.write().await;
        for action in persistence_actions {
            match action {
                PersistenceAction::Snapshot { path, .. } => {
                    let file = fs::read(path).await.map_err(StoreError::SnapshotIO)?;
                    inner.state = self
                        .serializer
                        .deserialize(&file[..])
                        .map_err(|err| StoreError::DecodeSnapshot(Box::new(err)))?
                        .ok_or(StoreError::DecodeSnapshot(Box::new(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "corrupted snapshot",
                        ))))?;
                }
                PersistenceAction::Journal { path, .. } => {
                    let mut file = File::open(path).await.map_err(StoreError::JournalIO)?;
                    for command in JournalFile::parse(&self.serializer, &mut file).await? {
                        inner.state.execute(command);
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
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
    dir: PathBuf,
    snapshot_version: SnapshotVersion,
    journal: JournalFile,
}

impl<T> StoreInner<T> {
    async fn writable_journal<S>(
        &mut self,
        max_entries: NonZeroUsize,
        serializer: &S,
    ) -> StoreResult<&mut JournalFile>
    where
        S: Serializer<T>,
    {
        if self.journal.written_entries >= max_entries.into() || self.journal.file.is_none() {
            self.create_new_journal(serializer).await?;
        }
        Ok(&mut self.journal)
    }

    async fn create_new_journal<S>(&mut self, serializer: &S) -> StoreResult<()>
    where
        S: Serializer<T>,
    {
        self.snapshot(serializer).await?;
        self.journal = JournalFile::open(self.dir.clone(), self.snapshot_version).await?;
        Ok(())
    }

    /// Writes fully serialized data to snapshot file.
    /// From now, in case of recovery, it has priority over journal file with same [SnapshotVersion].
    async fn snapshot<S>(&mut self, serializer: &S) -> StoreResult<()>
    where
        S: Serializer<T>,
    {
        let serialized = serializer
            .serialize(&self.state)
            .map_err(|err| StoreError::EncodeSnapshot(Box::new(err)))?;
        fs::write(
            self.dir
                .join(format!("{:0>10}.snapshot", self.snapshot_version)),
            serialized,
        )
        .await
        .map_err(StoreError::SnapshotIO)?;
        self.snapshot_version += 1;
        Ok(())
    }
}

pub struct QueryGuard<'a, T>(RwLockReadGuard<'a, StoreInner<T>>);

impl<'a, T> Deref for QueryGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.state
    }
}

type SnapshotVersion = u32;

#[derive(Debug)]
struct JournalFile {
    file: Option<File>,
    written_entries: usize,
}

impl JournalFile {
    pub async fn open(dir: impl Into<PathBuf>, id: SnapshotVersion) -> StoreResult<Self> {
        let dir: PathBuf = dir.into();
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("{:0>10}.journal", id)))
            .await
            .map_err(StoreError::JournalIO)?;
        Ok(Self {
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
}

enum PersistenceAction {
    Snapshot {
        snapshot_version: SnapshotVersion,
        path: PathBuf,
    },
    Journal {
        // Version of data snapshot which may not exist yet.
        snapshot_version: SnapshotVersion,
        path: PathBuf,
    },
}

impl PersistenceAction {
    /// Get only required actions to rebuild latest store state.
    /// All journals with [SnapshotVersion] lower or equal than latest snapshot's version are skipped.
    ///
    /// E.g. directory containing:
    /// - 0000000001.journal
    /// - 0000000001.snapshot
    /// - 0000000002.journal
    /// will return vec of:
    /// - 0000000001.snapshot
    /// - 0000000002.journal
    async fn rebuild(dir_path: impl AsRef<Path>) -> StoreResult<Vec<PersistenceAction>> {
        let mut actions = Self::read_dir(&dir_path).await?;
        actions.sort_by_key(|action| action.snapshot_version());
        let latest_snapshot_id = actions
            .iter()
            .filter_map(|action| match action {
                PersistenceAction::Snapshot {
                    snapshot_version, ..
                } => Some(*snapshot_version),
                _ => None,
            })
            .last();
        if let Some(latest_snapshot_id) = latest_snapshot_id {
            actions.retain(|action| match action {
                PersistenceAction::Journal {
                    snapshot_version, ..
                } => *snapshot_version > latest_snapshot_id,
                PersistenceAction::Snapshot {
                    snapshot_version, ..
                } => *snapshot_version == latest_snapshot_id,
            });
        }
        Ok(actions)
    }

    /// Read all past journals and snapshots.
    async fn read_dir(path: impl AsRef<Path>) -> StoreResult<Vec<PersistenceAction>> {
        let mut actions = Vec::new();
        let mut read_dir = fs::read_dir(&path).await.map_err(StoreError::JournalIO)?;
        while let Some(entry) = read_dir.next_entry().await.map_err(StoreError::JournalIO)? {
            let entry_path = entry.path();
            let action_id = entry_path
                .file_stem()
                .and_then(OsStr::to_str)
                .and_then(|path| path.parse::<SnapshotVersion>().ok())
                .ok_or_else(|| {
                    StoreError::JournalInvalidFileName(entry_path.file_stem().map(OsStr::to_owned))
                })?;
            match entry_path.extension() {
                Some(extension) if extension == "journal" => {
                    actions.push(PersistenceAction::Journal {
                        snapshot_version: action_id,
                        path: entry_path,
                    });
                }
                Some(extension) if extension == "snapshot" => {
                    actions.push(PersistenceAction::Snapshot {
                        snapshot_version: action_id,
                        path: entry_path,
                    });
                }
                _ => {}
            }
        }
        Ok(actions)
    }

    fn snapshot_version(&self) -> u32 {
        match self {
            PersistenceAction::Snapshot {
                snapshot_version, ..
            } => *snapshot_version,
            PersistenceAction::Journal {
                snapshot_version, ..
            } => *snapshot_version,
        }
    }
}

pub trait State: Default {
    type Command;

    fn execute(&mut self, command: Self::Command);
}

pub trait Serializer<C> {
    type Error: std::error::Error + Send + Sync + 'static;

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
        let first_ver = store.inner.read().await.snapshot_version;
        store.commit(CounterCommand::Increase).await.unwrap();
        store.commit(CounterCommand::Increase).await.unwrap();
        assert_eq!(store.inner.read().await.snapshot_version, first_ver);
        store.commit(CounterCommand::Increase).await.unwrap();
        assert_eq!(store.inner.read().await.snapshot_version, first_ver + 1);
    }
}

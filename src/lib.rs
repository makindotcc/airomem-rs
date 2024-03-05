use error::{StoreError, StoreResult};
use std::convert::Infallible;
use std::ffi::OsStr;
use std::num::NonZeroUsize;
use std::ops::Deref;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs::{self, File};
use tokio::io::BufWriter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;
use tokio::sync::RwLockReadGuard;
use tokio::task::JoinHandle;
use tokio::time::sleep;

pub mod error;

pub type JsonStore<D> = Store<D, JsonSerializer>;

pub struct Store<T, S>
where
    T: State,
{
    inner: SharedStoreInner<T>,
    serializer: S,
    options: StoreOptions,
    _flush_guard: Option<FlushGuard>,
}

impl<T, S> Store<T, S>
where
    T: State,
{
    pub async fn open<C>(
        serializer: S,
        options: StoreOptions,
        dir: impl Into<PathBuf>,
    ) -> StoreResult<Self>
    where
        T: State<Command = C> + Sync + Send + 'static,
        S: Serializer<C> + Serializer<T>,
    {
        let dir: PathBuf = dir.into();
        fs::create_dir_all(&dir).await.map_err(StoreError::FileIO)?;
        let persistence_actions = PersistenceAction::rebuild(&dir).await?;
        let next_snapshot_version: SnapshotVersion = persistence_actions
            .last()
            .map(|action| match action {
                PersistenceAction::Snapshot { version, .. } => *version + 1,
                PersistenceAction::Journal { version, .. } => *version,
            })
            .unwrap_or_default();
        let inner = Arc::new(RwLock::new({
            let mut inner = StoreInner::new(dir, next_snapshot_version).await?;
            inner.rebuild(&serializer, persistence_actions).await?;
            inner
        }));
        let flush_guard = match options.journal_flush_policy {
            JournalFlushPolicy::EveryCommit | JournalFlushPolicy::Manually => None,
            JournalFlushPolicy::Every(interval) => {
                Some(Self::start_flusher(interval, Arc::clone(&inner)))
            }
        };
        Ok(Self {
            inner,
            serializer,
            options,
            _flush_guard: flush_guard,
        })
    }

    pub async fn commit<'a, C>(&self, command: C) -> StoreResult<()>
    where
        T: State<Command = C> + Sync + Send + 'static,
        S: Serializer<C> + Serializer<T>,
    {
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
        if let JournalFlushPolicy::EveryCommit = self.options.journal_flush_policy {
            journal_file
                .flush_and_sync()
                .await
                .map_err(StoreError::JournalIO)?;
        }
        inner.state.execute(command);
        Ok(())
    }

    /// Returns immutable, read only store data.
    /// While [QueryGuard] is not dropped, any store updates are locked.
    pub async fn query(&self) -> QueryGuard<'_, T> {
        QueryGuard(self.inner.read().await)
    }

    /// Flushes buffered commands to file and synces it using [File::sync_all].
    pub async fn flush_and_sync(&self) -> StoreResult<()> {
        self.inner
            .write()
            .await
            .journal
            .flush_and_sync()
            .await
            .map_err(StoreError::JournalIO)?;
        Ok(())
    }

    fn start_flusher<C>(interval: Duration, inner: SharedStoreInner<T>) -> FlushGuard
    where
        T: State<Command = C> + Sync + Send + 'static,
        S: Serializer<C>,
    {
        FlushGuard(tokio::spawn(async move {
            loop {
                sleep(interval).await;
                let mut inner = inner.write().await;
                if let Err(err) = inner.journal.flush_and_sync().await {
                    eprintln!("Could not flush journal log: {err:?}");
                }
            }
        }))
    }
}

impl<T, S> Drop for Store<T, S>
where
    T: State,
{
    fn drop(&mut self) {
        if self.options.flush_synchronously_on_drop {
            futures::executor::block_on(async move {
                if let Err(err) = self.flush_and_sync().await {
                    eprintln!("Could not flush journal log on drop: {err:?}");
                };
            });
        }
    }
}

#[derive(Debug, Clone)]
pub struct StoreOptions {
    max_journal_entries: NonZeroUsize,
    journal_flush_policy: JournalFlushPolicy,
    flush_synchronously_on_drop: bool,
}

impl StoreOptions {
    /// Maximal amount of persisted commands in journal file before a
    /// snapshot is created and a new journal file.
    pub fn max_journal_entries(mut self, value: NonZeroUsize) -> Self {
        self.max_journal_entries = value;
        self
    }

    /// Dictates [Store] how often commands should be saved to [JournalFile].
    pub fn journal_flush_policy(mut self, value: JournalFlushPolicy) -> Self {
        self.journal_flush_policy = value;
        self
    }

    /// Should [Store] synchronously [Store::flush_and_sync] on [Store::drop]?
    pub fn flush_synchronously_on_drop(mut self, value: bool) -> Self {
        self.flush_synchronously_on_drop = value;
        self
    }
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            max_journal_entries: NonZeroUsize::new(16384).unwrap(),
            journal_flush_policy: JournalFlushPolicy::EveryCommit,
            flush_synchronously_on_drop: true,
        }
    }
}

/// Dictates [Store] how often commands should be saved to [JournalFile].
#[derive(Debug, Clone, Copy)]
pub enum JournalFlushPolicy {
    /// Slowest, but safest. Immediately saves command to file on [Store::commit].
    EveryCommit,
    /// [Store] will own background task that will attempt to flush commands every defined [Duration].
    /// Commands commited between flushes might be lost in case of crash.
    /// Any flush errors are printed to stderr using [eprintln!].
    Every(Duration),
    /// You are responsible for flushing data to file using [Store::flush_and_sync].
    Manually,
}

struct FlushGuard(JoinHandle<Infallible>);

impl Drop for FlushGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

type SharedStoreInner<T> = Arc<RwLock<StoreInner<T>>>;

struct StoreInner<T> {
    state: T,
    dir: PathBuf,
    next_snapshot_version: SnapshotVersion,
    journal: JournalFile,
}

impl<T> StoreInner<T>
where
    T: State,
{
    async fn new(dir: PathBuf, next_snapshot_version: SnapshotVersion) -> StoreResult<Self>
    where
        T: Default,
    {
        Ok(Self {
            state: Default::default(),
            journal: JournalFile::open(&dir, next_snapshot_version).await?,
            dir,
            next_snapshot_version,
        })
    }

    async fn rebuild<C, S>(
        &mut self,
        serializer: &S,
        persistence_actions: Vec<PersistenceAction>,
    ) -> StoreResult<()>
    where
        T: State<Command = C>,
        S: Serializer<T> + Serializer<C>,
    {
        for action in persistence_actions {
            match action {
                PersistenceAction::Snapshot { path, .. } => {
                    let file = fs::read(path).await.map_err(StoreError::SnapshotIO)?;
                    self.state = serializer
                        .deserialize(&file[..])
                        .map_err(|err| StoreError::DecodeSnapshot(Box::new(err)))?
                        .ok_or(StoreError::DecodeSnapshot(Box::new(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "corrupted snapshot",
                        ))))?;
                }
                PersistenceAction::Journal { path, .. } => {
                    let mut file = File::open(path).await.map_err(StoreError::JournalIO)?;
                    for command in JournalFile::parse::<S, C>(serializer, &mut file).await? {
                        self.state.execute(command);
                    }
                }
            }
        }
        Ok(())
    }

    async fn writable_journal<S>(
        &mut self,
        max_entries: NonZeroUsize,
        serializer: &S,
    ) -> StoreResult<&mut JournalFile>
    where
        S: Serializer<T>,
    {
        if self.journal.written_entries >= max_entries.into() || self.journal.writer.is_none() {
            self.create_new_journal(serializer).await?;
        }
        Ok(&mut self.journal)
    }

    async fn create_new_journal<S>(&mut self, serializer: &S) -> StoreResult<()>
    where
        S: Serializer<T>,
    {
        self.journal
            .flush_and_sync()
            .await
            .map_err(StoreError::JournalIO)?;
        self.snapshot(serializer).await?;
        self.journal = JournalFile::open(self.dir.clone(), self.next_snapshot_version).await?;
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
                .join(format!("{:0>10}.snapshot", self.next_snapshot_version)),
            serialized,
        )
        .await
        .map_err(StoreError::SnapshotIO)?;
        self.next_snapshot_version += 1;
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
    writer: Option<BufWriter<File>>,
    written_entries: usize,
}

impl JournalFile {
    pub async fn open(dir: impl Into<PathBuf>, version: SnapshotVersion) -> StoreResult<Self> {
        let dir: PathBuf = dir.into();
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("{:0>10}.journal", version)))
            .await
            .map_err(StoreError::JournalIO)?;
        Ok(Self {
            writer: Some(BufWriter::new(file)),
            written_entries: 0,
        })
    }

    async fn append(&mut self, command: &[u8]) -> std::io::Result<()> {
        let writer = self.get_writer()?;
        if let Err(err) = writer.write_all(command).await {
            self.writer = None;
            return Err(err);
        }
        self.written_entries += 1;
        Ok(())
    }

    async fn flush_and_sync(&mut self) -> std::io::Result<()> {
        let writer = self.get_writer()?;
        writer.flush().await?;
        writer.get_mut().sync_all().await?;
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

    fn get_writer(&mut self) -> std::io::Result<&mut BufWriter<File>> {
        match &mut self.writer {
            Some(writer) => Ok(writer),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "file poisoned",
            )),
        }
    }
}

enum PersistenceAction {
    Snapshot {
        version: SnapshotVersion,
        path: PathBuf,
    },
    Journal {
        /// A version of data snapshot that may not yet exist due to
        /// an journal being incomplete.
        version: SnapshotVersion,
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
        let latest_version = actions
            .iter()
            .filter_map(|action| match action {
                PersistenceAction::Snapshot {
                    version: snapshot_version,
                    ..
                } => Some(*snapshot_version),
                _ => None,
            })
            .last();
        if let Some(latest_version) = latest_version {
            actions.retain(|action| match action {
                PersistenceAction::Journal { version, .. } => *version > latest_version,
                PersistenceAction::Snapshot { version, .. } => *version == latest_version,
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
            let version = entry_path
                .file_stem()
                .and_then(OsStr::to_str)
                .and_then(|path| path.parse::<SnapshotVersion>().ok())
                .ok_or_else(|| {
                    StoreError::JournalInvalidFileName(entry_path.file_stem().map(OsStr::to_owned))
                })?;
            match entry_path.extension() {
                Some(extension) if extension == "journal" => {
                    actions.push(PersistenceAction::Journal {
                        version,
                        path: entry_path,
                    });
                }
                Some(extension) if extension == "snapshot" => {
                    actions.push(PersistenceAction::Snapshot {
                        version,
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
            PersistenceAction::Snapshot { version, .. } => *version,
            PersistenceAction::Journal { version, .. } => *version,
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
        let options = StoreOptions::default().max_journal_entries(NonZeroUsize::new(2).unwrap());
        let store: Store<Counter, _> = Store::open(JsonSerializer, options, dir.path())
            .await
            .unwrap();
        let first_ver = store.inner.read().await.next_snapshot_version;
        store.commit(CounterCommand::Increase).await.unwrap();
        store.commit(CounterCommand::Increase).await.unwrap();
        assert_eq!(store.inner.read().await.next_snapshot_version, first_ver);
        store.commit(CounterCommand::Increase).await.unwrap();
        assert_eq!(
            store.inner.read().await.next_snapshot_version,
            first_ver + 1
        );
    }

    #[tokio::test]
    async fn test_retake_unfulfilled_journal_on_recovery() {
        let dir = tempdir().unwrap();
        let options = StoreOptions::default().max_journal_entries(NonZeroUsize::new(10).unwrap());
        let first_ver = {
            let store: Store<Counter, _> = Store::open(JsonSerializer, options.clone(), dir.path())
                .await
                .unwrap();
            store.commit(CounterCommand::Increase).await.unwrap();
            let ver = store.inner.read().await.next_snapshot_version;
            ver
        };

        let store: Store<Counter, _> = Store::open(JsonSerializer, options, dir.path())
            .await
            .unwrap();
        store.commit(CounterCommand::Increase).await.unwrap();
        assert_eq!(store.inner.read().await.next_snapshot_version, first_ver);
    }
}

use error::{StoreError, StoreResult};
use std::convert::Infallible;
use std::ffi::OsStr;
use std::marker::PhantomData;
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
pub mod tx;

pub use tx::Tx;

pub type JsonStore<D, T> = Store<D, T, JsonSerializer>;

/// [Store] is clonable (so do not wrap it in [Arc]!), thread-safe implementation of system prevalence.
///
/// Some methods like [Store::commit] take mutable reference to prevent easiest
/// deadlocks in one function like:
/// ```compile_fail
/// let read_locked = store.query().await; // immutable borrow occurs here
/// store.commit(...).await // mutable borrow occurs here, compilation fails
/// ```
/// So do not wrap it using [Arc] and [Mutex]/[RwLock]. Just [Clone] it.
pub struct Store<D, T, S> {
    inner: Arc<StoreInner<D, T, S>>,
}

impl<D, T, S> Clone for Store<D, T, S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<D, T, S> Store<D, T, S>
where
    D: Default + Send + Sync + 'static,
    T: Tx<D>,
    S: Serializer<D> + Serializer<T>,
{
    /// Restores [Store] state using files from given directory.
    /// If none, new store is created with [`<D>`] data [Default::default].
    pub async fn open(
        serializer: S,
        options: StoreOptions,
        dir: impl Into<PathBuf>,
    ) -> StoreResult<Self> {
        let inner = StoreInner::open(serializer, options, dir).await?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Persists [Tx] to file(s) and calls [Tx::execute] on store data.
    /// Returns tx result
    ///
    /// Takes mutable reference to prevent some kind of deadlocks e.g.
    /// committing tx, while holding still [QueryGuard] in one, single threaded function:
    /// ```compile_fail
    /// async fn request_handler(mut store: JsonStore<Counter, CounterTx>) -> StoreResult<()> {
    ///     let read_locked = store.query().await; // immutable borrow occurs here
    ///     store.commit(()).await // mutable borrow occurs here, compilation fails
    ///                            // (cannot borrow `store` as mutable because it is also borrowed as immutable)
    /// }
    /// ```
    /// However function, like rest of [Store], is thread safe.
    ///
    /// # Cancelation safety
    /// Function is cancel safe in meaning store state won't be corrupted after cancelation.
    /// If function is canceled, store might execute transaction in background.
    pub async fn commit<Q, R>(&mut self, tx_query: Q) -> StoreResult<R>
    where
        Q: Tx<D, R> + Into<T> + From<T>,
        S: Send + Sync + 'static,
        T: Send + Sync + 'static,
        R: Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        let wrapped_tx: T = tx_query.into();
        let serialized: Vec<u8> = inner
            .serializer
            .serialize(&wrapped_tx)
            .map_err(|err| StoreError::EncodeJournalEntry(err.into()))?;
        let result = tokio::spawn(async move {
            let mut persistent_holder = inner.persistent.write().await;
            let mut persistent_data = match persistent_holder.take() {
                Some(persistent_data) => persistent_data,
                None => {
                    let reloaded_data = inner.load_persistent_data().await?;
                    reloaded_data
                }
            };

            let journal_file = persistent_data
                .writable_journal(
                    inner.options.max_journal_entries,
                    &inner.serializer,
                    &inner.dir,
                )
                .await?;

            journal_file
                .append(&serialized)
                .await
                .map_err(StoreError::JournalIO)?;
            if let JournalFlushPolicy::EveryCommit {
                journal_flush_method,
                ..
            } = inner.options.journal_flush_policy
            {
                journal_file
                    .persist(journal_flush_method)
                    .await
                    .map_err(StoreError::JournalIO)?;
            }
            let tx_query: Q = wrapped_tx.into();
            let result = tx_query.execute(&mut persistent_data.data);

            *persistent_holder = Some(persistent_data);
            Ok::<R, StoreError>(result)
        })
        .await
        .map_err(StoreError::JoinError)??;
        Ok(result)
    }

    /// Returns immutable, read only store data.
    /// While [QueryGuard] is not dropped, any store updates are locked.
    pub async fn query(&self) -> StoreResult<QueryGuard<'_, D>> {
        let mut persistent_data = self.inner.persistent.read().await;
        if persistent_data.is_none() {
            drop(persistent_data);
            let mut writable_persistent_data = self.inner.persistent.write().await;
            if writable_persistent_data.is_none() {
                *writable_persistent_data = Some(self.inner.load_persistent_data().await?);
            }
            persistent_data = writable_persistent_data.downgrade();
        }
        Ok(QueryGuard(RwLockReadGuard::map(persistent_data, |p| {
            p.as_ref().unwrap()
        })))
    }

    /// Flushes buffered transactions to file. This method is faster than [Store::flush_and_sync], but
    /// less durable (in case of system crash you have higher chance of losing data).
    /// Use [Store::flush_and_sync] for stricter durability.
    pub async fn flush(&mut self) -> StoreResult<()> {
        let mut persistent = self.inner.persistent.write().await;
        match &mut *persistent {
            Some(persistent) => persistent
                .journal
                .flush()
                .await
                .map_err(StoreError::JournalIO),
            None => Err(StoreError::StatePoisoned),
        }
    }

    /// Flushes buffered transactions to file and synces it using [File::sync_data].
    /// This method is way slower than [Store::flush], but is more durable.
    pub async fn flush_and_sync(&mut self) -> StoreResult<()> {
        let mut persistent = self.inner.persistent.write().await;
        match &mut *persistent {
            Some(persistent) => persistent
                .journal
                .flush_and_sync()
                .await
                .map_err(StoreError::JournalIO),
            None => Err(StoreError::StatePoisoned),
        }
    }
}

struct StoreInner<D, T, S> {
    persistent: SharedPersistentData<D>,
    serializer: S,
    options: StoreOptions,
    dir: PathBuf,
    _flusher_guard: Option<FlusherGuard>,
    _phantom: PhantomData<T>,
}

impl<T, D, S> StoreInner<D, T, S>
where
    D: Default + Send + Sync + 'static,
    S: Serializer<D> + Serializer<T>,
    T: Tx<D>,
{
    pub async fn open(
        serializer: S,
        options: StoreOptions,
        dir: impl Into<PathBuf>,
    ) -> StoreResult<Self> {
        let dir: PathBuf = dir.into();
        let persistent_data = PersistentData::load::<T, S>(
            &dir,
            options.can_flush_synchronously_on_drop(),
            &serializer,
        )
        .await?;
        let shared_persistent_data = Arc::new(RwLock::new(Some(persistent_data)));
        let _flusher_guard = match options.journal_flush_policy {
            JournalFlushPolicy::EveryCommit { .. } | JournalFlushPolicy::Manually => None,
            JournalFlushPolicy::Every {
                duration,
                journal_flush_method,
                ..
            } => Some(Self::start_flusher(
                duration,
                journal_flush_method,
                Arc::clone(&shared_persistent_data),
            )),
        };
        Ok(Self {
            persistent: shared_persistent_data,
            serializer,
            options,
            dir,
            _flusher_guard,
            _phantom: PhantomData,
        })
    }

    async fn load_persistent_data(&self) -> StoreResult<PersistentData<D>> {
        PersistentData::load::<T, S>(
            &self.dir,
            self.options.can_flush_synchronously_on_drop(),
            &self.serializer,
        )
        .await
    }

    fn start_flusher(
        interval: Duration,
        journal_flush_method: JournalFlushMethod,
        persistent: SharedPersistentData<D>,
    ) -> FlusherGuard
    where
        D: Sync + Send + 'static,
        S: Serializer<T>,
    {
        FlusherGuard(tokio::spawn(async move {
            loop {
                sleep(interval).await;
                let mut persistent = persistent.write().await;
                if let Some(persistent) = &mut *persistent {
                    if let Err(err) = persistent.journal.persist(journal_flush_method).await {
                        eprintln!("Could not flush journal log: {err:?}");
                    }
                }
            }
        }))
    }
}

#[derive(Debug, Clone)]
pub struct StoreOptions {
    max_journal_entries: NonZeroUsize,
    journal_flush_policy: JournalFlushPolicy,
    flush_synchronously_on_drop: bool,
}

impl StoreOptions {
    /// Maximal amount of persisted transactions in journal file before a
    /// snapshot is created and a new journal file.
    pub fn max_journal_entries(mut self, value: NonZeroUsize) -> Self {
        self.max_journal_entries = value;
        self
    }

    /// Dictates [Store] how often transactions should be saved to [JournalFile].
    pub fn journal_flush_policy(mut self, value: JournalFlushPolicy) -> Self {
        self.journal_flush_policy = value;
        self
    }

    /// Should [Store] synchronously [Store::flush_and_sync] on [Store::drop]?
    /// NO-OP when using [Self::journal_flush_policy] with value [JournalFlushPolicy::EveryCommit].
    pub fn flush_synchronously_on_drop(mut self, value: bool) -> Self {
        self.flush_synchronously_on_drop = value;
        self
    }

    fn can_flush_synchronously_on_drop(&self) -> bool {
        self.flush_synchronously_on_drop
            && !matches!(
                self.journal_flush_policy,
                JournalFlushPolicy::EveryCommit {
                    journal_flush_method: JournalFlushMethod::FlushAndSync
                }
            )
    }
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            max_journal_entries: NonZeroUsize::new(65535).unwrap(),
            journal_flush_policy: JournalFlushPolicy::EveryCommit {
                journal_flush_method: JournalFlushMethod::FlushAndSync,
            },
            flush_synchronously_on_drop: true,
        }
    }
}

/// Dictates [Store] how often transactions should be saved to [JournalFile].
#[derive(Debug, Clone, Copy)]
pub enum JournalFlushPolicy {
    /// Slowest, but safest. Immediately saves transactions to file on [Store::commit].
    EveryCommit {
        /// Decides how [Store] should flush transactions to file.
        /// Read [JournalFlushMethod] for more info.
        journal_flush_method: JournalFlushMethod,
    },
    /// [Store] will own background task that will attempt to flush transactions every defined [Duration].
    /// Transactions commited between flushes might be lost in case of crash.
    /// Any flush errors are printed to stderr using [eprintln!] so not really
    /// recommended for other things than cache that can be lost on restart.
    Every {
        duration: Duration,
        journal_flush_method: JournalFlushMethod,
    },
    /// You are responsible for flushing data to file using [Store::flush_and_sync].
    Manually,
}

#[derive(Debug, Clone, Copy)]
pub enum JournalFlushMethod {
    /// Flushes and syncs data to file. Slowest, but most durable.
    /// Sync means fdatasync on Unix systems - read more in [man page](https://man7.org/linux/man-pages/man2/fsync.2.html).
    FlushAndSync,
    /// Flushes data to file. Faster, but less durable in case of system crash.
    Flush,
}

struct FlusherGuard(JoinHandle<Infallible>);

impl Drop for FlusherGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

type SharedPersistentData<D> = Arc<RwLock<Option<PersistentData<D>>>>;

struct PersistentData<D> {
    data: D,
    next_snapshot_version: SnapshotVersion,
    journal: JournalFile,
    flush_on_drop: bool,
}

impl<D> PersistentData<D> {
    pub async fn load<T, S>(dir: &Path, flush_on_drop: bool, serializer: &S) -> StoreResult<Self>
    where
        T: Tx<D>,
        S: Serializer<D> + Serializer<T>,
        D: Default,
    {
        fs::create_dir_all(dir).await.map_err(StoreError::FileIO)?;
        let persistence_actions = PersistenceAction::rebuild(dir).await?;
        let next_snapshot_version: SnapshotVersion = persistence_actions
            .last()
            .map(|action| match action {
                PersistenceAction::Snapshot { version, .. } => *version + 1,
                PersistenceAction::Journal { version, .. } => *version,
            })
            .unwrap_or_default();
        let mut persistent = Self {
            data: Default::default(),
            journal: JournalFile::open(dir, next_snapshot_version).await?,
            next_snapshot_version,
            flush_on_drop,
        };
        persistent
            .rebuild::<T, S>(&serializer, persistence_actions)
            .await?;
        Ok(persistent)
    }

    async fn rebuild<T, S>(
        &mut self,
        serializer: &S,
        persistence_actions: Vec<PersistenceAction>,
    ) -> StoreResult<()>
    where
        T: Tx<D>,
        S: Serializer<D> + Serializer<T>,
    {
        for action in persistence_actions {
            match action {
                PersistenceAction::Snapshot { path, .. } => {
                    let file = fs::read(path).await.map_err(StoreError::SnapshotIO)?;
                    self.data = serializer
                        .deserialize(&file[..])
                        .map_err(|err| StoreError::DecodeSnapshot(Box::new(err)))?
                        .ok_or(StoreError::DecodeSnapshot(Box::new(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "corrupted snapshot",
                        ))))?;
                }
                PersistenceAction::Journal { path, .. } => {
                    let mut file = File::open(path).await.map_err(StoreError::JournalIO)?;
                    for tx in JournalFile::parse::<T, S>(serializer, &mut file).await? {
                        tx.execute(&mut self.data);
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
        dir: &Path,
    ) -> StoreResult<&mut JournalFile>
    where
        S: Serializer<D>,
    {
        if self.journal.written_entries >= max_entries.into() {
            self.create_new_journal(serializer, dir).await?;
        }
        Ok(&mut self.journal)
    }

    async fn create_new_journal<S>(&mut self, serializer: &S, dir: &Path) -> StoreResult<()>
    where
        S: Serializer<D>,
    {
        self.journal
            .flush_and_sync()
            .await
            .map_err(StoreError::JournalIO)?;
        self.snapshot(serializer, dir).await?;
        self.journal = JournalFile::open(dir, self.next_snapshot_version).await?;
        Ok(())
    }

    /// Writes fully serialized data to snapshot file.
    /// From now, in case of recovery, it has priority over journal file with same [SnapshotVersion].
    async fn snapshot<S>(&mut self, serializer: &S, dir: &Path) -> StoreResult<()>
    where
        S: Serializer<D>,
    {
        let serialized = serializer
            .serialize(&self.data)
            .map_err(|err| StoreError::EncodeSnapshot(Box::new(err)))?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(dir.join(format!("{:0>10}.snapshot", self.next_snapshot_version)))
            .await
            .map_err(StoreError::SnapshotIO)?;
        file.write_all(&serialized)
            .await
            .map_err(StoreError::SnapshotIO)?;
        file.sync_data().await.map_err(StoreError::SnapshotIO)?;
        self.next_snapshot_version += 1;
        Ok(())
    }
}

impl<T> Drop for PersistentData<T> {
    fn drop(&mut self) {
        if self.flush_on_drop {
            futures::executor::block_on(async move {
                if let Err(err) = self.journal.flush_and_sync().await {
                    eprintln!("Could not flush journal log on drop: {err:?}");
                };
            });
        }
    }
}

pub struct QueryGuard<'a, T>(RwLockReadGuard<'a, PersistentData<T>>);

impl<'a, T> Deref for QueryGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0.data
    }
}

type SnapshotVersion = u32;

#[derive(Debug)]
struct JournalFile {
    writer: BufWriter<File>,
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
            writer: BufWriter::new(file),
            written_entries: 0,
        })
    }

    async fn append(&mut self, transaction: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(transaction).await?;
        self.written_entries += 1;
        Ok(())
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush().await
    }

    async fn flush_and_sync(&mut self) -> std::io::Result<()> {
        self.flush().await?;
        self.writer.get_mut().sync_data().await
    }

    async fn persist(&mut self, flush_method: JournalFlushMethod) -> std::io::Result<()> {
        match flush_method {
            JournalFlushMethod::FlushAndSync => {
                self.flush().await?;
                self.writer.get_mut().sync_data().await
            }
            JournalFlushMethod::Flush => self.flush().await,
        }
    }

    async fn parse<T, S>(serializer: &S, file: &mut File) -> StoreResult<Vec<T>>
    where
        S: Serializer<T>,
    {
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .await
            .map_err(StoreError::JournalIO)?;

        let mut cursor = &buf[..];
        let mut transactions: Vec<T> = Vec::new();
        while let Some(entry) = serializer.deserialize(&mut cursor).transpose() {
            let tx = entry.map_err(|err| StoreError::DecodeJournalEntry(err.into()))?;
            transactions.push(tx);
        }
        Ok(transactions)
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

pub trait Serializer<T> {
    type Error: std::error::Error + Send + Sync + 'static;

    fn serialize(&self, transaction: &T) -> Result<Vec<u8>, Self::Error>;

    fn deserialize<R>(&self, reader: R) -> Result<Option<T>, Self::Error>
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
            Err(err) if err.is_eof() && err.column() == 0 => Ok(None),
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

    MergeTx!(CounterTx<Counter> = Increase | IncreaseBy | DecreaseBy);

    #[derive(Serialize, Deserialize)]
    struct Increase;

    #[derive(Serialize, Deserialize)]
    struct IncreaseBy {
        by: usize,
    }

    #[derive(Serialize, Deserialize)]
    struct DecreaseBy {
        by: usize,
    }

    impl Tx<Counter> for Increase {
        fn execute(self, data: &mut Counter) {
            data.value += 1;
        }
    }

    impl Tx<Counter> for IncreaseBy {
        fn execute(self, data: &mut Counter) {
            data.value += data.value;
        }
    }

    impl Tx<Counter> for DecreaseBy {
        fn execute(self, data: &mut Counter) {
            data.value -= data.value;
        }
    }

    #[tokio::test]
    async fn test_journal_chunking() {
        let dir = tempdir().unwrap();
        let options = StoreOptions::default().max_journal_entries(NonZeroUsize::new(2).unwrap());
        let mut store: JsonStore<Counter, CounterTx> =
            Store::open(JsonSerializer, options, dir.path())
                .await
                .unwrap();
        let first_ver = get_snapshot_version(&store).await;
        store.commit(IncreaseBy { by: 2115 }).await.unwrap();
        store.commit(Increase).await.unwrap();
        assert_eq!(get_snapshot_version(&store).await, first_ver);
        store.commit(Increase).await.unwrap();
        assert_eq!(get_snapshot_version(&store).await, first_ver + 1);
    }

    #[tokio::test]
    async fn test_retake_unfulfilled_journal_on_recovery() {
        let dir = tempdir().unwrap();
        let options = StoreOptions::default().max_journal_entries(NonZeroUsize::new(10).unwrap());
        let first_ver = {
            let mut store: JsonStore<Counter, CounterTx> =
                Store::open(JsonSerializer, options.clone(), dir.path())
                    .await
                    .unwrap();
            store.commit(Increase).await.unwrap();
            get_snapshot_version(&store).await
        };

        let mut store: JsonStore<Counter, CounterTx> =
            Store::open(JsonSerializer, options, dir.path())
                .await
                .unwrap();
        store.commit(Increase).await.unwrap();
        assert_eq!(get_snapshot_version(&store).await, first_ver);
    }

    async fn get_snapshot_version(store: &JsonStore<Counter, CounterTx>) -> u32 {
        store
            .inner
            .persistent
            .read()
            .await
            .as_ref()
            .unwrap()
            .next_snapshot_version
    }
}

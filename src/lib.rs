use error::{StoreError, StoreResult};
use std::ffi::OsStr;
use std::path::Path;
use std::path::PathBuf;
use tokio::fs::{self, File};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub mod error;

pub type JsonStore<D> = Store<D, JsonSerializer>;

pub struct Store<T, S>
where
    T: State,
{
    data: T,
    serializer: S,
    journal_file: JournalFile,
}

impl<T, S, C> Store<T, S>
where
    T: State<Command = C>,
{
    pub async fn open(serializer: S, dir: impl AsRef<Path>) -> StoreResult<Self>
    where
        S: Serializer<T> + Serializer<C>,
    {
        fs::create_dir_all(&dir).await.map_err(StoreError::FileIO)?;
        let old_journals = {
            let mut it = JournalFile::read_dir(&dir).await?;
            it.sort_by_key(|(id, _)| *id);
            it
        };
        let latest_journal_id: JournalId =
            old_journals.last().map(|(id, _)| *id).unwrap_or_default();

        let mut store = Self {
            data: Default::default(),
            serializer,
            journal_file: JournalFile::open(&dir, latest_journal_id + 1).await?,
        };
        store.rebuild(&old_journals).await?;
        Ok(store)
    }

    pub async fn commit<'a>(&mut self, command: C) -> StoreResult<()>
    where
        S: Serializer<C>,
    {
        let serialized: Vec<u8> = self
            .serializer
            .serialize(&command)
            .map_err(|err| StoreError::EncodeJournalEntry(err.into()))?;
        self.data.execute(command);
        self.journal_file.append(&serialized).await?;
        Ok(())
    }

    pub fn query(&self) -> &T {
        &self.data
    }

    async fn rebuild(&mut self, journals: &Vec<(JournalId, PathBuf)>) -> StoreResult<()>
    where
        S: Serializer<C>,
    {
        for (_, journal) in journals {
            let mut journal_file = File::open(journal).await.map_err(StoreError::JournalIO)?;
            for command in JournalFile::parse(&self.serializer, &mut journal_file).await? {
                self.data.execute(command);
            }
        }
        Ok(())
    }
}

type JournalId = u32;

struct JournalFile {
    file: File,
}

impl JournalFile {
    pub async fn open(dir: impl AsRef<Path>, id: JournalId) -> StoreResult<Self> {
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.as_ref().join(format!("{:0>10}.journal", id)))
            .await
            .map_err(StoreError::JournalIO)?;
        Ok(Self { file })
    }

    async fn append(&mut self, data: &[u8]) -> StoreResult<()> {
        self.file
            .write_all(data)
            .await
            .map_err(StoreError::JournalIO)?;
        self.file.sync_all().await.map_err(StoreError::JournalIO)?;
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

    fn deserialize<'a, R>(&self, reader: R) -> Result<Option<C>, Self::Error>
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
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }
}

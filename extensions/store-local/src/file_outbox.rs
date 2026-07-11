//! Crash-recoverable local outbox backed by an append-only journal.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use aether_domain::TimestampMs;
use aether_ports::{
    DurableOutbox, OutboxEntry, OutboxId, OutboxMessage, PortError, PortErrorKind, PortResult,
};
use async_trait::async_trait;
use fs2::FileExt;
use tokio::sync::oneshot;

const FILE_MAGIC: &[u8; 8] = b"AETHOBX1";
const FILE_VERSION: u32 = 1;
const FILE_HEADER_LEN: usize = 16;
const RECORD_MAGIC: u32 = 0x5842_4F41;
const RECORD_HEADER_LEN: usize = 12;
const MAX_RECORD_LEN: usize = 16 * 1024 * 1024;
const MAX_CAPACITY: usize = (MAX_RECORD_LEN - 5) / std::mem::size_of::<u64>();
const REQUEST_QUEUE_CAPACITY: usize = 256;

const OP_ENQUEUE: u8 = 1;
const OP_ACKNOWLEDGE: u8 = 2;
const OP_CHECKPOINT: u8 = 3;

/// A bounded, crash-recoverable outbox that requires no external service.
///
/// Operations are serialized on a dedicated worker thread. An enqueue or
/// acknowledgement is reported as successful only after its journal record
/// has been synchronized to disk. The journal path is exclusively locked for
/// the lifetime of this value, including across cloned handles.
#[derive(Clone)]
pub struct FileOutbox {
    worker: Arc<Worker>,
}

impl std::fmt::Debug for FileOutbox {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileOutbox")
            .field("path", &self.worker.path)
            .finish_non_exhaustive()
    }
}

impl FileOutbox {
    /// Opens or creates a journal that stores at most `capacity` live entries.
    ///
    /// Opening performs synchronous recovery and should normally happen during
    /// gateway startup, before latency-sensitive tasks are launched.
    pub fn open(path: impl AsRef<Path>, capacity: usize) -> PortResult<Self> {
        if capacity == 0 {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "file outbox capacity must be greater than zero",
            ));
        }
        if capacity > MAX_CAPACITY {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                format!("file outbox capacity exceeds maximum {MAX_CAPACITY}"),
            ));
        }

        let path = path.as_ref().to_path_buf();
        let journal = Journal::open(path.clone(), capacity)?;
        let (sender, receiver) = sync_channel(REQUEST_QUEUE_CAPACITY);
        let handle = thread::Builder::new()
            .name("aether-file-outbox".to_string())
            .spawn(move || run_worker(journal, receiver))
            .map_err(|error| io_error("spawn file outbox worker", error))?;

        Ok(Self {
            worker: Arc::new(Worker {
                path,
                sender,
                join: Mutex::new(Some(handle)),
            }),
        })
    }

    /// Atomically rewrites the journal with only live entries.
    ///
    /// The checkpoint also persists the next identifier, so compaction never
    /// permits an acknowledged identifier to be reused after restart.
    pub async fn compact(&self) -> PortResult<()> {
        let (reply, response) = oneshot::channel();
        self.submit(Request::Compact { reply })?;
        await_response(response).await
    }

    fn submit(&self, request: Request) -> PortResult<()> {
        self.worker
            .sender
            .try_send(request)
            .map_err(|error| match error {
                TrySendError::Full(_) => PortError::new(
                    PortErrorKind::Unavailable,
                    "file outbox worker queue is full",
                ),
                TrySendError::Disconnected(_) => PortError::new(
                    PortErrorKind::Permanent,
                    "file outbox worker stopped unexpectedly",
                ),
            })
    }
}

#[async_trait]
impl DurableOutbox for FileOutbox {
    async fn enqueue(&self, message: OutboxMessage) -> PortResult<OutboxId> {
        let (reply, response) = oneshot::channel();
        self.submit(Request::Enqueue { message, reply })?;
        await_response(response).await
    }

    async fn peek(&self, limit: usize) -> PortResult<Vec<OutboxEntry>> {
        let (reply, response) = oneshot::channel();
        self.submit(Request::Peek { limit, reply })?;
        await_response(response).await
    }

    async fn acknowledge(&self, ids: &[OutboxId]) -> PortResult<usize> {
        let (reply, response) = oneshot::channel();
        self.submit(Request::Acknowledge {
            ids: ids.to_vec(),
            reply,
        })?;
        await_response(response).await
    }
}

async fn await_response<T>(response: oneshot::Receiver<PortResult<T>>) -> PortResult<T> {
    response.await.map_err(|_| {
        PortError::new(
            PortErrorKind::Permanent,
            "file outbox worker dropped a response",
        )
    })?
}

struct Worker {
    path: PathBuf,
    sender: SyncSender<Request>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        // A blocking send is intentional during final ownership release: it
        // drains already-accepted requests before the worker releases the file
        // lock. No async lock is held here.
        let _ = self.sender.send(Request::Shutdown);
        if let Ok(mut slot) = self.join.lock()
            && let Some(handle) = slot.take()
        {
            let _ = handle.join();
        }
    }
}

enum Request {
    Enqueue {
        message: OutboxMessage,
        reply: oneshot::Sender<PortResult<OutboxId>>,
    },
    Peek {
        limit: usize,
        reply: oneshot::Sender<PortResult<Vec<OutboxEntry>>>,
    },
    Acknowledge {
        ids: Vec<OutboxId>,
        reply: oneshot::Sender<PortResult<usize>>,
    },
    Compact {
        reply: oneshot::Sender<PortResult<()>>,
    },
    Shutdown,
}

fn run_worker(mut journal: Journal, receiver: Receiver<Request>) {
    while let Ok(request) = receiver.recv() {
        match request {
            Request::Enqueue { message, reply } => {
                let _ = reply.send(journal.enqueue(message));
            },
            Request::Peek { limit, reply } => {
                let _ = reply.send(journal.peek(limit));
            },
            Request::Acknowledge { ids, reply } => {
                let _ = reply.send(journal.acknowledge(&ids));
            },
            Request::Compact { reply } => {
                let _ = reply.send(journal.compact());
            },
            Request::Shutdown => break,
        }
    }
}

struct Journal {
    path: PathBuf,
    capacity: usize,
    file: File,
    _lock_file: File,
    next_id: u64,
    entries: BTreeMap<OutboxId, OutboxEntry>,
    poisoned: Option<String>,
}

impl Journal {
    fn open(path: PathBuf, capacity: usize) -> PortResult<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| io_error("create outbox directory", error))?;
        }

        let lock_path = sibling_path(&path, ".lock");
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| io_error("open outbox lock file", error))?;
        FileExt::try_lock_exclusive(&lock_file).map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                PortError::new(
                    PortErrorKind::Conflict,
                    format!("outbox journal is already open: {}", path.display()),
                )
            } else {
                io_error("lock outbox journal", error)
            }
        })?;

        let stale_compaction = sibling_path(&path, ".compact.tmp");
        match std::fs::remove_file(&stale_compaction) {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(io_error("remove stale outbox compaction", error)),
        }

        let mut file = open_journal_file(&path)?;
        let recovered = recover(&mut file)?;
        file.seek(SeekFrom::End(0))
            .map_err(|error| io_error("seek recovered outbox journal", error))?;

        Ok(Self {
            path,
            capacity,
            file,
            _lock_file: lock_file,
            next_id: recovered.next_id,
            entries: recovered.entries,
            poisoned: None,
        })
    }

    fn enqueue(&mut self, message: OutboxMessage) -> PortResult<OutboxId> {
        self.ensure_healthy()?;
        if self.entries.len() >= self.capacity {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                format!("file outbox capacity {} reached", self.capacity),
            ));
        }

        let id = OutboxId::new(self.next_id);
        let next_id = self.next_id.checked_add(1).ok_or_else(|| {
            PortError::new(PortErrorKind::Permanent, "outbox identifier exhausted")
        })?;
        let entry = OutboxEntry::new(id, message, 0);
        let payload = encode_enqueue(&entry)?;
        append_record(&mut self.file, &payload)?;

        self.entries.insert(id, entry);
        self.next_id = next_id;
        Ok(id)
    }

    fn peek(&self, limit: usize) -> PortResult<Vec<OutboxEntry>> {
        self.ensure_healthy()?;
        Ok(self.entries.values().take(limit).cloned().collect())
    }

    fn acknowledge(&mut self, ids: &[OutboxId]) -> PortResult<usize> {
        self.ensure_healthy()?;
        let existing = ids
            .iter()
            .copied()
            .filter(|id| self.entries.contains_key(id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if existing.is_empty() {
            return Ok(0);
        }

        let payload = encode_acknowledge(&existing)?;
        append_record(&mut self.file, &payload)?;
        for id in &existing {
            self.entries.remove(id);
        }
        Ok(existing.len())
    }

    fn compact(&mut self) -> PortResult<()> {
        self.ensure_healthy()?;
        let temp_path = sibling_path(&self.path, ".compact.tmp");
        let mut temp = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&temp_path)
            .map_err(|error| io_error("create outbox compaction file", error))?;
        write_file_header(&mut temp)?;
        write_record(&mut temp, &encode_checkpoint(self.next_id))?;
        for entry in self.entries.values() {
            write_record(&mut temp, &encode_enqueue(entry)?)?;
        }
        temp.sync_all()
            .map_err(|error| io_error("sync outbox compaction file", error))?;

        std::fs::rename(&temp_path, &self.path)
            .map_err(|error| io_error("commit outbox compaction", error))?;

        let replacement = open_journal_file(&self.path).and_then(|mut replacement| {
            replacement
                .seek(SeekFrom::End(0))
                .map_err(|error| io_error("seek compacted outbox journal", error))?;
            Ok(replacement)
        });
        match replacement {
            Ok(file) => self.file = file,
            Err(error) => {
                self.poisoned = Some(error.to_string());
                return Err(error);
            },
        }

        sync_parent_directory(&self.path)
    }

    fn ensure_healthy(&self) -> PortResult<()> {
        if let Some(reason) = &self.poisoned {
            return Err(PortError::new(
                PortErrorKind::Permanent,
                format!("file outbox is unavailable after a fatal journal error: {reason}"),
            ));
        }
        Ok(())
    }
}

struct Recovered {
    next_id: u64,
    entries: BTreeMap<OutboxId, OutboxEntry>,
}

fn recover(file: &mut File) -> PortResult<Recovered> {
    let file_len = file
        .metadata()
        .map_err(|error| io_error("stat outbox journal", error))?
        .len();
    if file_len == 0 {
        write_file_header(file)?;
        return Ok(Recovered {
            next_id: 1,
            entries: BTreeMap::new(),
        });
    }
    if file_len < FILE_HEADER_LEN as u64 {
        return Err(corrupt("outbox journal has an incomplete file header"));
    }

    file.seek(SeekFrom::Start(0))
        .map_err(|error| io_error("seek outbox journal header", error))?;
    let mut file_header = [0_u8; FILE_HEADER_LEN];
    file.read_exact(&mut file_header)
        .map_err(|error| io_error("read outbox journal header", error))?;
    if &file_header[..8] != FILE_MAGIC {
        return Err(corrupt("outbox journal magic does not match"));
    }
    let version = u32::from_le_bytes(
        file_header[8..12]
            .try_into()
            .map_err(|_| corrupt("outbox journal version is malformed"))?,
    );
    if version != FILE_VERSION {
        return Err(corrupt(format!(
            "unsupported outbox journal version {version}"
        )));
    }

    let mut recovered = Recovered {
        next_id: 1,
        entries: BTreeMap::new(),
    };
    let mut offset = FILE_HEADER_LEN as u64;
    while offset < file_len {
        let remaining = file_len - offset;
        if remaining < RECORD_HEADER_LEN as u64 {
            truncate_tail(file, offset)?;
            break;
        }

        file.seek(SeekFrom::Start(offset))
            .map_err(|error| io_error("seek outbox record", error))?;
        let mut record_header = [0_u8; RECORD_HEADER_LEN];
        file.read_exact(&mut record_header)
            .map_err(|error| io_error("read outbox record header", error))?;
        let magic = u32::from_le_bytes(
            record_header[..4]
                .try_into()
                .map_err(|_| corrupt("outbox record magic is malformed"))?,
        );
        if magic != RECORD_MAGIC {
            return Err(corrupt(format!(
                "outbox record at offset {offset} has invalid magic"
            )));
        }
        let payload_len = u32::from_le_bytes(
            record_header[4..8]
                .try_into()
                .map_err(|_| corrupt("outbox record length is malformed"))?,
        ) as usize;
        if payload_len == 0 || payload_len > MAX_RECORD_LEN {
            return Err(corrupt(format!(
                "outbox record at offset {offset} has invalid length {payload_len}"
            )));
        }
        let expected_checksum = u32::from_le_bytes(
            record_header[8..12]
                .try_into()
                .map_err(|_| corrupt("outbox record checksum is malformed"))?,
        );
        let record_end = offset
            .checked_add(RECORD_HEADER_LEN as u64)
            .and_then(|value| value.checked_add(payload_len as u64))
            .ok_or_else(|| corrupt("outbox record offset overflow"))?;
        if record_end > file_len {
            truncate_tail(file, offset)?;
            break;
        }

        let mut payload = vec![0_u8; payload_len];
        file.read_exact(&mut payload)
            .map_err(|error| io_error("read outbox record payload", error))?;
        if checksum(&payload) != expected_checksum {
            if record_end == file_len {
                truncate_tail(file, offset)?;
                break;
            }
            return Err(corrupt(format!(
                "outbox record at offset {offset} failed checksum"
            )));
        }
        apply_record(&payload, &mut recovered)?;
        offset = record_end;
    }

    Ok(recovered)
}

fn apply_record(payload: &[u8], recovered: &mut Recovered) -> PortResult<()> {
    let mut cursor = ByteCursor::new(payload);
    match cursor.read_u8()? {
        OP_ENQUEUE => {
            let id = OutboxId::new(cursor.read_u64()?);
            let created_at = TimestampMs::new(cursor.read_u64()?);
            let attempts = cursor.read_u32()?;
            let destination_len = cursor.read_u32()? as usize;
            let payload_len = cursor.read_u32()? as usize;
            let destination = String::from_utf8(cursor.read_bytes(destination_len)?.to_vec())
                .map_err(|_| corrupt("outbox destination is not UTF-8"))?;
            let message_payload = cursor.read_bytes(payload_len)?.to_vec();
            cursor.finish()?;

            if recovered.entries.contains_key(&id) {
                return Err(corrupt(format!(
                    "duplicate outbox enqueue identifier {}",
                    id.get()
                )));
            }
            let next_id = id
                .get()
                .checked_add(1)
                .ok_or_else(|| corrupt("outbox identifier exhausted in journal"))?;
            recovered.next_id = recovered.next_id.max(next_id);
            recovered.entries.insert(
                id,
                OutboxEntry::new(
                    id,
                    OutboxMessage::new(destination, message_payload, created_at),
                    attempts,
                ),
            );
        },
        OP_ACKNOWLEDGE => {
            let count = cursor.read_u32()? as usize;
            for _ in 0..count {
                recovered.entries.remove(&OutboxId::new(cursor.read_u64()?));
            }
            cursor.finish()?;
        },
        OP_CHECKPOINT => {
            let next_id = cursor.read_u64()?;
            cursor.finish()?;
            if next_id == 0 {
                return Err(corrupt("outbox checkpoint contains identifier zero"));
            }
            recovered.next_id = recovered.next_id.max(next_id);
        },
        operation => {
            return Err(corrupt(format!(
                "unknown outbox journal operation {operation}"
            )));
        },
    }
    Ok(())
}

fn encode_enqueue(entry: &OutboxEntry) -> PortResult<Vec<u8>> {
    let destination = entry.message().destination().as_bytes();
    let message_payload = entry.message().payload();
    let destination_len =
        u32::try_from(destination.len()).map_err(|_| corrupt("outbox destination is too large"))?;
    let payload_len =
        u32::try_from(message_payload.len()).map_err(|_| corrupt("outbox payload is too large"))?;

    let mut bytes = Vec::with_capacity(29 + destination.len() + message_payload.len());
    bytes.push(OP_ENQUEUE);
    bytes.extend_from_slice(&entry.id().get().to_le_bytes());
    bytes.extend_from_slice(&entry.message().created_at().get().to_le_bytes());
    bytes.extend_from_slice(&entry.attempts().to_le_bytes());
    bytes.extend_from_slice(&destination_len.to_le_bytes());
    bytes.extend_from_slice(&payload_len.to_le_bytes());
    bytes.extend_from_slice(destination);
    bytes.extend_from_slice(message_payload);
    validate_record_size(&bytes)?;
    Ok(bytes)
}

fn encode_acknowledge(ids: &[OutboxId]) -> PortResult<Vec<u8>> {
    let count =
        u32::try_from(ids.len()).map_err(|_| corrupt("too many outbox acknowledgements"))?;
    let mut bytes = Vec::with_capacity(5 + ids.len() * std::mem::size_of::<u64>());
    bytes.push(OP_ACKNOWLEDGE);
    bytes.extend_from_slice(&count.to_le_bytes());
    for id in ids {
        bytes.extend_from_slice(&id.get().to_le_bytes());
    }
    validate_record_size(&bytes)?;
    Ok(bytes)
}

fn encode_checkpoint(next_id: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(9);
    bytes.push(OP_CHECKPOINT);
    bytes.extend_from_slice(&next_id.to_le_bytes());
    bytes
}

fn validate_record_size(payload: &[u8]) -> PortResult<()> {
    if payload.is_empty() || payload.len() > MAX_RECORD_LEN {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            format!(
                "outbox journal record size {} exceeds maximum {MAX_RECORD_LEN}",
                payload.len()
            ),
        ));
    }
    Ok(())
}

fn append_record(file: &mut File, payload: &[u8]) -> PortResult<()> {
    validate_record_size(payload)?;
    let start = file
        .seek(SeekFrom::End(0))
        .map_err(|error| io_error("seek outbox append position", error))?;
    let result = write_record(file, payload).and_then(|()| {
        file.sync_data()
            .map_err(|error| io_error("sync outbox journal record", error))
    });
    if result.is_err() {
        let _ = file.set_len(start);
        let _ = file.seek(SeekFrom::End(0));
        let _ = file.sync_data();
    }
    result
}

fn write_record(file: &mut File, payload: &[u8]) -> PortResult<()> {
    validate_record_size(payload)?;
    let payload_len = u32::try_from(payload.len())
        .map_err(|_| corrupt("outbox record length cannot be represented"))?;
    let mut header = [0_u8; RECORD_HEADER_LEN];
    header[..4].copy_from_slice(&RECORD_MAGIC.to_le_bytes());
    header[4..8].copy_from_slice(&payload_len.to_le_bytes());
    header[8..12].copy_from_slice(&checksum(payload).to_le_bytes());
    file.write_all(&header)
        .and_then(|()| file.write_all(payload))
        .map_err(|error| io_error("write outbox journal record", error))
}

fn write_file_header(file: &mut File) -> PortResult<()> {
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.set_len(0))
        .map_err(|error| io_error("reset outbox journal", error))?;
    let mut header = [0_u8; FILE_HEADER_LEN];
    header[..8].copy_from_slice(FILE_MAGIC);
    header[8..12].copy_from_slice(&FILE_VERSION.to_le_bytes());
    file.write_all(&header)
        .and_then(|()| file.sync_all())
        .map_err(|error| io_error("initialize outbox journal", error))
}

fn truncate_tail(file: &mut File, valid_len: u64) -> PortResult<()> {
    file.set_len(valid_len)
        .and_then(|()| file.sync_data())
        .and_then(|()| file.seek(SeekFrom::End(0)).map(|_| ()))
        .map_err(|error| io_error("truncate incomplete outbox journal tail", error))
}

fn open_journal_file(path: &Path) -> PortResult<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| io_error("open outbox journal", error))
}

fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let mut file_name = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("outbox"))
        .to_os_string();
    file_name.push(suffix);
    path.with_file_name(file_name)
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> PortResult<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| io_error("sync outbox parent directory", error))
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> PortResult<()> {
    Ok(())
}

fn checksum(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

struct ByteCursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> ByteCursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn read_u8(&mut self) -> PortResult<u8> {
        Ok(self.read_bytes(1)?[0])
    }

    fn read_u32(&mut self) -> PortResult<u32> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| corrupt("outbox u32 field is malformed"))?,
        ))
    }

    fn read_u64(&mut self) -> PortResult<u64> {
        let bytes = self.read_bytes(8)?;
        Ok(u64::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| corrupt("outbox u64 field is malformed"))?,
        ))
    }

    fn read_bytes(&mut self, length: usize) -> PortResult<&'a [u8]> {
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| corrupt("outbox record offset overflow"))?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| corrupt("outbox record ended before all fields were decoded"))?;
        self.position = end;
        Ok(bytes)
    }

    fn finish(self) -> PortResult<()> {
        if self.position != self.bytes.len() {
            return Err(corrupt("outbox record contains trailing bytes"));
        }
        Ok(())
    }
}

fn corrupt(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}

fn io_error(context: &str, error: std::io::Error) -> PortError {
    let kind = match error.kind() {
        std::io::ErrorKind::PermissionDenied
        | std::io::ErrorKind::InvalidInput
        | std::io::ErrorKind::InvalidData
        | std::io::ErrorKind::Unsupported => PortErrorKind::Permanent,
        std::io::ErrorKind::AlreadyExists => PortErrorKind::Conflict,
        _ => PortErrorKind::Unavailable,
    };
    PortError::new(kind, format!("{context}: {error}"))
}

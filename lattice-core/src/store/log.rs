//! Log file I/O for append-only entry storage
//!
//! Each author has a log file containing length-delimited LogRecord messages.
//! LogRecord = { hash: [u8; 32], entry_bytes: SignedEntry }

use crate::proto::storage::{LogRecord, SignedEntry as ProtoSignedEntry};
use crate::entry::{SignedEntry, EntryError};
use crate::MAX_ENTRY_SIZE;
use prost::Message;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;
use thiserror::Error;

/// Errors that can occur during log operations
#[derive(Error, Debug)]
pub enum LogError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    
    #[error("Proto decode error: {0}")]
    Decode(#[from] prost::DecodeError),
    
    #[error("Entry error: {0}")]
    Entry(#[from] EntryError),
    
    #[error("Entry too large: {0} bytes (max {MAX_ENTRY_SIZE})")]
    EntryTooLarge(usize),
    
    #[error("Unexpected EOF while reading entry")]
    UnexpectedEof,
    
    #[error("Hash mismatch: stored hash does not match computed hash")]
    HashMismatch,
}

/// Iterator over entries in a log file with optional sequence range filtering.
pub struct EntryIter {
    reader: Option<LogReader>,
    from_seq: u64,
    to_seq: u64,  // 0 = no upper bound
}

impl EntryIter {
    fn new(reader: LogReader, from_seq: u64, to_seq: u64) -> Self {
        Self {
            reader: Some(reader),
            from_seq,
            to_seq,
        }
    }
}

impl Iterator for EntryIter {
    type Item = Result<SignedEntry, LogError>;
    
    fn next(&mut self) -> Option<Self::Item> {
        let reader = self.reader.as_mut()?;
        
        loop {
            match reader.next()? {
                Ok((_, signed_entry)) => {
                    // Skip range filtering if from_seq=0 (return all entries)
                    if self.from_seq == 0 {
                        return Some(Ok(signed_entry));
                    }
                           
                    // Decode Entry to get sequence (no decode needed for internal type)
                    let seq = signed_entry.entry.seq;
                    
                    if seq < self.from_seq {
                        continue; // Skip entries before range
                    }
                    if self.to_seq > 0 && seq > self.to_seq {
                        return None; // Past end of range
                    }
                    return Some(Ok(signed_entry));
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

/// Iterator over entries in a log file
/// Returns (hash, SignedEntry) pairs
pub struct LogReader {
    reader: BufReader<File>,
}

impl LogReader {
    /// Create a reader from an existing file, seeking to start
    pub fn from_file(mut file: File) -> Result<Self, LogError> {
        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start(0))?;
        Ok(Self {
            reader: BufReader::new(file),
        })
    }
}

impl Iterator for LogReader {
    type Item = Result<([u8; 32], SignedEntry), LogError>;
    
    fn next(&mut self) -> Option<Self::Item> {
        match read_one_record(&mut self.reader) {
            Ok(Some(pair)) => Some(Ok(pair)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

use std::path::PathBuf;

/// A log file with proper file handle ownership.
/// Opened once, kept open for the lifetime of the struct.
pub struct Log {
    path: PathBuf,
    writer: BufWriter<File>,
}

impl Log {
    /// Open an existing log file for reading and appending.
    /// Returns error if file doesn't exist.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LogError> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            path,
            writer: BufWriter::new(file),
        })
    }
    
    /// Open an existing log file, or create new if it doesn't exist.
    pub fn open_or_create(path: impl AsRef<Path>) -> Result<Self, LogError> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)?;
        Ok(Self {
            path,
            writer: BufWriter::new(file),
        })
    }
    
    /// Append a SignedEntry to this log.
    pub fn append(&mut self, entry: &SignedEntry) -> Result<(), LogError> {
        let proto: ProtoSignedEntry = entry.clone().into();
        let entry_bytes = proto.encode_to_vec();
        
        if entry_bytes.len() > MAX_ENTRY_SIZE {
            return Err(LogError::EntryTooLarge(entry_bytes.len()));
        }
        
        // Compute hash
        let hash = entry.hash();
        
        // Create LogRecord
        let record = LogRecord {
            hash: hash.to_vec(),
            entry_bytes,
        };
        
        // Write length-delimited LogRecord
        let mut buf = Vec::new();
        record.encode_length_delimited(&mut buf)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        
        self.writer.write_all(&buf)?;
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        
        Ok(())
    }
    
    /// Iterate all entries in this log.
    /// Uses try_clone() on the underlying file to share the file descriptor.
    pub fn iter(&self) -> Result<EntryIter, LogError> {
        let file = self.writer.get_ref().try_clone()?;
        let reader = LogReader::from_file(file)?;
        Ok(EntryIter::new(reader, 0, 0))
    }
    
    /// Iterate entries in this log within a sequence range.
    /// Uses try_clone() on the underlying file to share the file descriptor.
    pub fn iter_range(&self, from_seq: u64, to_seq: u64) -> Result<EntryIter, LogError> {
        let file = self.writer.get_ref().try_clone()?;
        let reader = LogReader::from_file(file)?;
        Ok(EntryIter::new(reader, from_seq, to_seq))
    }
    
    /// Get the path to this log file.
    pub fn path(&self) -> &Path {
        &self.path
    }
    
    /// Get reference to the underlying writer's file (for testing/cloning)
    #[cfg(test)]
    pub fn writer_file(&self) -> &File {
        self.writer.get_ref()
    }
}

/// Read a single LogRecord, returning (hash, SignedEntry)
fn read_one_record<R: Read>(reader: &mut R) -> Result<Option<([u8; 32], SignedEntry)>, LogError> {
    // Read length-delimited bytes
    let record_bytes = match read_length_delimited_bytes(reader) {
        Ok(bytes) => bytes,
        Err(LogError::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    };
    
    // Decode LogRecord
    let record = LogRecord::decode(&record_bytes[..])?;
    
    // Verify hash
    let computed_hash: [u8; 32] = blake3::hash(&record.entry_bytes).into();
    let stored_hash: [u8; 32] = record.hash.try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid hash length"))?;
    
    if computed_hash != stored_hash {
        return Err(LogError::HashMismatch);
    }
    
    // Decode SignedEntry
    let proto_entry = ProtoSignedEntry::decode(&record.entry_bytes[..])?;
    let entry = SignedEntry::try_from(proto_entry)?;
    Ok(Some((stored_hash, entry)))
}

/// Read length-delimited bytes from a reader
fn read_length_delimited_bytes<R: Read>(reader: &mut R) -> Result<Vec<u8>, LogError> {
    // Read varint length prefix
    let mut prefix_buf = Vec::with_capacity(10);
    let mut byte = [0u8; 1];
    
    loop {
        match reader.read_exact(&mut byte) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(e.into()),
            Err(e) => return Err(e.into()),
        }
        prefix_buf.push(byte[0]);
        
        if byte[0] & 0x80 == 0 {
            break;
        }
        if prefix_buf.len() > 10 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "varint too long").into());
        }
    }
    
    // Decode the length
    let len = prost::decode_length_delimiter(&prefix_buf[..])
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    
    if len > MAX_ENTRY_SIZE {
        return Err(LogError::EntryTooLarge(len));
    }
    
    // Read the data
    let mut data_buf = vec![0u8; len];
    reader.read_exact(&mut data_buf).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            LogError::UnexpectedEof
        } else {
            e.into()
        }
    })?;
    
    Ok(data_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::MockClock;
    use crate::hlc::HLC;
    use crate::node_identity::NodeIdentity;
    use crate::proto::storage::Operation;
    use crate::entry::EntryBuilder;
    
    fn read_entries(path: impl AsRef<std::path::Path>) -> Result<Vec<SignedEntry>, LogError> {
        Log::open(&path)?.iter()?.collect()
    }

    #[test]
    fn test_append_and_read_single() {
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.log");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        let hlc = HLC::now_with_clock(&clock);
        
        let entry = EntryBuilder::new(1, hlc)
            .operation(Operation::put("/test/key", b"value".to_vec()))
            .sign(&node);
        
        Log::open_or_create(&path).unwrap().append(&entry).unwrap();
        
        let entries = read_entries(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry.seq, 1);
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_append_multiple() {
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.log");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        for i in 1..=5 {
            let entry = EntryBuilder::new(i, HLC::now_with_clock(&clock))
                .operation(Operation::put(format!("/key/{}", i), format!("value{}", i).into_bytes()))
                .sign(&node);
            Log::open_or_create(&path).unwrap().append(&entry).unwrap();
        }
        
        let entries = read_entries(&path).unwrap();
        assert_eq!(entries.len(), 5);
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_log_reader_returns_hash() {
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.log");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        let entry = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .operation(Operation::put("/key", b"value".to_vec()))
            .sign(&node);
        let expected_hash = entry.hash();
        let log = Log::open_or_create(&path).unwrap();
        let mut log = log;
        log.append(&entry).unwrap();
        
        // Get LogReader from the Log's file handle
        let file = log.writer_file().try_clone().unwrap();
        let mut reader = LogReader::from_file(file).unwrap();
        let (hash, _) = reader.next().unwrap().unwrap();
        assert_eq!(hash, expected_hash);
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_empty_file() {
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.log");
        std::fs::remove_file(&path).ok();
        
        File::create(&path).unwrap();
        
        let entries = read_entries(&path).unwrap();
        assert_eq!(entries.len(), 0);
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_nonexistent() {
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.log");
        std::fs::remove_file(&path).ok();
        
        // Log::open returns NotFound error for missing files
        let result = Log::open(&path);
        assert!(result.is_err());
        match result {
            Err(LogError::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            _ => panic!("Expected NotFound error"),
        }
    }

    // --- Negative Tests ---

    #[test]
    fn test_corrupted_entry_detected() {
        use std::io::Seek;
        
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.log");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        let entry = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .operation(Operation::put("/key", b"original".to_vec()))
            .sign(&node);
        Log::open_or_create(&path).unwrap().append(&entry).unwrap();
        
        // Corrupt the file: change a byte in the middle
        let mut file = OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(io::SeekFrom::Start(20)).unwrap();
        file.write_all(&[0xFF]).unwrap();
        drop(file);
        
        let result = read_entries(&path);
        
        match result {
            Err(LogError::HashMismatch) => (),
            Err(LogError::Decode(_)) => (),
            Err(e) => panic!("Expected HashMismatch or Decode error, got: {:?}", e),
            Ok(_) => panic!("Corrupted entry was accepted!"),
        }
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_truncated_file() {
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.log");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        let entry = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .operation(Operation::put("/key", b"data".to_vec()))
            .sign(&node);
        Log::open_or_create(&path).unwrap().append(&entry).unwrap();
        
        // Truncate file by 1 byte
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        let len = file.metadata().unwrap().len();
        file.set_len(len - 1).unwrap();
        drop(file);
        
        let result = read_entries(&path);
        
        match result {
            Err(LogError::UnexpectedEof) => (),
            Err(LogError::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof => (),
            Err(LogError::Decode(_)) => (), // Also acceptable
            res => panic!("Expected UnexpectedEof, got: {:?}", res),
        }
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_append_too_large() {
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.log");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        // Create payload larger than MAX_ENTRY_SIZE
        let huge_payload = vec![0u8; crate::MAX_ENTRY_SIZE + 100];
        
        let entry = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .operation(Operation::put("/huge", huge_payload))
            .sign(&node);
        
        let result = Log::open_or_create(&path).unwrap().append(&entry);
        
        match result {
            Err(LogError::EntryTooLarge(size)) => assert!(size > crate::MAX_ENTRY_SIZE),
            _ => panic!("Expected EntryTooLarge error"),
        }
        
    }

    #[test]
    fn test_read_entry_exceeding_limit() {
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.log");
        std::fs::remove_file(&path).ok();
        
        // Write a length prefix claiming the entry is > MAX_ENTRY_SIZE
        let mut file = File::create(&path).unwrap();
        
        let too_big = (crate::MAX_ENTRY_SIZE + 1) as usize;
        let mut buf = Vec::new();
        prost::encode_length_delimiter(too_big, &mut buf).unwrap();
        
        file.write_all(&buf).unwrap();
        // Write some dummy bytes (reader should reject before reading these)
        file.write_all(&[0u8; 10]).unwrap();
        drop(file);
        
        let result = read_entries(&path);
        
        match result {
            Err(LogError::EntryTooLarge(size)) => assert_eq!(size, too_big),
            _ => panic!("Expected EntryTooLarge before allocating RAM"),
        }
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_corruption_in_middle_of_stream() {
        let _tmp = tempfile::tempdir().unwrap(); let path = _tmp.path().join("test.log");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        // Write 3 entries
        for i in 0..3 {
            let entry = EntryBuilder::new(i + 1, HLC::now_with_clock(&clock))
                .operation(Operation::put(format!("/key/{}", i), b"val"))
                .sign(&node);
            Log::open_or_create(&path).unwrap().append(&entry).unwrap();
        }
        
        // Corrupt a byte in the middle of the file
        let mut file_bytes = std::fs::read(&path).unwrap();
        let mid_idx = file_bytes.len() / 2;
        file_bytes[mid_idx] = !file_bytes[mid_idx]; // Bitflip
        std::fs::write(&path, file_bytes).unwrap();
        
        // Reading should fail at some point
        let result = read_entries(&path);
        
        // Must fail with HashMismatch or DecodeError  
        assert!(result.is_err());
        
        std::fs::remove_file(&path).ok();
    }
}

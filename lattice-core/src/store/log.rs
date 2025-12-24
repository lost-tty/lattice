//! Log file I/O for append-only entry storage
//!
//! Each author has a log file containing length-delimited LogRecord messages.
//! LogRecord = { hash: [u8; 32], entry_bytes: SignedEntry }

use crate::proto::{LogRecord, SignedEntry};
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
    
    #[error("Entry too large: {0} bytes (max {MAX_ENTRY_SIZE})")]
    EntryTooLarge(usize),
    
    #[error("Unexpected EOF while reading entry")]
    UnexpectedEof,
    
    #[error("Hash mismatch: stored hash does not match computed hash")]
    HashMismatch,
}

/// Append a SignedEntry to a log file as a LogRecord
pub fn append_entry(path: impl AsRef<Path>, entry: &SignedEntry) -> Result<u64, LogError> {
    let path = path.as_ref();
    
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    
    let mut writer = BufWriter::new(file);
    
    // Serialize the SignedEntry
    let entry_bytes = entry.encode_to_vec();
    
    if entry_bytes.len() > MAX_ENTRY_SIZE {
        return Err(LogError::EntryTooLarge(entry_bytes.len()));
    }
    
    // Compute hash
    let hash: [u8; 32] = blake3::hash(&entry_bytes).into();
    
    // Create LogRecord
    let record = LogRecord {
        hash: hash.to_vec(),
        entry_bytes,
    };
    
    // Write length-delimited LogRecord
    let mut buf = Vec::new();
    record.encode_length_delimited(&mut buf)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    
    writer.write_all(&buf)?;
    writer.flush()?;
    
    // Ensure data is physically written to disk
    writer.get_ref().sync_all()?;
    
    // Return new file size
    let metadata = std::fs::metadata(path)?;
    Ok(metadata.len())
}

/// Read all SignedEntry messages from a log file (with hash verification)
pub fn read_entries(path: impl AsRef<Path>) -> Result<Vec<SignedEntry>, LogError> {
    read_entries_after(path, None)
}

/// Read all entries that come AFTER the given hash.
/// If `last_hash` is None, reads all entries.
pub fn read_entries_after(path: impl AsRef<Path>, last_hash: Option<[u8; 32]>) -> Result<Vec<SignedEntry>, LogError> {
    let reader = match LogReader::open(&path) {
        Ok(r) => r,
        Err(LogError::Io(e)) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    
    let mut entries = Vec::new();
    let mut found_start = last_hash.is_none();
    
    for result in reader {
        let (hash, entry) = result?;
        
        if found_start {
            entries.push(entry);
        } else if let Some(target) = last_hash {
            if hash == target {
                found_start = true;
            }
        }
    }
    
    Ok(entries)
}

/// Iterator over entries in a log file
/// Returns (hash, SignedEntry) pairs
pub struct LogReader {
    reader: BufReader<File>,
}

impl LogReader {
    /// Open a log file for reading
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LogError> {
        let file = File::open(path)?;
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
    let entry = SignedEntry::decode(&record.entry_bytes[..])?;
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
    use crate::store::signed_entry::EntryBuilder;
    use crate::proto::Operation;
    use std::env::temp_dir;

    fn temp_log_path(name: &str) -> std::path::PathBuf {
        temp_dir().join(format!("lattice_test_{}.log", name))
    }
    
    /// Compute hash the same way append_entry does
    fn compute_entry_hash(entry: &SignedEntry) -> [u8; 32] {
        let entry_bytes = entry.encode_to_vec();
        blake3::hash(&entry_bytes).into()
    }

    #[test]
    fn test_append_and_read_single() {
        let path = temp_log_path("single_v6");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        let hlc = HLC::now_with_clock(&clock);
        
        let entry = EntryBuilder::new(1, hlc)
            .operation(Operation::put("/test/key", b"value".to_vec()))
            .sign(&node);
        
        append_entry(&path, &entry).unwrap();
        
        let entries = read_entries(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry_bytes, entry.entry_bytes);
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_append_multiple() {
        let path = temp_log_path("multiple_v6");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        for i in 1..=5 {
            let entry = EntryBuilder::new(i, HLC::now_with_clock(&clock))
                .operation(Operation::put(format!("/key/{}", i), format!("value{}", i).into_bytes()))
                .sign(&node);
            append_entry(&path, &entry).unwrap();
        }
        
        let entries = read_entries(&path).unwrap();
        assert_eq!(entries.len(), 5);
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_entries_after() {
        let path = temp_log_path("after_v6");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        let mut entries = Vec::new();
        let mut hashes = Vec::new();
        
        for i in 1..=5 {
            let entry = EntryBuilder::new(i, HLC::now_with_clock(&clock))
                .operation(Operation::put(format!("/key/{}", i), format!("value{}", i).into_bytes()))
                .sign(&node);
            hashes.push(compute_entry_hash(&entry));
            append_entry(&path, &entry).unwrap();
            entries.push(entry);
        }
        
        // Read entries after hash[1] (second entry) -> should get entries 3, 4, 5
        let result = read_entries_after(&path, Some(hashes[1])).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].entry_bytes, entries[2].entry_bytes);
        
        // Read all entries (no hash)
        let all = read_entries_after(&path, None).unwrap();
        assert_eq!(all.len(), 5);
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_hash_not_found_returns_empty() {
        let path = temp_log_path("not_found_v6");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        let entry = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .operation(Operation::put("/key", b"value".to_vec()))
            .sign(&node);
        append_entry(&path, &entry).unwrap();
        
        let fake_hash = [0u8; 32];
        let result = read_entries_after(&path, Some(fake_hash)).unwrap();
        assert_eq!(result.len(), 0);
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_log_reader_returns_hash() {
        let path = temp_log_path("reader_hash_v6");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        let entry = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .operation(Operation::put("/key", b"value".to_vec()))
            .sign(&node);
        let expected_hash = compute_entry_hash(&entry);
        append_entry(&path, &entry).unwrap();
        
        let mut reader = LogReader::open(&path).unwrap();
        let (hash, _) = reader.next().unwrap().unwrap();
        assert_eq!(hash, expected_hash);
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_empty_file() {
        let path = temp_log_path("empty_v6");
        std::fs::remove_file(&path).ok();
        
        File::create(&path).unwrap();
        
        let entries = read_entries(&path).unwrap();
        assert_eq!(entries.len(), 0);
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_nonexistent() {
        let path = temp_log_path("nonexistent_v6");
        std::fs::remove_file(&path).ok();
        
        let entries = read_entries(&path).unwrap();
        assert_eq!(entries.len(), 0);
    }

    // --- Negative Tests ---

    #[test]
    fn test_corrupted_entry_detected() {
        use std::io::Seek;
        
        let path = temp_log_path("corrupted_v6");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        let entry = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .operation(Operation::put("/key", b"original".to_vec()))
            .sign(&node);
        append_entry(&path, &entry).unwrap();
        
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
        let path = temp_log_path("truncated_v6");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        let entry = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .operation(Operation::put("/key", b"data".to_vec()))
            .sign(&node);
        append_entry(&path, &entry).unwrap();
        
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
        let path = temp_log_path("too_large_v6");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        // Create payload larger than MAX_ENTRY_SIZE
        let huge_payload = vec![0u8; crate::MAX_ENTRY_SIZE + 100];
        
        let entry = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .operation(Operation::put("/huge", huge_payload))
            .sign(&node);
        
        let result = append_entry(&path, &entry);
        
        match result {
            Err(LogError::EntryTooLarge(size)) => assert!(size > crate::MAX_ENTRY_SIZE),
            _ => panic!("Expected EntryTooLarge error"),
        }
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_after_last_element() {
        let path = temp_log_path("boundary_last_v6");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        let entry = EntryBuilder::new(1, HLC::now_with_clock(&clock))
            .operation(Operation::put("/key", b"val".to_vec()))
            .sign(&node);
        append_entry(&path, &entry).unwrap();
        
        let hash = compute_entry_hash(&entry);
        
        // Ask for everything AFTER the only entry
        let result = read_entries_after(&path, Some(hash)).unwrap();
        
        // Result must be empty
        assert_eq!(result.len(), 0);
        
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_entry_exceeding_limit() {
        let path = temp_log_path("huge_read_v6");
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
        let path = temp_log_path("corruption_middle_v6");
        std::fs::remove_file(&path).ok();
        
        let node = NodeIdentity::generate();
        let clock = MockClock::new(1000);
        
        // Write 3 entries
        for i in 0..3 {
            let entry = EntryBuilder::new(i + 1, HLC::now_with_clock(&clock))
                .operation(Operation::put(format!("/key/{}", i), b"val"))
                .sign(&node);
            append_entry(&path, &entry).unwrap();
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

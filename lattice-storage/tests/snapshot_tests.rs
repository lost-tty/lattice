use lattice_storage::{StateBackend, StateDbError, TABLE_DATA};
use redb::ReadableTableMetadata;
use uuid::Uuid;
use std::io::Cursor;
use tempfile::tempdir;

fn populate_db(backend: &StateBackend) {
    let db = backend.db();
    let write_txn = db.begin_write().unwrap();
    {
        let mut table = write_txn.open_table(TABLE_DATA).unwrap();
        table.insert(b"key1".as_slice(), b"val1".as_slice()).unwrap();
        table.insert(b"key2".as_slice(), b"val2".as_slice()).unwrap();
    }
    write_txn.commit().unwrap();
}

#[test]
fn test_snapshot_restore_success() {
    let dir1 = tempdir().unwrap();
    let id1 = Uuid::new_v4();
    let backend1 = StateBackend::open(id1, dir1.path(), None, None, 0).unwrap();
    populate_db(&backend1);
    
    let mut buffer = Vec::new();
    backend1.snapshot(&mut buffer).unwrap();
    
    let dir2 = tempdir().unwrap();
    // Same ID required for restore unless we want ID mismatch
    let backend2 = StateBackend::open(id1, dir2.path(), None, None, 0).unwrap();
    
    backend2.restore(&mut Cursor::new(buffer)).unwrap();
    
    let read_txn = backend2.db().begin_read().unwrap();
    let table = read_txn.open_table(TABLE_DATA).unwrap();
    assert_eq!(table.len().unwrap(), 2);
    assert_eq!(table.get(&b"key1"[..]).unwrap().unwrap().value(), b"val1");
}

#[test]
fn test_snapshot_uuid_mismatch() {
    let dir1 = tempdir().unwrap();
    let id1 = Uuid::new_v4();
    let backend1 = StateBackend::open(id1, dir1.path(), None, None, 0).unwrap();
    populate_db(&backend1);
    
    let mut buffer = Vec::new();
    backend1.snapshot(&mut buffer).unwrap();
    
    let dir2 = tempdir().unwrap();
    let id2 = Uuid::new_v4(); // Different ID
    let backend2 = StateBackend::open(id2, dir2.path(), None, None, 0).unwrap();
    
    let err = backend2.restore(&mut Cursor::new(buffer)).unwrap_err();
    
    assert!(matches!(err, StateDbError::StoreIdMismatch { .. }));
}

#[test]
fn test_snapshot_checksum_failure_last_byte() {
    let dir1 = tempdir().unwrap();
    let id1 = Uuid::new_v4();
    let backend1 = StateBackend::open(id1, dir1.path(), None, None, 0).unwrap();
    populate_db(&backend1);
    
    let mut buffer = Vec::new();
    backend1.snapshot(&mut buffer).unwrap();
    
    // Corrupt last byte (checksum part)
    let len = buffer.len();
    buffer[len - 1] ^= 0xFF;
    
    let dir2 = tempdir().unwrap();
    let backend2 = StateBackend::open(id1, dir2.path(), None, None, 0).unwrap();
    
    let err = backend2.restore(&mut Cursor::new(buffer)).unwrap_err();
    
    // We expect InvalidSnapshot with "Checksum mismatch"
    match err {
        StateDbError::InvalidSnapshot(msg) if msg.contains("Checksum mismatch") => {},
        _ => panic!("Expected ChecksumMismatch, got: {:?}", err),
    }
}

#[test]
fn test_restore_transaction_atomicity() {
    let dir1 = tempdir().unwrap();
    let id1 = Uuid::new_v4();
    let backend1 = StateBackend::open(id1, dir1.path(), None, None, 0).unwrap();
    populate_db(&backend1);
    
    let mut buffer = Vec::new();
    backend1.snapshot(&mut buffer).unwrap();
    
    // Corrupt last byte of checksum
    let len = buffer.len();
    buffer[len - 1] ^= 0xFF;
    
    let dir2 = tempdir().unwrap();
    let backend2 = StateBackend::open(id1, dir2.path(), None, None, 0).unwrap();
    
    // 1. Initial state for db2 should be empty, let's pre-fill it
    populate_db(&backend2); 
    
    // 2. Try restore with corruption
    let err = backend2.restore(&mut Cursor::new(buffer)).unwrap_err();
    
    match err {
        StateDbError::InvalidSnapshot(msg) if msg.contains("Checksum mismatch") => {},
        _ => panic!("Expected ChecksumMismatch, got: {:?}", err),
    }
    
    // 3. Verify Transaction Rollback
    let read_txn = backend2.db().begin_read().unwrap();
    let table = read_txn.open_table(TABLE_DATA).unwrap();
    
    assert_eq!(table.len().unwrap(), 2, "Transaction did not rollback correctly! Table should still have old data.");
    assert_eq!(table.get(&b"key1"[..]).unwrap().unwrap().value(), b"val1");
}

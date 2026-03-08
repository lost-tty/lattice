use lattice_storage::{SnapshotError, StateBackend, StateDbError, StorageConfig, TABLE_DATA};
use redb::ReadableTableMetadata;
use std::io::Cursor;
use uuid::Uuid;

fn populate_db(backend: &StateBackend) {
    let db = backend.db();
    let write_txn = db.begin_write().unwrap();
    {
        let mut table = write_txn.open_table(TABLE_DATA).unwrap();
        table
            .insert(b"key1".as_slice(), b"val1".as_slice())
            .unwrap();
        table
            .insert(b"key2".as_slice(), b"val2".as_slice())
            .unwrap();
    }
    write_txn.commit().unwrap();
}

#[test]
fn test_snapshot_restore_success() {
    let id1 = Uuid::new_v4();
    let backend1 = StateBackend::open(id1, &StorageConfig::InMemory, None, 0).unwrap();
    populate_db(&backend1);

    let mut snap1 = Vec::new();
    backend1.snapshot(&mut snap1).unwrap();

    // Same ID required for restore unless we want ID mismatch
    let backend2 = StateBackend::open(id1, &StorageConfig::InMemory, None, 0).unwrap();

    backend2.restore(&mut Cursor::new(&snap1)).unwrap();

    let read_txn = backend2.db().begin_read().unwrap();
    let table = read_txn.open_table(TABLE_DATA).unwrap();
    assert_eq!(table.len().unwrap(), 2);
    assert_eq!(table.get(&b"key1"[..]).unwrap().unwrap().value(), b"val1");
    drop(table);
    drop(read_txn);

    // Snapshot after restore should be byte-identical to the original
    let mut snap2 = Vec::new();
    backend2.snapshot(&mut snap2).unwrap();
    assert_eq!(
        snap1, snap2,
        "Snapshot after restore should be byte-identical"
    );
}

#[test]
fn test_snapshot_uuid_mismatch() {
    let id1 = Uuid::new_v4();
    let backend1 = StateBackend::open(id1, &StorageConfig::InMemory, None, 0).unwrap();
    populate_db(&backend1);

    let mut buffer = Vec::new();
    backend1.snapshot(&mut buffer).unwrap();

    let id2 = Uuid::new_v4(); // Different ID
    let backend2 = StateBackend::open(id2, &StorageConfig::InMemory, None, 0).unwrap();

    let err = backend2.restore(&mut Cursor::new(buffer)).unwrap_err();

    assert!(matches!(err, StateDbError::StoreIdMismatch { .. }));
}

#[test]
fn test_restore_overwrites_existing_data() {
    let id = Uuid::new_v4();

    // Backend A has "old_key"
    let backend_a = StateBackend::open(id, &StorageConfig::InMemory, None, 0).unwrap();
    {
        let txn = backend_a.db().begin_write().unwrap();
        {
            let mut table = txn.open_table(TABLE_DATA).unwrap();
            table
                .insert(b"old_key".as_slice(), b"old_val".as_slice())
                .unwrap();
        }
        txn.commit().unwrap();
    }

    // Backend B has "new_key" only
    let backend_b = StateBackend::open(id, &StorageConfig::InMemory, None, 0).unwrap();
    {
        let txn = backend_b.db().begin_write().unwrap();
        {
            let mut table = txn.open_table(TABLE_DATA).unwrap();
            table
                .insert(b"new_key".as_slice(), b"new_val".as_slice())
                .unwrap();
        }
        txn.commit().unwrap();
    }

    // Snapshot B, restore onto A
    let mut snap = Vec::new();
    backend_b.snapshot(&mut snap).unwrap();
    backend_a.restore(&mut Cursor::new(&snap)).unwrap();

    let rtx = backend_a.db().begin_read().unwrap();
    let table = rtx.open_table(TABLE_DATA).unwrap();
    assert!(
        table.get(&b"old_key"[..]).unwrap().is_none(),
        "Old data should be gone after restore"
    );
    assert_eq!(
        table.get(&b"new_key"[..]).unwrap().unwrap().value(),
        b"new_val"
    );
    drop(table);
    drop(rtx);

    // Round-trip: snapshot A should now match snapshot B
    let mut snap_after = Vec::new();
    backend_a.snapshot(&mut snap_after).unwrap();
    assert_eq!(
        snap, snap_after,
        "Snapshot after restore should match source"
    );
}

#[test]
fn test_snapshot_checksum_failure_last_byte() {
    let id1 = Uuid::new_v4();
    let backend1 = StateBackend::open(id1, &StorageConfig::InMemory, None, 0).unwrap();
    populate_db(&backend1);

    let mut buffer = Vec::new();
    backend1.snapshot(&mut buffer).unwrap();

    // Corrupt last byte (checksum part)
    let mut corrupt_tail = buffer.clone();
    let len = corrupt_tail.len();
    corrupt_tail[len - 1] ^= 0xFF;

    let backend2 = StateBackend::open(id1, &StorageConfig::InMemory, None, 0).unwrap();

    let err = backend2
        .restore(&mut Cursor::new(corrupt_tail))
        .unwrap_err();
    assert!(
        matches!(
            err,
            StateDbError::InvalidSnapshot(SnapshotError::ChecksumMismatch)
        ),
        "Expected ChecksumMismatch, got: {:?}",
        err,
    );

    // Corrupt a data byte in the middle
    let mut corrupt_middle = buffer.clone();
    corrupt_middle[50] ^= 0xFF;

    let err_middle = backend2
        .restore(&mut Cursor::new(corrupt_middle))
        .unwrap_err();
    assert!(
        matches!(
            err_middle,
            StateDbError::InvalidSnapshot(SnapshotError::ChecksumMismatch) | StateDbError::Io(_)
        ),
        "Expected ChecksumMismatch or IO error for middle-byte corruption, got: {:?}",
        err_middle,
    );
}

#[test]
fn test_restore_transaction_atomicity() {
    let id1 = Uuid::new_v4();
    let backend1 = StateBackend::open(id1, &StorageConfig::InMemory, None, 0).unwrap();
    populate_db(&backend1);

    let mut buffer = Vec::new();
    backend1.snapshot(&mut buffer).unwrap();

    // Corrupt last byte of checksum
    let len = buffer.len();
    buffer[len - 1] ^= 0xFF;

    let backend2 = StateBackend::open(id1, &StorageConfig::InMemory, None, 0).unwrap();

    // 1. Initial state for db2 should be empty, let's pre-fill it
    populate_db(&backend2);

    // 2. Try restore with corruption
    let err = backend2.restore(&mut Cursor::new(buffer)).unwrap_err();

    assert!(
        matches!(
            err,
            StateDbError::InvalidSnapshot(SnapshotError::ChecksumMismatch)
        ),
        "Expected ChecksumMismatch, got: {:?}",
        err,
    );

    // 3. Verify Transaction Rollback
    let read_txn = backend2.db().begin_read().unwrap();
    let table = read_txn.open_table(TABLE_DATA).unwrap();

    assert_eq!(
        table.len().unwrap(),
        2,
        "Transaction did not rollback correctly! Table should still have old data."
    );
    assert_eq!(table.get(&b"key1"[..]).unwrap().unwrap().value(), b"val1");
}

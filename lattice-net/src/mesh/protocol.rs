//! Protocol - shared logic for bidirectional sync entry exchange

use crate::{MessageSink, MessageStream, LatticeNetError};
use lattice_core::StoreHandle;
use lattice_core::proto::{peer_message, PeerMessage, SignedEntry};
use lattice_core::SyncState;
use prost::Message;

/// Send entries that peer is missing based on state diff.
/// If author_filter is non-empty, only send entries for those authors.
/// Returns (entries_sent, optional_error).
/// 
/// Uses streaming channels to send entries without loading all into memory.
pub async fn send_missing_entries(
    sink: &mut MessageSink,
    store: &StoreHandle,
    my_state: &SyncState,
    peer_state: &SyncState,
    author_filter: &[[u8; 32]],
) -> Result<u64, LatticeNetError> {
    let mut missing = peer_state.diff(my_state);
    
    // Filter by authors if specified
    if !author_filter.is_empty() {
        missing.retain(|r| author_filter.contains(&r.author));
    }
    
    let mut entries_sent = 0u64;
    
    // Stream entries per-author
    for range in missing {
        let author = range.author;
        let from_hash = if range.from_hash == [0u8; 32] { None } else { Some(range.from_hash) };
        
        // Get streaming receiver for this author's entries
        let mut rx = store.stream_entries_after(&author, from_hash).await?;
        
        // Stream entries as they arrive from background thread
        while let Some(entry) = rx.recv().await {
            let sync_msg = PeerMessage {
                message: Some(peer_message::Message::SyncEntry(lattice_core::proto::SyncEntry {
                    signed_entry: entry.encode_to_vec(),
                    hash: vec![],
                })),
            };
            sink.send(&sync_msg).await?;
            entries_sent += 1;
        }
    }
    
    // Send SyncDone
    let done = PeerMessage {
        message: Some(peer_message::Message::SyncDone(lattice_core::proto::SyncDone {
            entries_sent,
        })),
    };
    sink.send(&done).await?;
    
    Ok(entries_sent)
}

/// Receive and apply entries until SyncDone is received.
/// Returns (entries_applied, entries_reported_by_peer).
pub async fn receive_entries(
    stream: &mut MessageStream,
    store: &StoreHandle,
) -> Result<(u64, u64), LatticeNetError> {
    let mut entries_applied = 0u64;
    let mut entries_reported = 0u64;
    
    loop {
        match stream.recv().await {
            Ok(Some(msg)) => match msg.message {
                Some(peer_message::Message::SyncEntry(entry)) => {
                    if let Ok(signed) = SignedEntry::decode(&entry.signed_entry[..]) {
                        if store.ingest_entry(signed).await.is_ok() {
                            entries_applied += 1;
                        }
                    }
                }
                Some(peer_message::Message::SyncDone(done)) => {
                    entries_reported = done.entries_sent;
                    break;
                }
                _ => {}
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    
    Ok((entries_applied, entries_reported))
}

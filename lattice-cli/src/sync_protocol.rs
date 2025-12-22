//! Sync Protocol - shared logic for bidirectional sync
//!
//! Provides reusable functions for sending and receiving entries during sync.
//! Used by both accept_handler (incoming sync) and sync (outgoing sync).

use crate::node::StoreHandle;
use lattice_core::proto::{peer_message, PeerMessage, SignedEntry};
use lattice_core::sync_state::SyncState;
use lattice_net::{MessageSink, MessageStream};
use prost::Message;
use std::collections::VecDeque;

/// Send entries that peer is missing based on state diff.
/// Returns (entries_sent, optional_error).
pub async fn send_missing_entries(
    sink: &mut MessageSink,
    store: &StoreHandle,
    my_state: &SyncState,
    peer_state: &SyncState,
) -> Result<u64, String> {
    let missing = peer_state.diff(my_state);
    
    // Build queues for each author's entries
    let mut author_entries: Vec<VecDeque<SignedEntry>> = Vec::new();
    for range in missing {
        let from_hash = if range.from_hash == [0u8; 32] { None } else { Some(range.from_hash) };
        let entries = store.read_entries_after(&range.author, from_hash).await
            .map_err(|e| format!("Failed to read entries: {}", e))?;
        if !entries.is_empty() {
            author_entries.push(entries.into());
        }
    }
    
    // Stream entries in HLC (causal) order
    let mut entries_sent = 0u64;
    for entry in lattice_core::CausalEntryIter::new(author_entries) {
        let sync_msg = PeerMessage {
            message: Some(peer_message::Message::SyncEntry(lattice_core::proto::SyncEntry {
                signed_entry: entry.encode_to_vec(),
                hash: vec![],
            })),
        };
        sink.send(&sync_msg).await?;
        entries_sent += 1;
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
) -> Result<(u64, u64), String> {
    let mut entries_applied = 0u64;
    let mut entries_reported = 0u64;
    
    loop {
        match stream.recv().await {
            Ok(Some(msg)) => match msg.message {
                Some(peer_message::Message::SyncEntry(entry)) => {
                    if let Ok(signed) = SignedEntry::decode(&entry.signed_entry[..]) {
                        if store.apply_entry(signed).await.is_ok() {
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

//! Lattice API - Unified gRPC server/client
//!
//! Features:
//! - `server` (default): gRPC server implementation
//! - `client` (default): gRPC client implementation
//! - `ffi`: Adds UniFFI derives to proto types for FFI bindings

#[cfg(feature = "ffi")]
uniffi::setup_scaffolding!();

pub mod proto {
    tonic::include_proto!("lattice.daemon.v1");
}

pub use tonic;

// Conversions from model types to proto types
impl From<lattice_model::StoreMeta> for proto::StoreMeta {
    fn from(m: lattice_model::StoreMeta) -> Self {
        proto::StoreMeta {
            id: m.store_id.as_bytes().to_vec(),
            store_type: m.store_type,
            schema_version: m.schema_version,
        }
    }
}

impl From<lattice_model::hlc::HLC> for proto::Hlc {
    fn from(hlc: lattice_model::hlc::HLC) -> Self {
        proto::Hlc {
            wall_time: hlc.wall_time,
            counter: hlc.counter,
        }
    }
}

impl From<lattice_model::weaver::Condition> for proto::Condition {
    fn from(c: lattice_model::weaver::Condition) -> Self {
        match c {
            lattice_model::weaver::Condition::V1(deps) => proto::Condition {
                kind: Some(proto::condition::Kind::V1(proto::CausalDeps {
                    hashes: deps.into_iter()
                        .filter(|h| *h != lattice_model::types::Hash::ZERO)
                        .map(|h| h.to_vec())
                        .collect(),
                })),
            },
        }
    }
}

impl From<lattice_model::weaver::SignedIntention> for proto::SignedIntention {
    fn from(s: lattice_model::weaver::SignedIntention) -> Self {
        let hash = s.intention.hash();
        proto::SignedIntention {
            hash: hash.to_vec(),
            author: s.intention.author.to_vec(),
            signature: s.signature.0.to_vec(),
            timestamp: Some(s.intention.timestamp.into()),
            store_id: s.intention.store_id.as_bytes().to_vec(),
            store_prev: s.intention.store_prev.to_vec(),
            condition: Some(s.intention.condition.into()),
            ops: s.intention.ops,
        }
    }
}

impl From<lattice_model::SExpr> for proto::SExpr {
    fn from(expr: lattice_model::SExpr) -> Self {
        use lattice_model::SExpr as M;
        use proto::s_expr;
        let value = match expr {
            M::Symbol(s) => s_expr::Value::Symbol(s),
            M::Str(s) => s_expr::Value::Str(s),
            M::Raw(b) => s_expr::Value::Raw(b),
            M::Num(n) => s_expr::Value::Num(n),
            M::List(items) => s_expr::Value::List(proto::SExprList {
                items: items.into_iter().map(Into::into).collect(),
            }),
        };
        proto::SExpr { value: Some(value) }
    }
}

impl From<proto::SExpr> for lattice_model::SExpr {
    fn from(expr: proto::SExpr) -> Self {
        use lattice_model::SExpr as M;
        use proto::s_expr;
        match expr.value {
            Some(s_expr::Value::Symbol(s)) => M::Symbol(s),
            Some(s_expr::Value::Str(s)) => M::Str(s),
            Some(s_expr::Value::Raw(b)) => M::Raw(b),
            Some(s_expr::Value::Num(n)) => M::Num(n),
            Some(s_expr::Value::List(list)) => M::List(
                list.items.into_iter().map(Into::into).collect(),
            ),
            None => M::Symbol("?".into()),
        }
    }
}

impl From<proto::Hlc> for lattice_model::hlc::HLC {
    fn from(p: proto::Hlc) -> Self {
        lattice_model::hlc::HLC::new(p.wall_time, p.counter)
    }
}

impl From<proto::Condition> for lattice_model::weaver::Condition {
    fn from(p: proto::Condition) -> Self {
        match p.kind {
            Some(proto::condition::Kind::V1(deps)) => {
                let hashes = deps.hashes.into_iter().filter_map(|h| {
                    lattice_model::types::Hash::try_from(h.as_slice()).ok()
                }).collect();
                lattice_model::weaver::Condition::V1(hashes)
            }
            None => lattice_model::weaver::Condition::V1(vec![]),
        }
    }
}

impl From<proto::SignedIntention> for lattice_model::weaver::SignedIntention {
    fn from(p: proto::SignedIntention) -> Self {
        let author = lattice_model::types::PubKey(
            p.author.try_into().unwrap_or([0u8; 32])
        );
        let timestamp = p.timestamp
            .map(Into::into)
            .unwrap_or(lattice_model::hlc::HLC::new(0, 0));
        let store_id = uuid::Uuid::from_slice(&p.store_id).unwrap_or_default();
        let store_prev = lattice_model::types::Hash::try_from(p.store_prev.as_slice())
            .unwrap_or(lattice_model::types::Hash::ZERO);
        let condition = p.condition
            .map(Into::into)
            .unwrap_or(lattice_model::weaver::Condition::V1(vec![]));
        let sig_bytes: [u8; 64] = p.signature.try_into().unwrap_or([0u8; 64]);

        lattice_model::weaver::SignedIntention {
            intention: lattice_model::weaver::Intention {
                author,
                timestamp,
                store_id,
                store_prev,
                condition,
                ops: p.ops,
            },
            signature: lattice_model::types::Signature(sig_bytes),
        }
    }
}

impl From<proto::FloatingIntention> for lattice_model::weaver::FloatingIntention {
    fn from(p: proto::FloatingIntention) -> Self {
        lattice_model::weaver::FloatingIntention {
            signed: p.intention
                .map(Into::into)
                .unwrap_or_else(|| lattice_model::weaver::SignedIntention {
                    intention: lattice_model::weaver::Intention {
                        author: lattice_model::types::PubKey([0u8; 32]),
                        timestamp: lattice_model::hlc::HLC::new(0, 0),
                        store_id: uuid::Uuid::nil(),
                        store_prev: lattice_model::types::Hash::ZERO,
                        condition: lattice_model::weaver::Condition::V1(vec![]),
                        ops: vec![],
                    },
                    signature: lattice_model::types::Signature([0u8; 64]),
                }),
            received_at: p.received_at,
        }
    }
}

impl From<lattice_model::weaver::FloatingIntention> for proto::FloatingIntention {
    fn from(fi: lattice_model::weaver::FloatingIntention) -> Self {
        proto::FloatingIntention {
            intention: Some(fi.signed.into()),
            received_at: fi.received_at,
        }
    }
}

impl From<lattice_model::weaver::WitnessEntry> for proto::WitnessLogEntry {
    fn from(e: lattice_model::weaver::WitnessEntry) -> Self {
        proto::WitnessLogEntry {
            seq: e.seq,
            hash: e.content_hash.to_vec(),
            content: e.content,
            signature: e.signature,
        }
    }
}

impl From<proto::WitnessLogEntry> for lattice_model::weaver::WitnessEntry {
    fn from(e: proto::WitnessLogEntry) -> Self {
        let content_hash = lattice_model::types::Hash::try_from(e.hash.as_slice())
            .unwrap_or(lattice_model::types::Hash::ZERO);
        lattice_model::weaver::WitnessEntry {
            seq: e.seq,
            content_hash,
            content: e.content,
            signature: e.signature,
        }
    }
}

pub mod backend;

#[cfg(feature = "server")]
mod node_service;

#[cfg(feature = "server")]
mod store_service;
#[cfg(feature = "server")]
mod dynamic_store_service;
#[cfg(feature = "server")]
mod server;

#[cfg(feature = "client")]
mod client;

#[cfg(feature = "server")]
pub use server::RpcServer;

#[cfg(feature = "client")]
pub use client::RpcClient;

// Re-export backend types for consumers
pub use backend::{
    LatticeBackend, Backend, NodeEvent, BackendError, BackendResult, 
    AsyncResult, EventReceiver,
};

#[cfg(test)]
mod tests {
    use super::proto;
    use lattice_model::hlc::HLC;
    use lattice_model::types::Hash;
    use lattice_model::weaver::{Condition, Intention, SignedIntention};

    #[test]
    fn hlc_to_proto_preserves_fields() {
        let hlc = HLC::new(1_700_000_000_000, 42);
        let p: proto::Hlc = hlc.into();
        assert_eq!(p.wall_time, 1_700_000_000_000);
        assert_eq!(p.counter, 42);
    }

    #[test]
    fn condition_to_proto_maps_v1_and_filters_zero() {
        let h1 = Hash([0xAA; 32]);
        let cond = Condition::v1(vec![Hash::ZERO, h1]);
        let p: proto::Condition = cond.into();
        match p.kind.unwrap() {
            proto::condition::Kind::V1(deps) => {
                assert_eq!(deps.hashes.len(), 1, "ZERO hash should be filtered out");
                assert_eq!(deps.hashes[0], h1.to_vec());
            }
        }
    }

    #[test]
    fn signed_intention_to_proto_roundtrips() {
        let identity = lattice_model::NodeIdentity::generate();
        let pk = identity.public_key();
        let store_id = uuid::Uuid::from_bytes([0xBB; 16]);
        let prev = Hash([0xCC; 32]);
        let dep = Hash([0xDD; 32]);

        let intention = Intention {
            author: pk,
            timestamp: HLC::new(999, 7),
            store_id,
            store_prev: prev,
            condition: Condition::v1(vec![dep]),
            ops: vec![1, 2, 3, 4],
        };
        let hash = intention.hash();
        let signed = SignedIntention::sign(intention, identity.signing_key());

        let p: proto::SignedIntention = signed.into();

        assert_eq!(p.hash, hash.to_vec(), "hash");
        assert_eq!(p.author, pk.0.to_vec(), "author");
        assert_eq!(p.signature.len(), 64, "signature length");
        assert_eq!(p.store_id, store_id.as_bytes().to_vec(), "store_id");
        assert_eq!(p.store_prev, prev.to_vec(), "store_prev");
        assert_eq!(p.ops, vec![1, 2, 3, 4], "ops");

        // HLC
        let ts = p.timestamp.unwrap();
        assert_eq!(ts.wall_time, 999);
        assert_eq!(ts.counter, 7);

        // Condition
        let cond = p.condition.unwrap();
        match cond.kind.unwrap() {
            proto::condition::Kind::V1(deps) => {
                assert_eq!(deps.hashes.len(), 1);
                assert_eq!(deps.hashes[0], dep.to_vec());
            }
        }
    }
}

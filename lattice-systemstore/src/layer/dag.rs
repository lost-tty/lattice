use lattice_model::{Hash, IntentionInfo};
use lattice_proto::storage::{universal_op, UniversalOp};
use prost::Message;
use std::borrow::Cow;

/// Which side of the `UniversalOp` envelope to extract.
#[derive(Copy, Clone)]
pub(crate) enum DagScope {
    /// Extract `AppData` payload — for the inner state machine.
    AppData,
    /// Extract `SystemOp` payload — for the system table.
    System,
}

/// Wraps `DagQueries` to unwrap `UniversalOp` envelopes from payloads.
///
/// The DAG stores raw `UniversalOp`-encoded bytes. `SystemLayer` splits
/// intentions into system vs app-data paths, stripping the envelope from
/// `Op.info.payload`. This wrapper does the same for DAG lookups, so
/// `op.info.payload` and `dag.get_intention().payload` are always in the
/// same coordinate system for the consumer.
pub(crate) struct ScopedDag<'a> {
    pub inner: &'a dyn lattice_model::DagQueries,
    pub scope: DagScope,
}

impl ScopedDag<'_> {
    fn unwrap_payload(
        info: IntentionInfo<'static>,
        scope: DagScope,
    ) -> anyhow::Result<IntentionInfo<'static>> {
        let universal = UniversalOp::decode(info.payload.as_ref())
            .map_err(|e| anyhow::anyhow!("invalid UniversalOp in DAG: {e}"))?;
        let payload = match (scope, universal.op) {
            (DagScope::AppData, Some(universal_op::Op::AppData(data))) => data,
            (DagScope::System, Some(universal_op::Op::System(sys))) => sys.encode_to_vec(),
            // Wrong variant for this scope — return empty payload.
            _ => Vec::new(),
        };
        Ok(IntentionInfo {
            payload: Cow::Owned(payload),
            ..info
        })
    }
}

impl lattice_model::DagQueries for ScopedDag<'_> {
    fn get_intention(&self, hash: &Hash) -> anyhow::Result<IntentionInfo<'static>> {
        Self::unwrap_payload(self.inner.get_intention(hash)?, self.scope)
    }

    fn find_lca(&self, a: &Hash, b: &Hash) -> anyhow::Result<Hash> {
        self.inner.find_lca(a, b)
    }

    fn get_path(&self, from: &Hash, to: &Hash) -> anyhow::Result<Vec<Hash>> {
        self.inner.get_path(from, to)
    }

    fn is_ancestor(&self, ancestor: &Hash, descendant: &Hash) -> anyhow::Result<bool> {
        self.inner.is_ancestor(ancestor, descendant)
    }
}

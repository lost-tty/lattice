//! Shared test node builder — creates an in-memory `NodeBuilder` with the
//! `core:nullstore` opener pre-registered.

use lattice_node::{direct_opener, DataDir, NodeBuilder};
use lattice_systemstore::SystemLayer;

type TestNullState = SystemLayer<crate::null_state::NullState>;

/// Return an in-memory `NodeBuilder` with the null-store opener registered.
///
/// Call sites that need additional openers (e.g. kvstore) can chain
/// `.with_opener(...)` on the returned builder.
pub fn test_node_builder(data_dir: DataDir) -> NodeBuilder {
    NodeBuilder::new(data_dir)
        .in_memory()
        .with_opener(crate::STORE_TYPE_NULLSTORE, || {
            direct_opener::<TestNullState>()
        })
}

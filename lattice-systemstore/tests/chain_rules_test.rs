use lattice_mockkernel::wrap_app_data;
use lattice_mockkernel::{NullState, TestStore};
use lattice_model::dag_queries::HashMapDag;
use lattice_model::hlc::HLC;
use lattice_model::types::{Hash, PubKey};
use lattice_model::{Op, StateMachine};

fn create_test_op(
    author: PubKey,
    id: Hash,
    timestamp: HLC,
    prev_hash: Hash,
    deps: &[Hash],
) -> Op<'static> {
    let payload = wrap_app_data(b"ignored");
    let deps_leaked = Box::leak(deps.to_vec().into_boxed_slice());
    Op {
        info: lattice_model::IntentionInfo {
            hash: id,
            payload: std::borrow::Cow::Owned(payload),
            timestamp,
            author,
        },
        causal_deps: deps_leaked,
        prev_hash,
    }
}

#[test]
fn test_chain_rules_compliance() {
    let store = TestStore::<NullState>::new();
    let dag = HashMapDag::new();
    let author = PubKey::from([0x99; 32]);
    let hlc = HLC::now();
    let hash1 = Hash::from([0x11; 32]);

    // 1. Invalid genesis: prev_hash must be ZERO
    let bad_genesis = create_test_op(author, hash1, hlc, Hash::from([0x01; 32]), &[]);
    let err = store.state.apply(&bad_genesis, &dag).unwrap_err();
    let err_str = err.to_string().to_lowercase();
    assert!(
        err_str.contains("invalid genesis"),
        "Expected InvalidGenesis, got: {err:?}",
    );

    // 2. Valid genesis
    let valid_genesis = create_test_op(author, hash1, hlc, Hash::ZERO, &[]);
    dag.record(&valid_genesis);
    store.state.apply(&valid_genesis, &dag).unwrap();

    // 3. Idempotency: applying the same op again should succeed
    store.state.apply(&valid_genesis, &dag).unwrap();

    // 4. Broken chain: prev_hash doesn't match current tip
    let hash2 = Hash::from([0x22; 32]);
    let bad_link = create_test_op(author, hash2, hlc, Hash::ZERO, &[]);
    let err = store.state.apply(&bad_link, &dag).unwrap_err();
    let err_str = err.to_string().to_lowercase();
    assert!(
        err_str.contains("broken chain"),
        "Expected BrokenChain, got: {err:?}",
    );

    // 5. Valid link: prev_hash matches tip (hash1)
    let valid_link = create_test_op(author, hash2, hlc, hash1, &[]);
    dag.record(&valid_link);
    store.state.apply(&valid_link, &dag).unwrap();
}

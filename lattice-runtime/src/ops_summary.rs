//! Intention ops decoding — turns raw `UniversalOp` bytes into structured SExprs.

use lattice_model::SExpr;

/// Decode an intention's ops bytes into structured `(system ...)` or `(data ...)` SExprs.
///
/// System ops are fully decoded. App data is decoded via the store's
/// `Introspectable` dispatcher.
pub fn summarize_intention_ops(
    ops: &[u8],
    dispatcher: &dyn lattice_store_base::Introspectable,
    hash: &lattice_model::Hash,
) -> Vec<SExpr> {
    use lattice_proto::storage::{UniversalOp, universal_op};
    use prost::Message;

    let Ok(universal) = UniversalOp::decode(ops) else {
        return vec![SExpr::raw(hash.0[..4].to_vec())];
    };

    match universal.op {
        Some(universal_op::Op::System(sys_op)) => {
            let mut items = vec![SExpr::sym("system")];
            items.extend(crate::system_summary::summarize(&sys_op));
            vec![SExpr::list(items)]
        }
        Some(universal_op::Op::AppData(data)) => {
            let mut items = vec![SExpr::sym("data")];
            items.extend(decode_app_ops(&data, dispatcher, hash));
            vec![SExpr::list(items)]
        }
        None => vec![SExpr::raw(hash.0[..4].to_vec())],
    }
}

/// Decode app-data bytes into operation SExprs using the store's introspection.
fn decode_app_ops(
    data: &[u8],
    dispatcher: &dyn lattice_store_base::Introspectable,
    hash: &lattice_model::Hash,
) -> Vec<SExpr> {
    dispatcher.decode_payload(data)
        .map(|msg| {
            let s = dispatcher.summarize_payload(&msg);
            if s.is_empty() { vec![SExpr::raw(hash.0[..4].to_vec())] } else { s }
        })
        .unwrap_or_else(|_| vec![SExpr::raw(hash.0[..4].to_vec())])
}

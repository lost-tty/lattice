use lattice_proto::storage::ChildStatus as ProtoChildStatus;

/// Map proto `ChildStatus` to model `ChildStatus`.
///
/// Used by both the apply path (event emission) and the read path (table queries).
pub fn map_to_model_status(proto: ProtoChildStatus) -> lattice_model::store_info::ChildStatus {
    match proto {
        ProtoChildStatus::CsActive => lattice_model::store_info::ChildStatus::Active,
        ProtoChildStatus::CsArchived => lattice_model::store_info::ChildStatus::Archived,
        _ => lattice_model::store_info::ChildStatus::Unknown,
    }
}

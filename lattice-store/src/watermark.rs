//! Watermarks for agreeing on safe compaction points

/// A watermark representing a safe cut-off point for log compaction.
///
/// Nodes use watermarks to agree on which entries can be safely
/// deleted and replaced with snapshots.
pub struct Watermark {
    // TODO: watermark data
}

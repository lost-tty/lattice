//! Display helpers for CLI output formatting
//!
//! Provides formatting functions for CLI output.

use lattice_runtime::PubKey;
use owo_colors::AnsiColors;
use std::time::Duration;
use uuid::Uuid;

/// Parse bytes to Uuid
pub fn parse_uuid(bytes: &[u8]) -> Option<Uuid> {
    Uuid::from_slice(bytes).ok()
}

/// Format bytes as UUID string (or hex fallback)
pub fn format_id(bytes: &[u8]) -> String {
    if let Ok(uuid) = Uuid::from_slice(bytes) {
        uuid.to_string()
    } else {
        hex::encode(bytes)
    }
}

/// Format elapsed time as human-readable "X ago" string
pub fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

/// Get deterministic color for an author (based on first bytes)
/// Used for consistent author coloring across history and graph rendering.
pub fn author_color(author: &PubKey) -> AnsiColors {
    // 12 Safe colors (skipping Black, White, and Gray to ensure readability)
    const COLORS: [AnsiColors; 12] = [
        // Standard
        AnsiColors::Red, AnsiColors::Green, AnsiColors::Yellow,
        AnsiColors::Blue, AnsiColors::Magenta, AnsiColors::Cyan,
        // Bright / Bold variants
        AnsiColors::BrightRed, AnsiColors::BrightGreen, AnsiColors::BrightYellow,
        AnsiColors::BrightBlue, AnsiColors::BrightMagenta, AnsiColors::BrightCyan,
    ];
    // Mix first 4 bytes to prevent clumping when keys share prefixes
    let hash = author[0] as usize 
             ^ author[1] as usize 
             ^ author[2] as usize 
             ^ author[3] as usize;

    COLORS[hash % COLORS.len()]
}

/// Convert AnsiColors to ANSI escape code number
pub fn ansi_code(color: AnsiColors) -> u8 {
    match color {
        AnsiColors::Red => 31,
        AnsiColors::Green => 32,
        AnsiColors::Yellow => 33,
        AnsiColors::Blue => 34,
        AnsiColors::Magenta => 35,
        AnsiColors::Cyan => 36,
        AnsiColors::BrightRed => 91,
        AnsiColors::BrightGreen => 92,
        AnsiColors::BrightYellow => 93,
        AnsiColors::BrightBlue => 94,
        AnsiColors::BrightMagenta => 95,
        AnsiColors::BrightCyan => 96,
        _ => 37,
    }
}

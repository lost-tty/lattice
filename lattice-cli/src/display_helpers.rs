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

/// Render an SExpr with ANSI colors and hex truncation.
///
/// "Modern IDE" color scheme:
/// - Symbols: blue (keywords/verbs)
/// - Strings: green (universal string literal color)
/// - Numbers: yellow (distinct from strings, high contrast)
/// - Raw hex: magenta (binary/magic data)
/// - Parens: dimmed (structure fades, content pops)
pub fn render_sexpr_colored(expr: &lattice_runtime::SExpr, max_hex_bytes: usize) -> String {
    use lattice_runtime::SExpr;
    use owo_colors::OwoColorize;
    match expr {
        SExpr::Symbol(s) => format!("{}", s.blue()),
        SExpr::Str(s) => format!("\"{}\"", s.green()),
        SExpr::Raw(b) => {
            let h = if b.len() > max_hex_bytes {
                format!("{}…", hex::encode(&b[..max_hex_bytes]))
            } else {
                hex::encode(b)
            };
            format!("{}", h.magenta())
        }
        SExpr::Num(n) => format!("{}", n.yellow()),
        SExpr::List(items) => {
            let inner: Vec<String> = items.iter()
                .map(|i| render_sexpr_colored(i, max_hex_bytes))
                .collect();
            format!("{}{}{}", "(".dimmed(), inner.join(" "), ")".dimmed())
        }
    }
}

/// Render an SExpr with ANSI colors, hex truncation, and indentation.
///
/// Top-level list children get their own indented line (like `to_pretty()`).
pub fn render_sexpr_pretty_colored(expr: &lattice_runtime::SExpr, max_hex_bytes: usize) -> String {
    fmt_pretty_colored(expr, max_hex_bytes, 0)
}

fn fmt_pretty_colored(expr: &lattice_runtime::SExpr, max_hex_bytes: usize, indent: usize) -> String {
    use lattice_runtime::SExpr;
    use owo_colors::OwoColorize;
    match expr {
        SExpr::List(items) if items.len() > 1 && items.iter().any(|i| matches!(i, SExpr::List(_))) => {
            let pad = "  ".repeat(indent + 1);
            let head = render_sexpr_colored(&items[0], max_hex_bytes);
            let children: Vec<String> = items[1..].iter()
                .map(|item| format!("{}{}", pad, fmt_pretty_colored(item, max_hex_bytes, indent + 1)))
                .collect();
            format!("{}{}\n{}{}", "(".dimmed(), head, children.join("\n"), ")".dimmed())
        }
        _ => render_sexpr_colored(expr, max_hex_bytes),
    }
}

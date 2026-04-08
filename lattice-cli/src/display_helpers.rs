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
        AnsiColors::Red,
        AnsiColors::Green,
        AnsiColors::Yellow,
        AnsiColors::Blue,
        AnsiColors::Magenta,
        AnsiColors::Cyan,
        // Bright / Bold variants
        AnsiColors::BrightRed,
        AnsiColors::BrightGreen,
        AnsiColors::BrightYellow,
        AnsiColors::BrightBlue,
        AnsiColors::BrightMagenta,
        AnsiColors::BrightCyan,
    ];
    // Mix first 4 bytes to prevent clumping when keys share prefixes
    let hash = author[0] as usize ^ author[1] as usize ^ author[2] as usize ^ author[3] as usize;

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
            let inner: Vec<String> = items
                .iter()
                .map(|i| render_sexpr_colored(i, max_hex_bytes))
                .collect();
            format!("{}{}{}", "(".dimmed(), inner.join(" "), ")".dimmed())
        }
    }
}

/// Find a `(items (...))` child within an SExpr list and return the row slice.
/// Matches the standard list response convention: every list response wraps its
/// repeated field in a field named "items".
fn find_items_rows(children: &[lattice_runtime::SExpr]) -> Option<&[lattice_runtime::SExpr]> {
    use lattice_runtime::SExpr;
    children.iter().find_map(|child| match child {
        SExpr::List(pair) => match pair.as_slice() {
            [SExpr::Symbol(name), SExpr::List(rows)] if name == "items" => Some(rows.as_slice()),
            _ => None,
        },
        _ => None,
    })
}

/// Render an SExpr with ANSI colors, hex truncation, and indentation.
///
/// Top-level list children get their own indented line (like `to_pretty()`).
/// Lists of homogeneous records are rendered as aligned tables.
pub fn render_sexpr_pretty_colored(expr: &lattice_runtime::SExpr, max_hex_bytes: usize) -> String {
    fmt_pretty_colored(expr, max_hex_bytes, 0)
}

fn fmt_pretty_colored(
    expr: &lattice_runtime::SExpr,
    max_hex_bytes: usize,
    indent: usize,
) -> String {
    use lattice_runtime::SExpr;
    use owo_colors::OwoColorize;
    match expr {
        SExpr::List(items)
            if items.len() > 1 && items.iter().any(|i| matches!(i, SExpr::List(_))) =>
        {
            // Try table rendering for bare lists of homogeneous records
            // e.g. (list (Row (f1 v) ...) (Row (f1 v) ...))
            if let Some(table) = try_render_table(items, max_hex_bytes, indent) {
                return table;
            }

            // Explicit items field detection — (WrapperType (items (row1 row2 ...)))
            if let Some(rows) = find_items_rows(items) {
                if let Some(table) = try_render_table(rows, max_hex_bytes, indent) {
                    return table;
                }
            }

            let pad = "  ".repeat(indent + 1);
            let head = render_sexpr_colored(&items[0], max_hex_bytes);
            let children: Vec<String> = items[1..]
                .iter()
                .map(|item| {
                    format!(
                        "{}{}",
                        pad,
                        fmt_pretty_colored(item, max_hex_bytes, indent + 1)
                    )
                })
                .collect();
            format!(
                "{}{}\n{}{}",
                "(".dimmed(),
                head,
                children.join("\n"),
                ")".dimmed()
            )
        }
        _ => render_sexpr_colored(expr, max_hex_bytes),
    }
}

/// Detect and render a list of homogeneous records as an aligned table.
///
/// Triggers when `items` is `(head (T (f1 v) (f2 v) ...) ...)`:
/// - 2+ elements (head symbol + at least 1 row)
/// - All children after head are Lists
/// - All those Lists share the same first Symbol (the record type)
/// - All those Lists have the same length
/// - Each field in a row is a 2-element List `(name value)`
///
/// Returns `None` if the pattern doesn't match, letting the caller fall through.
fn try_render_table(
    items: &[lattice_runtime::SExpr],
    max_hex_bytes: usize,
    indent: usize,
) -> Option<String> {
    use lattice_runtime::SExpr;
    use owo_colors::OwoColorize;
    use unicode_width::UnicodeWidthStr;

    if items.is_empty() {
        return None;
    }

    // Determine rows: if items[0] is a Symbol, it's a head and rows are items[1..].
    // If items[0] is a List (headless list from Value::List), rows are all of items.
    let rows: &[SExpr] = match &items[0] {
        SExpr::Symbol(_) => {
            if items.len() < 2 {
                return None;
            }
            &items[1..]
        }
        SExpr::List(_) => items,
        _ => return None,
    };

    if rows.len() < 2 {
        return None;
    }

    // All rows must be Lists
    let row_lists: Vec<&Vec<SExpr>> = rows
        .iter()
        .map(|r| match r {
            SExpr::List(inner) => Some(inner),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;

    // All rows must have same length and >= 2 elements (type name + at least one field)
    let row_len = row_lists[0].len();
    if row_len < 2 || row_lists.iter().any(|r| r.len() != row_len) {
        return None;
    }

    // All rows must share the same head Symbol (the record type name)
    let type_name = match &row_lists[0][0] {
        SExpr::Symbol(s) => s.as_str(),
        _ => return None,
    };
    for row in &row_lists {
        match &row[0] {
            SExpr::Symbol(s) if s == type_name => {}
            _ => return None,
        }
    }

    // Each field in every row must be a 2-element List (name, value)
    // Extract headers from the first row
    let mut headers: Vec<String> = Vec::with_capacity(row_len - 1);
    for field in &row_lists[0][1..] {
        match field {
            SExpr::List(pair) if pair.len() == 2 => match &pair[0] {
                SExpr::Symbol(name) => headers.push(name.to_uppercase()),
                _ => return None,
            },
            _ => return None,
        }
    }

    // Verify all rows have the same field names and extract cell values.
    // All cells are rendered inline initially; the last column may be
    // re-rendered with pretty-printing if it overflows the terminal width.
    let num_cols = headers.len();
    let last_col = num_cols - 1;
    let mut cells: Vec<Vec<String>> = Vec::with_capacity(rows.len());
    // Collect last-column SExpr values for potential re-rendering
    let mut last_col_exprs: Vec<&SExpr> = Vec::with_capacity(rows.len());
    for row in &row_lists {
        let mut row_cells: Vec<String> = Vec::with_capacity(num_cols);
        for (i, field) in row[1..].iter().enumerate() {
            match field {
                SExpr::List(pair) if pair.len() == 2 => {
                    match &pair[0] {
                        SExpr::Symbol(name) if name.to_uppercase() == headers[i] => {}
                        _ => return None, // field name mismatch
                    }
                    row_cells.push(render_sexpr_colored(&pair[1], max_hex_bytes));
                    if i == last_col {
                        last_col_exprs.push(&pair[1]);
                    }
                }
                _ => return None,
            }
        }
        cells.push(row_cells);
    }

    // Compute column widths (exclude last column — it flows freely)
    let col_widths: Vec<usize> = (0..num_cols)
        .map(|i| {
            if i == last_col {
                return 0;
            }
            let header_w = headers[i].width();
            let max_cell_w = cells
                .iter()
                .map(|row| strip_ansi(&row[i]).width())
                .max()
                .unwrap_or(0);
            header_w.max(max_cell_w)
        })
        .collect();

    let pad = "  ".repeat(indent);
    let last_col_offset = pad.len() + col_widths[..last_col].iter().map(|w| w + 2).sum::<usize>();
    let term_width = terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(120);

    // Re-render last column cells with pretty-printing if they overflow
    for (row_idx, row) in cells.iter_mut().enumerate() {
        let inline_width = strip_ansi(&row[last_col]).width();
        if last_col_offset + inline_width > term_width {
            row[last_col] = fmt_pretty_colored(last_col_exprs[row_idx], max_hex_bytes, 0);
        }
    }

    // Render
    let mut out = String::new();

    // Header line
    out.push_str(&pad);
    for (i, h) in headers.iter().enumerate() {
        let w = col_widths[i];
        let styled = format!("{}", h.dimmed());
        let visible_len = h.width();
        out.push_str(&styled);
        if i + 1 < num_cols {
            for _ in 0..(w - visible_len + 2) {
                out.push(' ');
            }
        }
    }
    out.push('\n');

    // Data rows
    for row in &cells {
        out.push_str(&pad);
        for (i, cell) in row.iter().enumerate() {
            if i == last_col {
                // Last column: handle potential multi-line content
                let lines: Vec<&str> = cell.split('\n').collect();
                out.push_str(lines[0]);
                for cont_line in &lines[1..] {
                    out.push('\n');
                    for _ in 0..last_col_offset {
                        out.push(' ');
                    }
                    out.push_str(cont_line);
                }
            } else {
                let w = col_widths[i];
                let visible_len = strip_ansi(cell).width();
                out.push_str(cell);
                for _ in 0..(w - visible_len + 2) {
                    out.push(' ');
                }
            }
        }
        out.push('\n');
    }

    // Footer
    out.push_str(&pad);
    out.push_str(&format!("{}", format!("({} items)", rows.len()).dimmed()));

    Some(out)
}

/// Strip ANSI escape sequences for visible-width measurement.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_escape = false;
    for c in s.chars() {
        if in_escape {
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else if c == '\x1b' {
            in_escape = true;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_runtime::SExpr;

    #[test]
    fn table_renders_for_homogeneous_records() {
        // (items (KV (key "a") (value "hello")) (KV (key "bb") (value "x")))
        let expr = SExpr::list(vec![
            SExpr::sym("items"),
            SExpr::list(vec![
                SExpr::sym("KV"),
                SExpr::list(vec![SExpr::sym("key"), SExpr::str("a")]),
                SExpr::list(vec![SExpr::sym("value"), SExpr::str("hello")]),
            ]),
            SExpr::list(vec![
                SExpr::sym("KV"),
                SExpr::list(vec![SExpr::sym("key"), SExpr::str("bb")]),
                SExpr::list(vec![SExpr::sym("value"), SExpr::str("x")]),
            ]),
        ]);

        let rendered = render_sexpr_pretty_colored(&expr, 8);
        let plain = strip_ansi(&rendered);

        // Should have header, 2 data rows, and footer
        let lines: Vec<&str> = plain.lines().collect();
        assert_eq!(lines.len(), 4, "expected 4 lines, got: {:?}", lines);
        assert!(lines[0].contains("KEY"), "header should contain KEY");
        assert!(lines[0].contains("VALUE"), "header should contain VALUE");
        assert!(lines[1].contains("\"a\""), "row 1 should contain key a");
        assert!(
            lines[1].contains("\"hello\""),
            "row 1 should contain value hello"
        );
        assert!(lines[2].contains("\"bb\""), "row 2 should contain key bb");
        assert!(lines[2].contains("\"x\""), "row 2 should contain value x");
        assert!(lines[3].contains("(2 items)"), "footer should show count");
    }

    #[test]
    fn table_columns_are_aligned() {
        let expr = SExpr::list(vec![
            SExpr::sym("list"),
            SExpr::list(vec![
                SExpr::sym("Row"),
                SExpr::list(vec![SExpr::sym("name"), SExpr::str("a")]),
                SExpr::list(vec![SExpr::sym("count"), SExpr::Num(1)]),
            ]),
            SExpr::list(vec![
                SExpr::sym("Row"),
                SExpr::list(vec![SExpr::sym("name"), SExpr::str("longer")]),
                SExpr::list(vec![SExpr::sym("count"), SExpr::Num(999)]),
            ]),
        ]);

        let rendered = render_sexpr_pretty_colored(&expr, 8);
        let plain = strip_ansi(&rendered);
        let lines: Vec<&str> = plain.lines().collect();

        // The COUNT header and values should start at the same column
        let header_count_pos = lines[0].find("COUNT").expect("COUNT header");
        let row1_count_pos = lines[1].find('1').expect("row 1 count");
        let row2_count_pos = lines[2].find("999").expect("row 2 count");
        assert_eq!(
            header_count_pos, row1_count_pos,
            "COUNT column should be aligned"
        );
        assert_eq!(
            header_count_pos, row2_count_pos,
            "COUNT column should be aligned"
        );
    }

    #[test]
    fn table_skipped_for_heterogeneous_lists() {
        // Different head symbols — should NOT trigger table
        let expr = SExpr::list(vec![
            SExpr::sym("data"),
            SExpr::list(vec![
                SExpr::sym("TypeA"),
                SExpr::list(vec![SExpr::sym("x"), SExpr::Num(1)]),
            ]),
            SExpr::list(vec![
                SExpr::sym("TypeB"),
                SExpr::list(vec![SExpr::sym("x"), SExpr::Num(2)]),
            ]),
        ]);

        let rendered = render_sexpr_pretty_colored(&expr, 8);
        let plain = strip_ansi(&rendered);
        // Should fall through to normal pretty rendering (has parens)
        assert!(plain.contains("("), "should use normal s-expr rendering");
        assert!(!plain.contains("items)"), "should not have table footer");
    }

    #[test]
    fn table_renders_for_single_row() {
        // A single row should NOT render as a table (minimum 2 rows)
        let expr = SExpr::list(vec![
            SExpr::sym("items"),
            SExpr::list(vec![
                SExpr::sym("KV"),
                SExpr::list(vec![SExpr::sym("key"), SExpr::str("a")]),
                SExpr::list(vec![SExpr::sym("value"), SExpr::str("hello")]),
            ]),
        ]);

        let rendered = render_sexpr_pretty_colored(&expr, 8);
        let plain = strip_ansi(&rendered);
        // Should render as nested S-expression, not a table
        assert!(
            !plain.contains("KEY"),
            "single row should not render as table"
        );
        assert!(plain.contains("\"a\""), "value should be present");
    }

    #[test]
    fn table_skipped_for_mismatched_field_names() {
        // Same type name but different field names
        let expr = SExpr::list(vec![
            SExpr::sym("data"),
            SExpr::list(vec![
                SExpr::sym("T"),
                SExpr::list(vec![SExpr::sym("a"), SExpr::Num(1)]),
            ]),
            SExpr::list(vec![
                SExpr::sym("T"),
                SExpr::list(vec![SExpr::sym("b"), SExpr::Num(2)]),
            ]),
        ]);

        let rendered = render_sexpr_pretty_colored(&expr, 8);
        let plain = strip_ansi(&rendered);
        assert!(
            plain.contains("("),
            "mismatched fields should use normal rendering"
        );
    }

    #[test]
    fn table_unwraps_nested_single_field_wrappers() {
        // Real structure from dynamic_message_to_sexpr:
        // (ListResponse (items ((KV (key "a") (value "1")) (KV (key "b") (value "2")))))
        //  wrapper        field   bare list from Value::List
        let expr = SExpr::list(vec![
            SExpr::sym("ListResponse"),
            SExpr::list(vec![
                SExpr::sym("items"),
                SExpr::list(vec![
                    SExpr::list(vec![
                        SExpr::sym("KV"),
                        SExpr::list(vec![SExpr::sym("key"), SExpr::str("a")]),
                        SExpr::list(vec![SExpr::sym("value"), SExpr::str("1")]),
                    ]),
                    SExpr::list(vec![
                        SExpr::sym("KV"),
                        SExpr::list(vec![SExpr::sym("key"), SExpr::str("b")]),
                        SExpr::list(vec![SExpr::sym("value"), SExpr::str("2")]),
                    ]),
                ]),
            ]),
        ]);

        let rendered = render_sexpr_pretty_colored(&expr, 8);
        let plain = strip_ansi(&rendered);
        let lines: Vec<&str> = plain.lines().collect();
        assert_eq!(lines.len(), 4, "expected 4 lines, got: {:?}", lines);
        assert!(lines[0].contains("KEY"), "header should contain KEY");
        assert!(lines[0].contains("VALUE"), "header should contain VALUE");
        assert!(
            !plain.contains("ListResponse"),
            "wrapper should be unwrapped"
        );
    }

    #[test]
    fn debug_real_list_response_structure() {
        // Exact structure from dynamic_message_to_sexpr for a ListResponse
        // with repeated KeyValuePair items field:
        //
        // (ListResponse
        //   (items                    ← field wrapper [sym, val]
        //     (                       ← bare list (no head sym) from Value::List
        //       (KeyValuePair (key "a") (value "c") (conflicted true))
        //       (KeyValuePair (key "b") (value "21") (conflicted false)))))
        let expr = SExpr::list(vec![
            SExpr::sym("ListResponse"),
            SExpr::list(vec![
                SExpr::sym("items"),
                SExpr::list(vec![
                    SExpr::list(vec![
                        SExpr::sym("KeyValuePair"),
                        SExpr::list(vec![SExpr::sym("key"), SExpr::str("a")]),
                        SExpr::list(vec![SExpr::sym("value"), SExpr::str("c")]),
                        SExpr::list(vec![SExpr::sym("conflicted"), SExpr::sym("true")]),
                    ]),
                    SExpr::list(vec![
                        SExpr::sym("KeyValuePair"),
                        SExpr::list(vec![SExpr::sym("key"), SExpr::str("b")]),
                        SExpr::list(vec![SExpr::sym("value"), SExpr::str("21")]),
                        SExpr::list(vec![SExpr::sym("conflicted"), SExpr::sym("false")]),
                    ]),
                ]),
            ]),
        ]);

        let rendered = render_sexpr_pretty_colored(&expr, 8);
        let plain = strip_ansi(&rendered);
        eprintln!("RENDERED:\n{}", plain);

        // The table should appear at top level, unwrapped
        let lines: Vec<&str> = plain.lines().collect();
        assert_eq!(
            lines.len(),
            4,
            "expected header + 2 rows + footer: {:?}",
            lines
        );
        assert!(
            lines[0].contains("KEY"),
            "first line should be table header: {:?}",
            lines
        );
        assert!(!plain.contains("ListResponse"), "wrapper should be gone");
        assert!(plain.contains("\"a\""), "should contain first row key");
        assert!(plain.contains("\"b\""), "should contain second row key");
    }

    #[test]
    fn strip_ansi_removes_escape_codes() {
        assert_eq!(strip_ansi("\x1b[34mhello\x1b[0m"), "hello");
        assert_eq!(strip_ansi("no escapes"), "no escapes");
        assert_eq!(strip_ansi("\x1b[1;31mred\x1b[0m plain"), "red plain");
    }
}

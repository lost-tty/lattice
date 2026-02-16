//! S-expression type for structured payload summaries.
//!
//! Used by `Introspectable::summarize_payload` to return structured
//! operation descriptions that consumers can render however they want.

/// A structured s-expression value.
#[derive(Debug, Clone, PartialEq)]
pub enum SExpr {
    /// A symbol/keyword (unquoted): `put`, `child-add`, `:active`
    Symbol(String),
    /// A string value (quoted in display): `"my-key"`, `"alice"`
    Str(String),
    /// Raw binary data (displayed as hex)
    Raw(Vec<u8>),
    /// A numeric value
    Num(u64),
    /// A nested list: `(put "key" "value")`
    List(Vec<SExpr>),
}

impl SExpr {
    /// Create a symbol.
    pub fn sym(s: impl Into<String>) -> Self {
        SExpr::Symbol(s.into())
    }

    /// Create a string value.
    pub fn str(s: impl Into<String>) -> Self {
        SExpr::Str(s.into())
    }

    /// Create a raw bytes value.
    pub fn raw(b: impl Into<Vec<u8>>) -> Self {
        SExpr::Raw(b.into())
    }

    /// Create a numeric value.
    pub fn num(n: u64) -> Self {
        SExpr::Num(n)
    }

    /// Create a list.
    pub fn list(items: Vec<SExpr>) -> Self {
        SExpr::List(items)
    }

    /// Render to human-readable s-expression text (full hex for Raw values).
    pub fn to_text(&self) -> String {
        match self {
            SExpr::Symbol(s) => s.clone(),
            SExpr::Str(s) => format!("\"{}\"", s),
            SExpr::Raw(b) => hex::encode(b),
            SExpr::Num(n) => n.to_string(),
            SExpr::List(items) => {
                let inner: Vec<String> = items.iter().map(|i| i.to_text()).collect();
                format!("({})", inner.join(" "))
            }
        }
    }

    /// Render with truncated hex for `Raw` values.
    ///
    /// `max_hex_bytes` controls how many bytes of raw data to show.
    /// Longer values are truncated with `…`.
    pub fn to_compact(&self, max_hex_bytes: usize) -> String {
        match self {
            SExpr::Raw(b) if b.len() > max_hex_bytes => {
                format!("{}…", hex::encode(&b[..max_hex_bytes]))
            }
            SExpr::List(items) => {
                let inner: Vec<String> = items.iter().map(|i| i.to_compact(max_hex_bytes)).collect();
                format!("({})", inner.join(" "))
            }
            _ => self.to_text(),
        }
    }

    /// Render with indentation. Top-level list children get their own line.
    pub fn to_pretty(&self) -> String {
        self.fmt_pretty(0)
    }

    fn fmt_pretty(&self, indent: usize) -> String {
        match self {
            SExpr::List(items) if indent == 0 && items.len() > 1 => {
                // Top-level: head on first line, each child indented
                let pad = "  ".repeat(indent + 1);
                let head = items[0].to_text();
                let children: Vec<String> = items[1..].iter()
                    .map(|item| format!("{}{}", pad, item.fmt_pretty(indent + 1)))
                    .collect();
                format!("({}\n{})", head, children.join("\n"))
            }
            _ => self.to_text(),
        }
    }
}

impl std::fmt::Display for SExpr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Default display uses compact rendering (8 bytes = 16 hex chars)
        write!(f, "{}", self.to_compact(8))
    }
}

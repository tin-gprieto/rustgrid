use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The connection a notebook runs against.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotebookBinding {
    pub connection_id: String,
    pub database: Option<String>,
    pub schema: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Notebook {
    pub cells: Vec<Cell>,
    pub binding: Option<NotebookBinding>,
    /// Unknown notebook-level metadata, preserved verbatim.
    pub extra_metadata: Map<String, Value>,
    /// `(nbformat, nbformat_minor)` as read; `None` for a notebook not read from a file.
    pub format_version: Option<(u32, u32)>,
    /// Unknown top-level keys of the file, preserved verbatim.
    pub extra_top_level: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CellKind {
    Sql,
    Markdown,
    /// A non-SQL code cell, preserved as is.
    Raw,
}

/// The nbformat `cell_type` a [`CellKind::Raw`] cell came from, so it is written back unchanged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RawKind {
    /// A `code` cell (Python, `%load_ext sql`, `%sql <url>`, ...).
    #[default]
    Code,
    /// An nbformat `raw` cell.
    Raw,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub id: String,
    pub kind: CellKind,
    /// Only meaningful when `kind` is `Raw`.
    pub raw_kind: RawKind,
    /// Cell text without the `%%sql` prefix.
    pub source: String,
    /// What follows `%%sql` on the first line.
    pub magic_args: String,
    pub outputs: Vec<CellOutput>,
    pub execution_count: Option<u32>,
    pub extra_metadata: Map<String, Value>,
    /// Unknown cell-level keys (e.g. `attachments`), preserved verbatim.
    pub extra_fields: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CellOutput {
    Table {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
        truncated: bool,
    },
    Affected {
        rows: u64,
    },
    Error {
        message: String,
    },
    /// An nbformat output that has no structured equivalent, preserved verbatim.
    Raw(Value),
}

impl Cell {
    pub fn new_sql(source: impl Into<String>) -> Self {
        Self::new(CellKind::Sql, source)
    }

    pub fn new_markdown(source: impl Into<String>) -> Self {
        Self::new(CellKind::Markdown, source)
    }

    /// A code cell that only loads JupySQL (`%load_ext sql` / `%reload_ext sql`, blank lines
    /// allowed). [`crate::write_ipynb`] adds one when a notebook has none, so the app keeps such a
    /// cell out of sight instead of showing it as a Raw cell.
    pub fn is_sql_extension_loader(&self) -> bool {
        let mut lines = self
            .source
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .peekable();
        self.kind == CellKind::Raw
            && self.raw_kind == RawKind::Code
            && lines.peek().is_some()
            && lines.all(|line| matches!(line, "%load_ext sql" | "%reload_ext sql"))
    }

    fn new(kind: CellKind, source: impl Into<String>) -> Self {
        Self {
            id: generate_cell_id(),
            kind,
            raw_kind: RawKind::Code,
            source: source.into(),
            magic_args: String::new(),
            outputs: Vec::new(),
            execution_count: None,
            extra_metadata: Map::new(),
            extra_fields: Map::new(),
        }
    }
}

/// An nbformat 4.5 cell id: 16 hex digits of the clock plus a process-wide counter, so ids stay
/// distinct even when generated within the same clock tick.
pub(crate) fn generate_cell_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:016x}-{n:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn only_a_pure_loader_cell_is_an_extension_loader() {
        let raw = |source: &str| Cell {
            kind: CellKind::Raw,
            ..Cell::new_sql(source)
        };
        assert!(raw("%load_ext sql").is_sql_extension_loader());
        assert!(raw("\n  %reload_ext sql \n").is_sql_extension_loader());
        assert!(!raw("%load_ext sql\n%sql sqlite://").is_sql_extension_loader());
        assert!(!raw("").is_sql_extension_loader());
        assert!(!Cell::new_sql("%load_ext sql").is_sql_extension_loader());
        let mut nbformat_raw = raw("%load_ext sql");
        nbformat_raw.raw_kind = RawKind::Raw;
        assert!(!nbformat_raw.is_sql_extension_loader());
    }

    #[test]
    fn generated_ids_are_valid_and_distinct() {
        let mut seen = HashSet::new();
        for i in 0..1000 {
            let cell = if i % 2 == 0 {
                Cell::new_sql("select 1")
            } else {
                Cell::new_markdown("# t")
            };
            assert!((1..=64).contains(&cell.id.len()), "{}", cell.id);
            assert!(
                cell.id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "{}",
                cell.id
            );
            assert!(seen.insert(cell.id), "duplicate id");
        }
    }
}

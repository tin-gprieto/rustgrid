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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CellKind {
    Sql,
    Markdown,
    /// A non-SQL code cell, preserved as is.
    Raw,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub id: String,
    pub kind: CellKind,
    /// Cell text without the `%%sql` prefix.
    pub source: String,
    /// What follows `%%sql` on the first line.
    pub magic_args: String,
    pub outputs: Vec<CellOutput>,
    pub execution_count: Option<u32>,
    pub extra_metadata: Map<String, Value>,
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
}

impl Cell {
    pub fn new_sql(source: impl Into<String>) -> Self {
        Self::new(CellKind::Sql, source)
    }

    pub fn new_markdown(source: impl Into<String>) -> Self {
        Self::new(CellKind::Markdown, source)
    }

    fn new(kind: CellKind, source: impl Into<String>) -> Self {
        Self {
            id: generate_cell_id(),
            kind,
            source: source.into(),
            magic_args: String::new(),
            outputs: Vec::new(),
            execution_count: None,
            extra_metadata: Map::new(),
        }
    }
}

/// An nbformat 4.5 cell id: 16 hex digits of the clock plus a process-wide counter, so ids stay
/// distinct even when generated within the same clock tick.
fn generate_cell_id() -> String {
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

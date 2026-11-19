use rustgrid_core::QueryResult;

use crate::CellOutput;

/// Converts query results into cell outputs, one per result and in order.
///
/// A result with a result set becomes a `Table` holding at most `max_rows` rows rendered with
/// `CellValue::as_display`; `truncated` is set when rows were dropped. `max_rows == 0` is a
/// literal limit, not "unlimited": no rows are kept and `truncated` is true if any existed.
/// A statement without a result set becomes `Affected`.
pub fn results_to_outputs(results: &[QueryResult], max_rows: usize) -> Vec<CellOutput> {
    results
        .iter()
        .map(|result| {
            if result.has_result_set {
                CellOutput::Table {
                    columns: result.columns.iter().map(|c| c.name.clone()).collect(),
                    rows: result
                        .rows
                        .iter()
                        .take(max_rows)
                        .map(|row| row.iter().map(|v| v.as_display()).collect())
                        .collect(),
                    truncated: result.rows.len() > max_rows,
                }
            } else {
                CellOutput::Affected {
                    rows: result.rows_affected,
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use rustgrid_core::{CellValue, ColumnInfo};

    use super::*;

    fn col(name: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.to_string(),
            data_type: String::new(),
            nullable: true,
            primary_key: false,
            auto_increment: false,
            generated: false,
            comment: String::new(),
        }
    }

    fn select(cols: &[&str], rows: Vec<Vec<CellValue>>) -> QueryResult {
        QueryResult {
            statement: "SELECT".into(),
            columns: cols.iter().map(|c| col(c)).collect(),
            rows,
            rows_affected: 0,
            has_result_set: true,
            last_insert_id: None,
        }
    }

    fn dml(n: u64) -> QueryResult {
        QueryResult {
            statement: "UPDATE".into(),
            columns: vec![],
            rows: vec![],
            rows_affected: n,
            has_result_set: false,
            last_insert_id: None,
        }
    }

    fn ints(n: i64) -> Vec<Vec<CellValue>> {
        (0..n).map(|i| vec![CellValue::Int(i)]).collect()
    }

    fn table(out: &CellOutput) -> (&Vec<String>, &Vec<Vec<String>>, bool) {
        match out {
            CellOutput::Table {
                columns,
                rows,
                truncated,
            } => (columns, rows, *truncated),
            other => panic!("not a table: {other:?}"),
        }
    }

    #[test]
    fn boundary_is_not_truncated() {
        let out = results_to_outputs(&[select(&["a"], ints(3))], 3);
        let (cols, rows, truncated) = table(&out[0]);
        assert_eq!(cols, &vec!["a".to_string()]);
        assert_eq!(rows.len(), 3);
        assert!(!truncated);
    }

    #[test]
    fn over_limit_is_truncated() {
        let out = results_to_outputs(&[select(&["a"], ints(4))], 3);
        let (_, rows, truncated) = table(&out[0]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2], vec!["2".to_string()]);
        assert!(truncated);
    }

    #[test]
    fn affected_for_non_result_statement() {
        let out = results_to_outputs(&[dml(7)], 10);
        assert!(matches!(out[0], CellOutput::Affected { rows: 7 }));
    }

    #[test]
    fn mixed_results_keep_order() {
        let out = results_to_outputs(&[dml(1), select(&["a"], ints(1)), dml(2)], 10);
        assert_eq!(out.len(), 3);
        assert!(matches!(out[0], CellOutput::Affected { rows: 1 }));
        assert!(matches!(out[1], CellOutput::Table { .. }));
        assert!(matches!(out[2], CellOutput::Affected { rows: 2 }));
    }

    #[test]
    fn empty_result_set_is_a_table() {
        let out = results_to_outputs(&[select(&["a", "b"], vec![])], 10);
        let (cols, rows, truncated) = table(&out[0]);
        assert_eq!(cols.len(), 2);
        assert!(rows.is_empty());
        assert!(!truncated);
    }

    #[test]
    fn null_and_bytes_use_as_display() {
        let rows = vec![vec![
            CellValue::Null,
            CellValue::Bytes(vec![0xde, 0xad]),
            CellValue::Text("x".into()),
        ]];
        let out = results_to_outputs(&[select(&["a", "b", "c"], rows)], 10);
        let (_, rows, _) = table(&out[0]);
        assert_eq!(rows[0], vec!["NULL", "dead", "x"]);
    }

    #[test]
    fn zero_max_rows_keeps_nothing() {
        let out = results_to_outputs(&[select(&["a"], ints(2)), select(&["a"], vec![])], 0);
        let (_, rows, truncated) = table(&out[0]);
        assert!(rows.is_empty());
        assert!(truncated);
        let (_, _, truncated) = table(&out[1]);
        assert!(!truncated);
    }
}

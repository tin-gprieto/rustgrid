use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::model::generate_cell_id;
use crate::{Cell, CellKind, CellOutput, Error, Notebook, NotebookBinding, RawKind, Result};

const SQL_MAGIC: &str = "%%sql";

/// Parses the text of an `.ipynb` file. Anything the model has no field for is kept verbatim
/// (extra metadata, unmapped outputs, unknown top-level keys) so it can be written back.
pub fn read_ipynb(text: &str) -> Result<Notebook> {
    let Value::Object(mut root) = serde_json::from_str::<Value>(text)? else {
        return Err(invalid("the notebook is not a JSON object"));
    };
    let Some(Value::Array(raw_cells)) = root.remove("cells") else {
        return Err(invalid("missing `cells` array"));
    };

    let metadata = match root.remove("metadata") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m,
        Some(_) => return Err(invalid("`metadata` is not an object")),
    };
    // The `rustgrid` object always stays in `extra_metadata`: a malformed one (no
    // `connection_id`) or one with keys beyond the binding's would otherwise be lost on
    // write-back. Writers must therefore overwrite that key from `binding`, not append it.
    let binding = metadata.get("rustgrid").and_then(read_binding);

    let major = root.remove("nbformat").and_then(|v| v.as_u64());
    let minor = root.remove("nbformat_minor").and_then(|v| v.as_u64());
    let format_version = major.map(|m| (m as u32, minor.unwrap_or(0) as u32));

    let mut seen = HashSet::new();
    let cells = raw_cells
        .into_iter()
        .enumerate()
        .map(|(i, cell)| read_cell(i, cell, &mut seen))
        .collect::<Result<Vec<_>>>()?;

    Ok(Notebook {
        cells,
        binding,
        extra_metadata: metadata,
        format_version,
        extra_top_level: root,
    })
}

fn invalid(msg: impl Into<String>) -> Error {
    Error::Invalid(msg.into())
}

fn read_binding(value: &Value) -> Option<NotebookBinding> {
    let obj = value.as_object()?;
    let text = |k: &str| obj.get(k).and_then(Value::as_str).map(str::to_owned);
    Some(NotebookBinding {
        connection_id: text("connection_id")?,
        database: text("database"),
        schema: text("schema"),
    })
}

fn read_cell(index: usize, cell: Value, seen: &mut HashSet<String>) -> Result<Cell> {
    let Value::Object(mut obj) = cell else {
        return Err(invalid(format!("cell {index} is not an object")));
    };
    let cell_type = obj
        .get("cell_type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("cell {index} has no `cell_type`")))?
        .to_owned();
    let source = match obj.remove("source") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s,
        Some(Value::Array(lines)) => lines
            .iter()
            .map(|l| {
                l.as_str()
                    .ok_or_else(|| invalid(format!("cell {index} `source` has a non-string line")))
            })
            .collect::<Result<String>>()?,
        Some(_) => return Err(invalid(format!("cell {index} `source` is not text"))),
    };

    let (kind, raw_kind, source, magic_args) = match cell_type.as_str() {
        "markdown" => (CellKind::Markdown, RawKind::Code, source, String::new()),
        "raw" => (CellKind::Raw, RawKind::Raw, source, String::new()),
        "code" => match split_sql_magic(&source) {
            Some((args, body)) => (CellKind::Sql, RawKind::Code, body, args),
            None => (CellKind::Raw, RawKind::Code, source, String::new()),
        },
        other => return Err(invalid(format!("cell {index} has unknown type `{other}`"))),
    };

    let id = match obj.get("id").and_then(Value::as_str) {
        Some(id) if is_valid_id(id) && !seen.contains(id) => id.to_owned(),
        _ => loop {
            let id = generate_cell_id();
            if !seen.contains(&id) {
                break id;
            }
        },
    };
    seen.insert(id.clone());

    let extra_metadata = match obj.remove("metadata") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m,
        Some(_) => return Err(invalid(format!("cell {index} `metadata` is not an object"))),
    };
    let outputs = match obj.remove("outputs") {
        Some(Value::Array(outs)) if kind != CellKind::Markdown => {
            outs.into_iter().map(CellOutput::Raw).collect()
        }
        _ => Vec::new(),
    };
    let execution_count = obj
        .remove("execution_count")
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok());
    obj.remove("cell_type");
    obj.remove("id");

    Ok(Cell {
        id,
        kind,
        raw_kind,
        source,
        magic_args,
        outputs,
        execution_count,
        extra_metadata,
        extra_fields: obj,
    })
}

/// `(magic_args, body)` when the source's first line is the `%%sql` cell magic.
fn split_sql_magic(source: &str) -> Option<(String, String)> {
    let (first, body) = source.split_once('\n').unwrap_or((source, ""));
    let args = first.strip_prefix(SQL_MAGIC)?;
    if !args.is_empty() && !args.starts_with(char::is_whitespace) {
        return None;
    }
    Some((args.trim().to_owned(), body.to_owned()))
}

fn is_valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CellKind, CellOutput, Error, RawKind};
    use serde_json::json;

    fn nb(cells: serde_json::Value) -> String {
        json!({"cells": cells, "metadata": {}, "nbformat": 4, "nbformat_minor": 5}).to_string()
    }

    fn one(cell: serde_json::Value) -> crate::Cell {
        read_ipynb(&nb(json!([cell]))).unwrap().cells.remove(0)
    }

    #[test]
    fn sql_magic_cell_is_sql_with_args() {
        let c = one(
            json!({"cell_type":"code","id":"a","metadata":{},"outputs":[],
            "execution_count":null,"source":"%%sql --save x  \nselect 1;\nselect 2"}),
        );
        assert_eq!(c.kind, CellKind::Sql);
        assert_eq!(c.magic_args, "--save x");
        assert_eq!(c.source, "select 1;\nselect 2");
        assert_eq!(c.id, "a");
    }

    #[test]
    fn bare_sql_magic_has_empty_source_and_args() {
        let c = one(json!({"cell_type":"code","metadata":{},"outputs":[],"source":"%%sql"}));
        assert_eq!(c.kind, CellKind::Sql);
        assert_eq!(c.magic_args, "");
        assert_eq!(c.source, "");
    }

    #[test]
    fn other_magics_and_python_are_raw_code() {
        for src in ["%load_ext sql", "%sql sqlite://", "print(1)", "%%sqlfoo\nx"] {
            let c = one(json!({"cell_type":"code","metadata":{},"outputs":[],"source":src}));
            assert_eq!(c.kind, CellKind::Raw, "{src}");
            assert_eq!(c.raw_kind, RawKind::Code);
            assert_eq!(c.source, src);
        }
    }

    #[test]
    fn markdown_and_nbformat_raw() {
        let m = one(json!({"cell_type":"markdown","metadata":{},"source":"# t"}));
        assert_eq!(m.kind, CellKind::Markdown);
        let r = one(json!({"cell_type":"raw","metadata":{},"source":"x"}));
        assert_eq!(r.kind, CellKind::Raw);
        assert_eq!(r.raw_kind, RawKind::Raw);
    }

    #[test]
    fn source_accepts_string_or_list() {
        let a = one(json!({"cell_type":"markdown","metadata":{},"source":"a\nb"}));
        let b = one(json!({"cell_type":"markdown","metadata":{},"source":["a\n","b"]}));
        assert_eq!(a.source, b.source);
        let s = one(json!({"cell_type":"code","metadata":{},"outputs":[],
            "source":["%%sql\n","select 1\n","from t"]}));
        assert_eq!(s.source, "select 1\nfrom t");
    }

    #[test]
    fn binding_and_extra_metadata() {
        let text =
            json!({"cells":[{"cell_type":"markdown","metadata":{"tags":["x"],"collapsed":true},
            "source":""}],
            "metadata":{"rustgrid":{"connection_id":"c1","database":"d","schema":null},
                "kernelspec":{"name":"python3"}},
            "nbformat":4,"nbformat_minor":5,"extra_key":1})
            .to_string();
        let n = read_ipynb(&text).unwrap();
        let b = n.binding.unwrap();
        assert_eq!(b.connection_id, "c1");
        assert_eq!(b.database.as_deref(), Some("d"));
        assert_eq!(b.schema, None);
        assert!(n.extra_metadata.contains_key("kernelspec"));
        assert_eq!(n.format_version, Some((4, 5)));
        assert_eq!(n.extra_top_level["extra_key"], 1);
        assert_eq!(n.cells[0].extra_metadata["tags"], json!(["x"]));
    }

    #[test]
    fn outputs_are_preserved_raw_and_execution_count_read() {
        let out = json!({"output_type":"execute_result","execution_count":3,"metadata":{},
            "data":{"text/plain":"x"}});
        let c = one(
            json!({"cell_type":"code","metadata":{},"outputs":[out.clone()],
            "execution_count":3,"source":"%%sql\nselect 1"}),
        );
        assert_eq!(c.outputs, vec![CellOutput::Raw(out)]);
        assert_eq!(c.execution_count, Some(3));
    }

    #[test]
    fn missing_or_duplicate_ids_are_generated() {
        let n = read_ipynb(&nb(json!([
            {"cell_type":"markdown","metadata":{},"source":""},
            {"cell_type":"markdown","metadata":{},"id":"dup","source":""},
            {"cell_type":"markdown","metadata":{},"id":"dup","source":""},
            {"cell_type":"markdown","metadata":{},"id":"bad id!","source":""}
        ])))
        .unwrap();
        let ids: std::collections::HashSet<_> = n.cells.iter().map(|c| c.id.clone()).collect();
        assert_eq!(ids.len(), 4);
        assert_eq!(n.cells[1].id, "dup");
    }

    #[test]
    fn unknown_cell_keys_are_preserved() {
        let att = json!({"img.png": {"image/png": "iVBORw0KGgo="}});
        let c = one(
            json!({"cell_type":"markdown","id":"m","metadata":{},"source":"x",
            "attachments": att.clone()}),
        );
        assert_eq!(c.extra_fields.len(), 1);
        assert_eq!(c.extra_fields["attachments"], att);
    }

    #[test]
    fn malformed_rustgrid_metadata_stays_in_extra_metadata() {
        let text = json!({"cells":[],"metadata":{"rustgrid":{"database":"d"}},
            "nbformat":4,"nbformat_minor":5})
        .to_string();
        let n = read_ipynb(&text).unwrap();
        assert!(n.binding.is_none());
        assert_eq!(n.extra_metadata["rustgrid"], json!({"database":"d"}));
    }

    #[test]
    fn rustgrid_metadata_with_unknown_keys_is_kept_too() {
        let text = json!({"cells":[],"metadata":{"rustgrid":{"connection_id":"c","x":1}},
            "nbformat":4,"nbformat_minor":5})
        .to_string();
        let n = read_ipynb(&text).unwrap();
        assert_eq!(n.binding.unwrap().connection_id, "c");
        assert_eq!(n.extra_metadata["rustgrid"]["x"], 1);
    }

    #[test]
    fn errors() {
        assert!(matches!(read_ipynb("{nope"), Err(Error::Json(_))));
        assert!(matches!(read_ipynb("{}"), Err(Error::Invalid(_))));
        assert!(matches!(read_ipynb("[]"), Err(Error::Invalid(_))));
        assert!(matches!(
            read_ipynb(&nb(json!([{"cell_type":"weird","source":""}]))),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            read_ipynb(&nb(json!([{"cell_type":"code","source":5}]))),
            Err(Error::Invalid(_))
        ));
    }
}

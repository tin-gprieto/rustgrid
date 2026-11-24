use serde_json::{Map, Value, json};

use crate::model::generate_cell_id;
use crate::{Cell, CellKind, CellOutput, Notebook, PARAMETERS_TAG, RawKind};

const LOAD_EXT: &str = "%load_ext sql";
const RELOAD_EXT: &str = "%reload_ext sql";

/// Serializes a notebook as nbformat 4.5 JSON (1-space indent, trailing newline, like Jupyter).
///
/// The input is not mutated: a `%load_ext sql` code cell is added to the output when no code cell
/// loads the extension, so reading the result back may contain one more cell than `nb`.
/// `format_version` is ignored; cells always carry ids, so the output is always 4.5.
pub fn write_ipynb(nb: &Notebook) -> String {
    let mut cells: Vec<Value> = Vec::with_capacity(nb.cells.len() + 1);
    if !nb.cells.iter().any(loads_sql_ext) {
        cells.push(json!({
            "cell_type": "code",
            "id": generate_cell_id(),
            "metadata": {},
            "outputs": [],
            "execution_count": null,
            "source": [LOAD_EXT],
        }));
    }
    cells.extend(nb.cells.iter().map(cell_json));

    let mut root = Map::new();
    root.insert("cells".into(), Value::Array(cells));
    root.insert("metadata".into(), Value::Object(metadata_json(nb)));
    root.insert("nbformat".into(), json!(4));
    root.insert("nbformat_minor".into(), json!(5));
    for (k, v) in &nb.extra_top_level {
        root.entry(k.clone()).or_insert_with(|| v.clone());
    }

    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(
        &mut out,
        serde_json::ser::PrettyFormatter::with_indent(b" "),
    );
    serde::Serialize::serialize(&Value::Object(root), &mut ser).expect("a JSON value serializes");
    out.push(b'\n');
    String::from_utf8(out).expect("serde_json emits UTF-8")
}

fn loads_sql_ext(cell: &Cell) -> bool {
    cell.kind == CellKind::Raw
        && cell.raw_kind == RawKind::Code
        && cell
            .source
            .lines()
            .any(|l| matches!(l.trim(), LOAD_EXT | RELOAD_EXT))
}

fn metadata_json(nb: &Notebook) -> Map<String, Value> {
    let mut meta = nb.extra_metadata.clone();
    if !meta.contains_key("kernelspec") {
        meta.insert(
            "kernelspec".into(),
            json!({"display_name": "Python 3", "language": "python", "name": "python3"}),
        );
    }
    if !meta.contains_key("language_info") {
        meta.insert("language_info".into(), json!({"name": "python"}));
    }
    match &nb.binding {
        Some(b) => {
            let mut rg = match meta.remove("rustgrid") {
                Some(Value::Object(m)) => m,
                _ => Map::new(),
            };
            rg.insert("connection_id".into(), json!(b.connection_id));
            for (key, value) in [("database", &b.database), ("schema", &b.schema)] {
                match value {
                    Some(v) => {
                        rg.insert(key.into(), json!(v));
                    }
                    None => remove_string(&mut rg, key),
                }
            }
            meta.insert("rustgrid".into(), Value::Object(rg));
        }
        // Unbinding must win over a binding read from the file. Only an object the reader
        // would have parsed as a binding (string `connection_id`) is touched; its string
        // connection_id/database/schema are the binding's fields and go, anything else stays.
        None => {
            if let Some(Value::Object(rg)) = meta.get_mut("rustgrid")
                && rg.get("connection_id").is_some_and(Value::is_string)
            {
                for key in ["connection_id", "database", "schema"] {
                    remove_string(rg, key);
                }
                if rg.is_empty() {
                    meta.shift_remove("rustgrid");
                }
            }
        }
    }
    meta
}

/// Removes `key` only when it holds a string: the reader maps nothing else, so other values
/// are unknown data that must survive a rewrite.
fn remove_string(map: &mut Map<String, Value>, key: &str) {
    if map.get(key).is_some_and(Value::is_string) {
        map.shift_remove(key);
    }
}

fn cell_json(cell: &Cell) -> Value {
    let mut obj = cell.extra_fields.clone();
    let (cell_type, source) = match cell.kind {
        CellKind::Markdown => ("markdown", cell.source.clone()),
        CellKind::Raw if cell.raw_kind == RawKind::Raw => ("raw", cell.source.clone()),
        CellKind::Raw | CellKind::Parameters => ("code", cell.source.clone()),
        CellKind::Sql => {
            let mut s = String::from("%%sql");
            if !cell.magic_args.is_empty() {
                s.push(' ');
                s.push_str(&cell.magic_args);
            }
            s.push('\n');
            s.push_str(&cell.source);
            ("code", s)
        }
    };
    obj.insert("cell_type".into(), json!(cell_type));
    obj.insert("id".into(), json!(cell.id));
    let mut metadata = cell.extra_metadata.clone();
    if cell.kind == CellKind::Parameters {
        tag_parameters(&mut metadata);
    }
    obj.insert("metadata".into(), Value::Object(metadata));
    obj.insert("source".into(), source_lines(&source));
    if cell_type == "code" {
        let outputs = cell
            .outputs
            .iter()
            .map(|o| output_json(o, cell.execution_count))
            .collect();
        obj.insert("outputs".into(), Value::Array(outputs));
        obj.insert("execution_count".into(), json!(cell.execution_count));
    }
    Value::Object(obj)
}

/// Add papermill's `parameters` tag to a cell's `metadata.tags`, keeping its other tags (and
/// replacing a `tags` value that is not a list, which Jupyter would reject anyway).
fn tag_parameters(metadata: &mut Map<String, Value>) {
    let tag = json!(PARAMETERS_TAG);
    match metadata.get_mut("tags") {
        Some(Value::Array(tags)) if tags.contains(&tag) => {}
        Some(Value::Array(tags)) => tags.push(tag),
        _ => {
            metadata.insert("tags".into(), json!([PARAMETERS_TAG]));
        }
    }
}

/// Jupyter style: each line keeps its `\n`, so joining the array restores the text.
fn source_lines(source: &str) -> Value {
    Value::Array(source.split_inclusive('\n').map(|l| json!(l)).collect())
}

fn output_json(output: &CellOutput, execution_count: Option<u32>) -> Value {
    match output {
        CellOutput::Raw(v) => v.clone(),
        CellOutput::Table {
            columns,
            rows,
            truncated,
        } => json!({
            "output_type": "execute_result",
            "execution_count": execution_count,
            "metadata": {},
            "data": {
                "text/html": html_table(columns, rows, *truncated),
                "text/plain": text_table(columns, rows, *truncated),
            },
        }),
        CellOutput::Affected { rows } => json!({
            "output_type": "execute_result",
            "execution_count": execution_count,
            "metadata": {},
            "data": {"text/plain": format!("{rows} rows affected.")},
        }),
        CellOutput::Error { message } => json!({
            "output_type": "error",
            "ename": "SQLError",
            "evalue": message,
            "traceback": [message],
        }),
    }
}

const TRUNCATED_NOTE: &str = "\u{2026} truncated";

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn html_table(columns: &[String], rows: &[Vec<String>], truncated: bool) -> String {
    let mut s = String::from("<table>\n<thead>\n<tr>");
    for c in columns {
        s.push_str(&format!("<th>{}</th>", html_escape(c)));
    }
    s.push_str("</tr>\n</thead>\n<tbody>\n");
    for row in rows {
        s.push_str("<tr>");
        for v in row {
            s.push_str(&format!("<td>{}</td>", html_escape(v)));
        }
        s.push_str("</tr>\n");
    }
    if truncated {
        let span = columns.len().max(1);
        s.push_str(&format!(
            "<tr><td colspan=\"{span}\">{TRUNCATED_NOTE}</td></tr>\n"
        ));
    }
    s.push_str("</tbody>\n</table>");
    s
}

fn text_table(columns: &[String], rows: &[Vec<String>], truncated: bool) -> String {
    let width = |i: usize| {
        std::iter::once(columns.get(i))
            .chain(rows.iter().map(|r| r.get(i)))
            .flatten()
            .map(|s| s.chars().count())
            .max()
            .unwrap_or(0)
    };
    let widths: Vec<usize> = (0..columns.len()).map(width).collect();
    let line = |cells: &[String]| {
        let parts: Vec<String> = widths
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let v = cells.get(i).map_or("", String::as_str);
                format!("{v:<w$}")
            })
            .collect();
        parts.join(" | ").trim_end().to_owned()
    };
    let mut lines = vec![line(columns)];
    lines.push(
        widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("-+-"),
    );
    lines.extend(rows.iter().map(|r| line(r)));
    if truncated {
        lines.push(TRUNCATED_NOTE.to_owned());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NotebookBinding, read_ipynb};

    fn parse(nb: &Notebook) -> Value {
        serde_json::from_str(&write_ipynb(nb)).unwrap()
    }

    fn raw_code(src: &str) -> Cell {
        let mut c = Cell::new_markdown(src);
        c.kind = CellKind::Raw;
        c.raw_kind = RawKind::Code;
        c
    }

    fn nb_with(cells: Vec<Cell>) -> Notebook {
        Notebook {
            cells,
            ..Default::default()
        }
    }

    #[test]
    fn parameters_cell_round_trips_with_its_tag() {
        let mut params = Cell::new_parameters();
        params.source = "country = \"AR\"\nyear = 2025".into();
        params.extra_metadata.insert("tags".into(), json!(["keep"]));
        let out = parse(&nb_with(vec![params.clone()]));
        let cell = &out["cells"][1];
        assert_eq!(cell["cell_type"], "code");
        assert_eq!(cell["metadata"]["tags"], json!(["keep", "parameters"]));
        assert_eq!(cell["source"], json!(["country = \"AR\"\n", "year = 2025"]));

        let back = read_ipynb(&write_ipynb(&nb_with(vec![params]))).unwrap();
        let back = &back.cells[1];
        assert_eq!(back.kind, CellKind::Parameters);
        assert_eq!(back.source, "country = \"AR\"\nyear = 2025");
        assert_eq!(back.extra_metadata["tags"], json!(["keep", "parameters"]));
        // Writing it again does not add a second tag.
        let again = parse(&nb_with(vec![back.clone()]));
        assert_eq!(
            again["cells"][1]["metadata"]["tags"],
            json!(["keep", "parameters"])
        );
    }

    #[test]
    fn header_format_and_kernelspec() {
        let text = write_ipynb(&nb_with(vec![]));
        assert!(text.ends_with("}\n"));
        assert!(text.contains("\n \"cells\""), "1-space indent: {text}");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["nbformat"], 4);
        assert_eq!(v["nbformat_minor"], 5);
        assert_eq!(v["metadata"]["kernelspec"]["name"], "python3");
        assert_eq!(v["metadata"]["language_info"]["name"], "python");
        assert!(v["metadata"].get("rustgrid").is_none());
    }

    /// The written file carries the connection by id only: no host, user or password can reach
    /// it, because the binding is all the writer is given.
    #[test]
    fn binding_writes_only_the_connection_reference() {
        let mut nb = nb_with(vec![Cell::new_sql("select 1")]);
        nb.binding = Some(NotebookBinding {
            connection_id: "conn-1".into(),
            database: Some("shop".into()),
            schema: None,
        });
        let v = parse(&nb);
        let rg = v["metadata"]["rustgrid"].as_object().unwrap();
        for key in rg.keys() {
            assert!(
                ["connection_id", "database", "schema"].contains(&key.as_str()),
                "unexpected key {key}"
            );
        }
        assert_eq!(rg["connection_id"], "conn-1");
        let text = write_ipynb(&nb).to_lowercase();
        for secret in ["password", "passwd", "secret", "\"host\""] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }

    #[test]
    fn load_ext_inserted_when_absent_and_not_duplicated() {
        let v = parse(&nb_with(vec![Cell::new_sql("select 1")]));
        assert_eq!(v["cells"].as_array().unwrap().len(), 2);
        assert_eq!(v["cells"][0]["cell_type"], "code");
        assert_eq!(v["cells"][0]["source"], json!(["%load_ext sql"]));

        let v = parse(&nb_with(vec![
            raw_code("import os\n%load_ext sql"),
            Cell::new_sql("select 1"),
        ]));
        assert_eq!(v["cells"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn rustgrid_binding_merges_into_stale_object() {
        let mut nb = nb_with(vec![]);
        nb.extra_metadata.insert(
            "rustgrid".into(),
            json!({"connection_id":"old","database":"x","extra":1}),
        );
        nb.binding = Some(NotebookBinding {
            connection_id: "new".into(),
            database: None,
            schema: None,
        });
        let v = parse(&nb);
        assert_eq!(
            v["metadata"]["rustgrid"],
            json!({"connection_id":"new","extra":1})
        );
        assert!(nb.extra_metadata["rustgrid"]["database"] == "x");
    }

    #[test]
    fn no_binding_keeps_existing_rustgrid() {
        let mut nb = nb_with(vec![]);
        nb.extra_metadata
            .insert("rustgrid".into(), json!({"database":"d"}));
        assert_eq!(parse(&nb)["metadata"]["rustgrid"], json!({"database":"d"}));
    }

    #[test]
    fn unbinding_a_read_notebook_drops_the_stale_binding() {
        let mut nb = read_ipynb(include_str!("../tests/fixtures/jupysql_basic.ipynb")).unwrap();
        assert!(nb.binding.is_some());
        nb.binding = None;
        let back = read_ipynb(&write_ipynb(&nb)).unwrap();
        assert!(back.binding.is_none());
    }

    #[test]
    fn unbinding_keeps_unknown_rustgrid_data_and_string_free_object_is_dropped() {
        let mut nb = nb_with(vec![]);
        nb.extra_metadata.insert(
            "rustgrid".into(),
            json!({"connection_id":"c","database":"d","schema":"s","extra":1}),
        );
        assert_eq!(parse(&nb)["metadata"]["rustgrid"], json!({"extra":1}));

        let mut nb = nb_with(vec![]);
        nb.extra_metadata.insert(
            "rustgrid".into(),
            json!({"connection_id":"c","database":"d"}),
        );
        assert!(parse(&nb)["metadata"].get("rustgrid").is_none());
    }

    #[test]
    fn non_string_binding_field_survives_when_unset() {
        let mut nb = nb_with(vec![]);
        nb.extra_metadata.insert(
            "rustgrid".into(),
            json!({"connection_id":"old","database":5,"schema":null}),
        );
        nb.binding = Some(NotebookBinding {
            connection_id: "new".into(),
            database: None,
            schema: None,
        });
        assert_eq!(
            parse(&nb)["metadata"]["rustgrid"],
            json!({"connection_id":"new","database":5,"schema":null})
        );
    }

    #[test]
    fn reload_ext_counts_as_loading_the_extension() {
        let v = parse(&nb_with(vec![raw_code("%reload_ext sql")]));
        assert_eq!(v["cells"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn sql_cell_with_magic_args_and_source_lines() {
        let mut c = Cell::new_sql("select 1;\nselect 2");
        c.magic_args = "--save x".into();
        let v = parse(&nb_with(vec![c]));
        let cell = &v["cells"][1];
        assert_eq!(
            cell["source"],
            json!(["%%sql --save x\n", "select 1;\n", "select 2"])
        );
        assert_eq!(cell["execution_count"], Value::Null);
        assert_eq!(cell["outputs"], json!([]));
    }

    #[test]
    fn empty_source_and_markdown_and_nbformat_raw() {
        let mut raw = raw_code("x");
        raw.kind = CellKind::Raw;
        raw.raw_kind = RawKind::Raw;
        let v = parse(&nb_with(vec![
            raw_code("%load_ext sql"),
            Cell::new_markdown(""),
            raw,
        ]));
        assert_eq!(v["cells"][1]["cell_type"], "markdown");
        assert_eq!(v["cells"][1]["source"], json!([]));
        assert!(v["cells"][1].get("outputs").is_none());
        assert_eq!(v["cells"][2]["cell_type"], "raw");
        assert!(v["cells"][2].get("outputs").is_none());
    }

    #[test]
    fn output_shapes() {
        let mut c = Cell::new_sql("select 1");
        c.execution_count = Some(7);
        c.outputs = vec![
            CellOutput::Table {
                columns: vec!["a".into(), "bb".into()],
                rows: vec![vec!["1".into(), "<x>".into()]],
                truncated: true,
            },
            CellOutput::Affected { rows: 3 },
            CellOutput::Error {
                message: "boom".into(),
            },
            CellOutput::Raw(json!({"output_type":"stream","name":"stdout","text":"hi"})),
        ];
        let v = parse(&nb_with(vec![c]));
        let outs = &v["cells"][1]["outputs"];
        assert_eq!(outs[0]["output_type"], "execute_result");
        assert_eq!(outs[0]["execution_count"], 7);
        assert_eq!(outs[0]["metadata"], json!({}));
        let html = outs[0]["data"]["text/html"].as_str().unwrap();
        assert!(html.contains("<th>bb</th>") && html.contains("<td>&lt;x&gt;</td>"));
        assert!(html.contains("truncated"));
        let plain = outs[0]["data"]["text/plain"].as_str().unwrap();
        assert!(
            plain.contains("a | bb") || plain.contains("a  | bb"),
            "{plain}"
        );
        assert!(plain.contains("truncated"));
        assert_eq!(outs[1]["data"]["text/plain"], "3 rows affected.");
        assert_eq!(outs[2]["output_type"], "error");
        assert_eq!(outs[2]["ename"], "SQLError");
        assert_eq!(outs[2]["evalue"], "boom");
        assert_eq!(outs[2]["traceback"], json!(["boom"]));
        assert_eq!(outs[3]["name"], "stdout");
    }

    #[test]
    fn html_escapes_special_characters() {
        let mut c = Cell::new_sql("x");
        c.outputs = vec![CellOutput::Table {
            columns: vec!["<h&>".into()],
            rows: vec![vec!["a\"b'c".into()]],
            truncated: false,
        }];
        let v = parse(&nb_with(vec![c]));
        let html = v["cells"][1]["outputs"][0]["data"]["text/html"]
            .as_str()
            .unwrap();
        assert!(html.contains("<th>&lt;h&amp;&gt;</th>"), "{html}");
        assert!(html.contains("<td>a&quot;b&#39;c</td>"), "{html}");
    }

    #[test]
    fn written_text_reads_back() {
        let mut nb = nb_with(vec![Cell::new_sql("select 1")]);
        nb.binding = Some(NotebookBinding {
            connection_id: "c".into(),
            database: Some("d".into()),
            schema: None,
        });
        let back = read_ipynb(&write_ipynb(&nb)).unwrap();
        assert_eq!(back.binding, nb.binding);
        assert_eq!(back.cells[1].source, "select 1");
        assert_eq!(back.cells[1].id, nb.cells[0].id);
    }
}

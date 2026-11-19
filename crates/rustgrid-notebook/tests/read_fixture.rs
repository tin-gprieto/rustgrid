use rustgrid_notebook::{CellKind, CellOutput, RawKind, read_ipynb};

#[test]
fn reads_jupysql_fixture() {
    let text = include_str!("fixtures/jupysql_basic.ipynb");
    let nb = read_ipynb(text).unwrap();

    let kinds: Vec<_> = nb.cells.iter().map(|c| c.kind).collect();
    use CellKind::*;
    assert_eq!(kinds, [Raw, Raw, Markdown, Sql, Sql, Sql, Raw, Raw]);
    assert_eq!(nb.cells[6].raw_kind, RawKind::Code);
    assert_eq!(nb.cells[7].raw_kind, RawKind::Raw);

    assert_eq!(nb.cells[3].source, "select id, name\nfrom users");
    assert_eq!(nb.cells[4].magic_args, "--save top_users");
    assert_eq!(nb.cells[3].execution_count, Some(3));
    assert_eq!(nb.cells[3].extra_metadata["tags"][0], "report");
    assert!(matches!(nb.cells[3].outputs[0], CellOutput::Raw(_)));
    assert_eq!(nb.cells[2].source, "# Users report\nCounts per country.");
    assert!(nb.cells[2].extra_metadata.contains_key("custom_cell_flag"));

    assert_eq!(
        nb.cells[2].extra_fields["attachments"]["img.png"]["image/png"],
        "iVBORw0KGgo="
    );

    let b = nb.binding.unwrap();
    assert_eq!(b.connection_id, "conn-1");
    assert_eq!(b.database.as_deref(), Some("main"));
    assert!(nb.extra_metadata.contains_key("kernelspec"));
    assert!(nb.extra_metadata.contains_key("custom_notebook_flag"));
    assert_eq!(nb.format_version, Some((4, 5)));
}

use rustgrid_notebook::{read_ipynb, write_ipynb};

#[test]
fn fixture_round_trips_losslessly_and_idempotently() {
    let text = include_str!("fixtures/jupysql_basic.ipynb");
    let first = read_ipynb(text).unwrap();
    let written = write_ipynb(&first);
    let second = read_ipynb(&written).unwrap();
    assert_eq!(first, second);
    assert_eq!(write_ipynb(&second), written);
}

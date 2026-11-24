//! SQL notebooks: an engine-agnostic notebook model that maps onto Jupyter's `.ipynb`.
//!
//! No GPUI or driver dependencies live here.

mod ipynb;
mod model;
mod outputs;
mod sections;
mod write;

pub use ipynb::read_ipynb;
pub use model::{Cell, CellKind, CellOutput, Notebook, NotebookBinding, RawKind};
pub use outputs::results_to_outputs;
pub use sections::{COLLAPSED_KEY, hidden_cells, section_end};
pub use write::write_ipynb;

/// Errors raised while reading or writing a notebook.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid notebook: {0}")]
    Invalid(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

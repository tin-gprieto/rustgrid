//! Collapsible sections, as in Google Colab and JupyterLab: a Markdown cell that starts a heading
//! of level N heads a section running to the next heading of level N or higher (fewer `#`), and
//! folding it hides every cell in between — `###` subsections, text and SQL cells alike.
//!
//! The folded state is stored on the heading cell under JupyterLab's own metadata key, so a file
//! folded in RustGrid opens folded in JupyterLab and the other way round.

use serde_json::Value;

use crate::{Cell, CellKind};

/// JupyterLab's cell-metadata flag for a folded heading.
pub const COLLAPSED_KEY: &str = "jp-MarkdownHeadingCollapsed";

impl Cell {
    /// The level (1–6) of the first ATX heading (`#` … `######`) of a Markdown cell, outside
    /// fenced code blocks; `None` for any other cell.
    pub fn heading_level(&self) -> Option<u8> {
        if self.kind != CellKind::Markdown {
            return None;
        }
        let mut fence: Option<&str> = None;
        for line in self.source.lines() {
            let trimmed = line.trim_start();
            // Up to three spaces of indentation; more is an indented code block.
            if line.len() - trimmed.len() > 3 {
                continue;
            }
            if let Some(open) = fence {
                if trimmed.starts_with(open) {
                    fence = None;
                }
                continue;
            }
            if trimmed.starts_with("```") {
                fence = Some("```");
                continue;
            }
            if trimmed.starts_with("~~~") {
                fence = Some("~~~");
                continue;
            }
            let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
            if (1..=6).contains(&hashes) {
                let rest = &trimmed[hashes..];
                if rest.is_empty() || rest.starts_with([' ', '\t']) {
                    return Some(hashes as u8);
                }
            }
        }
        None
    }

    /// Whether this heading cell is folded. Only a heading can be.
    pub fn is_collapsed(&self) -> bool {
        self.heading_level().is_some()
            && self.extra_metadata.get(COLLAPSED_KEY) == Some(&Value::Bool(true))
    }

    /// Fold or unfold this heading cell. Unfolding removes the key, so an unfolded file carries
    /// none of it (as JupyterLab writes it).
    pub fn set_collapsed(&mut self, collapsed: bool) {
        if collapsed {
            self.extra_metadata
                .insert(COLLAPSED_KEY.into(), Value::Bool(true));
        } else {
            self.extra_metadata.remove(COLLAPSED_KEY);
        }
    }
}

/// The index just past the section headed by the heading cell at `index`: the next heading of
/// the same or a higher level, or the end. A non-heading cell's "section" is just itself.
pub fn section_end<'a>(cells: impl IntoIterator<Item = &'a Cell>, index: usize) -> usize {
    let cells: Vec<&Cell> = cells.into_iter().collect();
    let Some(level) = cells.get(index).and_then(|cell| cell.heading_level()) else {
        return (index + 1).min(cells.len());
    };
    cells
        .iter()
        .enumerate()
        .skip(index + 1)
        .find(|(_, cell)| cell.heading_level().is_some_and(|next| next <= level))
        .map(|(position, _)| position)
        .unwrap_or(cells.len())
}

/// Which cells a folded heading hides, one flag per cell. A folded section nested in another
/// folded section changes nothing; a heading of the outer level or higher ends the fold.
pub fn hidden_cells<'a>(cells: impl IntoIterator<Item = &'a Cell>) -> Vec<bool> {
    let mut hidden = Vec::new();
    // The level of the outermost fold in effect.
    let mut fold: Option<u8> = None;
    for cell in cells {
        let level = cell.heading_level();
        if let (Some(folded), Some(level)) = (fold, level)
            && level <= folded
        {
            fold = None;
        }
        hidden.push(fold.is_some());
        if fold.is_none() && cell.is_collapsed() {
            fold = level;
        }
    }
    hidden
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(source: &str) -> Cell {
        Cell::new_markdown(source)
    }

    fn folded(source: &str) -> Cell {
        let mut cell = md(source);
        cell.set_collapsed(true);
        cell
    }

    #[test]
    fn heading_level_reads_the_first_atx_heading_outside_code() {
        assert_eq!(md("## Results\ntext").heading_level(), Some(2));
        assert_eq!(md("intro\n\n### Later").heading_level(), Some(3));
        assert_eq!(
            md("```\n# comment\n```\n#### Real").heading_level(),
            Some(4)
        );
        assert_eq!(md("#hashtag").heading_level(), None);
        assert_eq!(md("####### seven").heading_level(), None);
        assert_eq!(md("    # indented code").heading_level(), None);
        assert_eq!(Cell::new_sql("# not markdown").heading_level(), None);
    }

    #[test]
    fn a_folded_heading_hides_lower_levels_until_its_own_level() {
        let cells = vec![
            folded("## A"),
            md("### A.1"),
            Cell::new_sql("select 1"),
            md("#### A.1.a"),
            md("## B"),
            md("text"),
        ];
        assert_eq!(
            hidden_cells(&cells),
            vec![false, true, true, true, false, false]
        );
        assert_eq!(section_end(&cells, 0), 4);
        assert_eq!(section_end(&cells, 1), 4);
        assert_eq!(section_end(&cells, 2), 3);
    }

    #[test]
    fn a_higher_heading_ends_the_fold_and_nested_folds_are_inert() {
        let cells = vec![
            folded("### Sub"),
            folded("#### Deeper"),
            md("text"),
            md("# Top"),
            md("text"),
        ];
        assert_eq!(hidden_cells(&cells), vec![false, true, true, false, false]);
    }

    #[test]
    fn only_a_heading_reports_folded_and_unfolding_drops_the_key() {
        let mut cell = folded("plain text");
        assert!(!cell.is_collapsed());
        cell.source = "# Now a heading".into();
        assert!(cell.is_collapsed());
        cell.set_collapsed(false);
        assert!(cell.extra_metadata.get(COLLAPSED_KEY).is_none());
    }
}

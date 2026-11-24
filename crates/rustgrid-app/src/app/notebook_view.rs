//! The notebook tab: a toolbar and the notebook's cells as a vertical, scrolling list.
//!
//! The tab's state and execution live on `AppView` (`notebooks`, `notebook.rs`), like the query
//! tab's, so the view is drawn by an `AppView` method rather than a child entity.

use rustgrid_notebook::{CellKind, CellOutput};

use super::*;

impl AppView {
    pub(super) fn render_notebook_view(
        &mut self,
        index: usize,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let Some(tab) = self.notebooks.get(index) else {
            return div().into_any_element();
        };
        let tab_id = tab.id;
        let selected = tab.selected;
        let editor_font = self.editor_font();

        let toolbar = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_0p5()
            .px_1()
            .py_0p5()
            .flex_none()
            .bg(rgb(theme.toolbar_bg))
            .border_b_1()
            .border_color(rgb(theme.border))
            .child(self.query_tool_button(
                "notebook-add-sql",
                "icons/plus.svg",
                t!("notebook.add_sql").to_string(),
                theme.text,
                theme.text,
                true,
                cx.listener(move |this, _event, _window, cx| {
                    this.add_notebook_cell(index, CellKind::Sql, cx)
                }),
            ))
            .child(self.query_tool_button(
                "notebook-add-markdown",
                "icons/plus.svg",
                t!("notebook.add_markdown").to_string(),
                theme.text,
                theme.text,
                true,
                cx.listener(move |this, _event, _window, cx| {
                    this.add_notebook_cell(index, CellKind::Markdown, cx)
                }),
            ));

        let mut list = div()
            .id(SharedString::from(format!("notebook-cells-{tab_id}")))
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .gap_2()
            .p_3()
            .overflow_y_scroll()
            .bg(rgb(theme.editor_bg));
        if tab.cells.is_empty() {
            list = list.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(theme.text_muted))
                    .child(t!("notebook.empty").to_string()),
            );
        }
        for (cell_index, state) in tab.cells.iter().enumerate() {
            let cell = &state.cell;
            let kind = match cell.kind {
                CellKind::Sql => t!("notebook.cell.sql"),
                CellKind::Markdown => t!("notebook.cell.markdown"),
                CellKind::Raw => t!("notebook.cell.raw"),
            }
            .to_string();
            let count = match (cell.kind, cell.execution_count) {
                (CellKind::Sql, Some(count)) => format!("[{count}]"),
                (CellKind::Sql, None) => "[ ]".to_string(),
                _ => String::new(),
            };
            let source = if cell.source.is_empty() {
                div()
                    .text_color(rgb(theme.text_muted))
                    .child(t!("notebook.cell.empty").to_string())
            } else {
                div().text_color(rgb(theme.text)).child(cell.source.clone())
            };
            let is_selected = cell_index == selected;
            let mut card = div()
                .id(SharedString::from(format!(
                    "notebook-{tab_id}-cell-{}",
                    cell.id
                )))
                .flex()
                .flex_col()
                .flex_none()
                .gap_1()
                .p_2()
                .rounded(px(6.0))
                .border_1()
                .border_color(rgb(if is_selected {
                    theme.brand
                } else {
                    theme.border
                }))
                .bg(rgb(theme.window_bg))
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.select_notebook_cell(index, cell_index, cx)
                }))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .text_size(px(11.0))
                        .text_color(rgb(theme.text_muted))
                        .child(div().font_family(editor_font.clone()).child(count))
                        .child(kind),
                )
                .child(
                    source
                        .when(cell.kind != CellKind::Markdown, |source| {
                            source.font_family(editor_font.clone())
                        })
                        .text_size(px(12.5)),
                );
            for output in &cell.outputs {
                card = card.child(render_output_summary(output, theme));
            }
            list = list.child(card);
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .child(toolbar)
            .child(list)
            .into_any_element()
    }
}

/// One line describing a cell output. The result grids replace this for live results in the
/// execution sub-stage; outputs loaded from a file keep this summary.
fn render_output_summary(output: &CellOutput, theme: Theme) -> Div {
    let (text, color) = match output {
        CellOutput::Table {
            columns,
            rows,
            truncated,
        } => {
            let mut text = t!(
                "notebook.output.table",
                rows = rows.len(),
                columns = columns.len()
            )
            .to_string();
            if *truncated {
                text.push(' ');
                text.push_str(&t!("notebook.output.truncated"));
            }
            (text, theme.text_muted)
        }
        CellOutput::Affected { rows } => (
            t!("notebook.output.affected", rows = rows).to_string(),
            theme.text_muted,
        ),
        CellOutput::Error { message } => (message.clone(), theme.danger),
        CellOutput::Raw(_) => (t!("notebook.output.raw").to_string(), theme.text_muted),
    };
    div()
        .text_size(px(11.5))
        .text_color(rgb(color))
        .border_t_1()
        .border_color(rgb(theme.border))
        .pt_1()
        .child(text)
}

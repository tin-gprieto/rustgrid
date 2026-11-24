//! The notebook tab: a toolbar and the notebook's cells as a vertical, scrolling list.
//!
//! The tab's state and execution live on `AppView` (`notebooks`, `notebook.rs`), like the query
//! tab's, so the view is drawn by an `AppView` method rather than a child entity. An SQL cell is a
//! real [`ui::SqlEditor`] (created on first render, kept in `AppView::notebook_editors`), with its
//! result grids under it.

use gpui_kit::component::text::TextView;
use rustgrid_notebook::{CellKind, CellOutput};

use super::*;

/// The editor shows between this many and [`EDITOR_MAX_LINES`] lines; longer cells scroll.
const EDITOR_MIN_LINES: usize = 3;
const EDITOR_MAX_LINES: usize = 20;
/// A result grid shows up to this many rows before it scrolls.
const GRID_MAX_VISIBLE_ROWS: usize = 10;
/// The grid's header, footer and horizontal scrollbar around its rows.
const GRID_CHROME_HEIGHT: f32 = 64.0;

/// What one cell needs to draw, copied out of the tab so editors (which need `&mut self`) can be
/// created while the cells are drawn.
struct CellView {
    index: usize,
    id: String,
    kind: CellKind,
    source: String,
    count: Option<u32>,
    elapsed: Option<std::time::Duration>,
    running: bool,
    /// Markdown: shown as its editor rather than rendered.
    editing: bool,
    is_last: bool,
    /// Per output: its result grid when the last run produced one, otherwise a summary line.
    outputs: Vec<(Option<u64>, String, u32)>,
}

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
        let running = tab.running.is_some();
        let connected = tab
            .connection_index
            .and_then(|connection| self.connection_arc(connection))
            .is_some();
        let selected_is_sql = tab
            .cells
            .get(selected)
            .is_some_and(|state| state.cell.kind == CellKind::Sql);
        let has_session = self.notebook_sessions.contains_key(&tab_id);
        let cell_count = tab.cells.len();
        let cells: Vec<CellView> = tab
            .cells
            .iter()
            .enumerate()
            .map(|(cell_index, state)| CellView {
                index: cell_index,
                id: state.cell.id.clone(),
                kind: state.cell.kind,
                source: state.cell.source.clone(),
                count: state.cell.execution_count,
                elapsed: state.elapsed,
                running: tab.running == Some(cell_index),
                editing: state.editing,
                is_last: cell_index + 1 == cell_count,
                outputs: state
                    .cell
                    .outputs
                    .iter()
                    .enumerate()
                    .map(|(output_index, output)| {
                        let (text, color) = output_summary(output, theme);
                        let grid = state.result_grids.get(output_index).copied().flatten();
                        (grid, text, color)
                    })
                    .collect(),
            })
            .collect();
        let empty = cells.is_empty();

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
                "notebook-run-cell",
                "icons/run.svg",
                t!("notebook.run_cell").to_string(),
                theme.text,
                theme.icon_functions,
                connected && !running && selected_is_sql,
                cx.listener(move |this, _event, _window, cx| {
                    this.run_selected_notebook_cell(index, cx)
                }),
            ))
            .child(self.query_tool_button(
                "notebook-run-all",
                "icons/fast-forward.svg",
                t!("notebook.run_all").to_string(),
                theme.text,
                theme.icon_functions,
                connected && !running && !empty,
                cx.listener(move |this, _event, _window, cx| {
                    this.run_all_notebook_cells(index, cx)
                }),
            ))
            .child(self.query_tool_button(
                "notebook-stop",
                "icons/stop.svg",
                t!("query.stop").to_string(),
                theme.text,
                theme.danger,
                running,
                cx.listener(move |this, _event, _window, cx| this.stop_notebook(tab_id, cx)),
            ))
            .child(self.query_tool_button(
                "notebook-restart",
                "icons/rotate-ccw.svg",
                t!("notebook.restart").to_string(),
                theme.text,
                theme.text,
                running || has_session,
                cx.listener(move |this, _event, _window, cx| {
                    this.restart_notebook_session(tab_id, cx)
                }),
            ))
            .child(toolbar_separator(theme))
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
        if empty {
            list = list.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(theme.text_muted))
                    .child(t!("notebook.empty").to_string()),
            );
        }
        for cell in cells {
            let card = self.render_notebook_cell(index, tab_id, cell, selected, cx);
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

    fn render_notebook_cell(
        &mut self,
        index: usize,
        tab_id: u64,
        cell: CellView,
        selected: usize,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        let theme = self.theme;
        let editor_font = self.editor_font();
        let cell_index = cell.index;
        let kind = match cell.kind {
            CellKind::Sql => t!("notebook.cell.sql"),
            CellKind::Markdown => t!("notebook.cell.markdown"),
            CellKind::Raw => t!("notebook.cell.raw"),
        }
        .to_string();
        let count = match (cell.kind, cell.count) {
            (CellKind::Sql, Some(count)) => format!("[{count}]"),
            (CellKind::Sql, None) => "[ ]".to_string(),
            _ => String::new(),
        };
        let status = if cell.running {
            t!("query.running").to_string()
        } else if let Some(elapsed) = cell.elapsed {
            super::query_editor::format_elapsed(elapsed)
        } else {
            String::new()
        };

        let editor_height = |source: &str, font_size: f32| {
            let lines = source
                .lines()
                .count()
                .clamp(EDITOR_MIN_LINES, EDITOR_MAX_LINES);
            px(lines as f32 * font_size * 1.5 + 12.0)
        };
        let show_editor = match cell.kind {
            CellKind::Sql => true,
            CellKind::Markdown => cell.editing,
            CellKind::Raw => false,
        };
        let body: AnyElement = if show_editor {
            match self.ensure_notebook_editor(index, &cell.id, cx) {
                Some(editor) => div()
                    // The notebook's Enter/Escape commands, scoped to this cell (see
                    // `NOTEBOOK_INPUT_CONTEXT`). An unhandled key keeps propagating to the editor.
                    .key_context(NOTEBOOK_CONTEXT)
                    .on_action(cx.listener(move |this, _: &NotebookRunCell, _window, cx| {
                        if this.notebook_key_run(index, cell_index, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .on_action(
                        cx.listener(move |this, _: &NotebookRunAdvance, window, cx| {
                            if this.notebook_key_run_advance(index, cell_index, window, cx) {
                                cx.stop_propagation();
                            }
                        }),
                    )
                    .on_action(cx.listener(move |this, _: &NotebookRunInsert, window, cx| {
                        if this.notebook_key_run_insert(index, cell_index, window, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .on_action(
                        cx.listener(move |this, _: &NotebookLeaveEdit, _window, cx| {
                            if this.notebook_key_leave_edit(index, cell_index, cx) {
                                cx.stop_propagation();
                            }
                        }),
                    )
                    .flex()
                    .flex_col()
                    .h(editor_height(&cell.source, self.editor_font_size()))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(rgb(theme.border))
                    .overflow_hidden()
                    .child(editor)
                    .into_any_element(),
                None => div().into_any_element(),
            }
        } else if cell.kind == CellKind::Markdown {
            // Rendered; a double-click opens the editor.
            let content: AnyElement = if cell.source.trim().is_empty() {
                div()
                    .text_size(px(12.5))
                    .text_color(rgb(theme.text_muted))
                    .child(t!("notebook.cell.empty").to_string())
                    .into_any_element()
            } else {
                TextView::markdown(
                    SharedString::from(format!("notebook-{tab_id}-md-{}", cell.id)),
                    cell.source.clone(),
                )
                .selectable(true)
                .text_size(px(13.0))
                .into_any_element()
            };
            div()
                .id(SharedString::from(format!(
                    "notebook-{tab_id}-md-area-{}",
                    cell.id
                )))
                .on_click(cx.listener(move |this, event: &ClickEvent, _window, cx| {
                    if event.click_count() >= 2 {
                        this.edit_notebook_markdown(index, cell_index, true, cx);
                    }
                }))
                .child(content)
                .into_any_element()
        } else {
            // Raw: a non-SQL code cell (Python, `%load_ext sql`, ...) kept as it is.
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(12.5))
                        .font_family(editor_font.clone())
                        .text_color(rgb(theme.text_muted))
                        .child(cell.source.clone()),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .italic()
                        .text_color(rgb(theme.text_muted))
                        .child(t!("notebook.cell.raw_notice").to_string()),
                )
                .into_any_element()
        };

        // Per-cell actions, on the right of the header.
        let action = |id: &str, icon: &'static str, tip: String, enabled: bool| {
            ui::icon_button(
                SharedString::from(format!("notebook-{tab_id}-{id}-{}", cell.id)),
                icon,
                if enabled {
                    theme.text
                } else {
                    theme.text_muted
                },
                18.0,
                18.0,
                false,
                theme,
                |_, _, _| {},
            )
            .tooltip(ui::text_tooltip(tip))
            .when(!enabled, |button| button.opacity(0.4))
        };
        let mut actions = div().flex().flex_row().items_center().gap_0p5();
        if cell.kind == CellKind::Markdown && cell.editing {
            actions = actions.child(
                action(
                    "done",
                    "icons/check.svg",
                    t!("notebook.cell.done").to_string(),
                    true,
                )
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.edit_notebook_markdown(index, cell_index, false, cx)
                })),
            );
        }
        if cell.kind != CellKind::Raw {
            let (target, label) = if cell.kind == CellKind::Sql {
                (CellKind::Markdown, t!("notebook.cell.to_markdown"))
            } else {
                (CellKind::Sql, t!("notebook.cell.to_sql"))
            };
            actions = actions.child(
                action("kind", "icons/type.svg", label.to_string(), !cell.running).on_click(
                    cx.listener(move |this, _event, _window, cx| {
                        this.set_notebook_cell_kind(index, cell_index, target, cx)
                    }),
                ),
            );
        }
        actions = actions
            .child(
                action(
                    "up",
                    "icons/arrow-up.svg",
                    t!("notebook.cell.move_up").to_string(),
                    cell_index > 0,
                )
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.move_notebook_cell(index, cell_index, true, cx)
                })),
            )
            .child(
                action(
                    "down",
                    "icons/arrow-down.svg",
                    t!("notebook.cell.move_down").to_string(),
                    !cell.is_last,
                )
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.move_notebook_cell(index, cell_index, false, cx)
                })),
            )
            .child(
                action(
                    "delete",
                    "icons/trash.svg",
                    t!("notebook.cell.delete").to_string(),
                    !cell.running,
                )
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.delete_notebook_cell(index, cell_index, cx)
                })),
            );

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
            .border_color(rgb(if cell_index == selected {
                theme.brand
            } else {
                theme.border
            }))
            .bg(rgb(theme.window_bg))
            // Capture phase: a press inside the cell's editor selects the cell too, without taking
            // the press away from the editor.
            .capture_any_mouse_down(cx.listener(move |this, _event, _window, cx| {
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
                    .child(kind)
                    .child(div().flex_1())
                    .child(status)
                    .child(actions),
            )
            .child(body);
        for (grid_id, text, color) in cell.outputs {
            let grid = grid_id.and_then(|id| {
                self.grids
                    .iter()
                    .find(|grid| grid.read(cx).state.id == id)
                    .cloned()
            });
            card = card.child(match grid {
                Some(grid) => {
                    let rows = grid.read(cx).state.rows.len();
                    let height = rows.clamp(1, GRID_MAX_VISIBLE_ROWS) as f32 * GRID_ROW_HEIGHT
                        + GRID_CHROME_HEIGHT;
                    div()
                        .flex()
                        .flex_col()
                        .h(px(height))
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(rgb(theme.border))
                        .overflow_hidden()
                        .child(grid)
                        .into_any_element()
                }
                None => div()
                    .text_size(px(11.5))
                    .text_color(rgb(color))
                    .border_t_1()
                    .border_color(rgb(theme.border))
                    .pt_1()
                    .child(text)
                    .into_any_element(),
            });
        }
        card
    }
}

/// One line describing a cell output and its colour, for outputs without a live grid (statements
/// without a result set, errors, and outputs loaded from a file).
fn output_summary(output: &CellOutput, theme: Theme) -> (String, u32) {
    match output {
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
    }
}

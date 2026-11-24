//! SQL notebooks: running a notebook's cells against its connection.
//!
//! A notebook keeps one pinned [`rustgrid_core::ScriptSession`] (in `AppView::notebook_sessions`,
//! by notebook id) across cell runs, so `USE`/`SET`/`BEGIN` and temporary tables carry from one
//! cell to the next, like a Jupyter kernel. A driver that cannot pin a session runs every
//! statement on the pool instead, with no state shared between cells. The session is taken out of
//! the map while a cell runs and only put back when the run is still current; a run that Stop or
//! Restart made stale drops it, and [`ScriptSessionGuard`] releases it on the Tokio runtime.

use super::query::{StatementOutcome, run_statements, split_for_run};
use std::rc::Rc;

use super::runsql::ScriptSessionGuard;
use super::*;

/// How many rows of each result set a cell keeps in its outputs (and so writes to the file).
const NOTEBOOK_OUTPUT_MAX_ROWS: usize = 100;

/// What [`AppView::run_notebook_cell`] did with a cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum CellRunStart {
    /// The cell is running; its result arrives through `finish_notebook_run`.
    Started,
    /// Nothing to run (not an SQL cell, empty, or a run already in progress).
    Skipped,
    /// The cell failed before running (no open connection); its error is on the cell.
    Failed,
}

impl AppView {
    fn notebook_position(&self, tab_id: u64) -> Option<usize> {
        self.notebooks.iter().position(|tab| tab.id == tab_id)
    }

    /// Run the SQL cell `cell_id` of notebook `tab_id` on the notebook's session. Every statement
    /// of the cell runs; the first failure stops the cell (as Jupyter does).
    pub(super) fn run_notebook_cell(
        &mut self,
        tab_id: u64,
        cell_id: &str,
        cx: &mut Context<'_, Self>,
    ) -> CellRunStart {
        let Some(position) = self.notebook_position(tab_id) else {
            return CellRunStart::Skipped;
        };
        // The editor owns the live text; the cell's copy is a mirror that may lag a keystroke.
        if let Some(editor) = self.notebook_editors.get(&(tab_id, cell_id.to_string())) {
            let text = editor.read(cx).live_text(cx);
            self.notebooks[position].set_cell_source_by_id(cell_id, &text);
        }
        let tab = &self.notebooks[position];
        if tab.running.is_some() {
            return CellRunStart::Skipped;
        }
        let Some(state) = tab.cells.iter().find(|state| state.cell.id == cell_id) else {
            return CellRunStart::Skipped;
        };
        if state.cell.kind != rustgrid_notebook::CellKind::Sql {
            return CellRunStart::Skipped;
        }
        let cell_id = state.cell.id.clone();
        let source = state.cell.source.trim().to_string();
        if source.is_empty() {
            return CellRunStart::Skipped;
        }
        let database = tab.database.clone();
        let connection_index = tab.connection_index;
        let Some(connection) = connection_index.and_then(|index| self.connection_arc(index)) else {
            self.notebooks[position].fail_cell(&cell_id, t!("query.not_connected").to_string());
            self.set_notebook_cell_grids(position, &cell_id, Vec::new(), cx);
            cx.notify();
            return CellRunStart::Failed;
        };
        let mysql = connection_index
            .is_some_and(|index| self.driver_dialect(index) == rustgrid_core::DriverDialect::Mysql);
        // A cell is an explicit unit of work, so the query tab's per-run statement limit does not
        // apply to it.
        let (statements, _) = split_for_run(&source, &connection, mysql, 0);
        if statements.is_empty() {
            return CellRunStart::Skipped;
        }

        let Some((generation, cancel)) = self.notebooks[position].begin_run(&cell_id) else {
            return CellRunStart::Skipped;
        };
        let pinned = self.notebook_sessions.remove(&tab_id);
        cx.notify();

        let runtime = self.runtime.clone();
        cx.spawn(async move |this, cx| {
            let started = std::time::Instant::now();
            let mut session = match pinned {
                Some(session) => session,
                None => {
                    let mut session = ScriptSessionGuard {
                        runtime: runtime.clone(),
                        session: None,
                    };
                    let opened = {
                        let connection = connection.clone();
                        let database = database.clone();
                        runtime
                            .spawn(
                                async move { connection.script_session(database.as_deref()).await },
                            )
                            .await
                    };
                    let opened = match opened {
                        Ok(result) => result.map_err(|error| error.to_string()),
                        Err(error) => Err(Error::other(error).to_string()),
                    };
                    match opened {
                        Ok(opened) => session.session = opened,
                        // The connection itself failed: report it on the cell instead of trying
                        // every statement on the pool.
                        Err(error) => {
                            let _ = this.update(cx, |view, cx| {
                                view.finish_notebook_run(
                                    tab_id,
                                    &cell_id,
                                    generation,
                                    started.elapsed(),
                                    Vec::new(),
                                    None,
                                    Some(error),
                                    cx,
                                );
                            });
                            return;
                        }
                    }
                    session
                }
            };
            let outcomes = run_statements(
                &runtime,
                &mut session,
                &connection,
                database.as_deref(),
                statements,
                &cancel,
                true,
            )
            .await;
            let elapsed = started.elapsed();
            let (results, error) = flatten_outcomes(outcomes);
            let _ = this.update(cx, |view, cx| {
                view.finish_notebook_run(
                    tab_id,
                    &cell_id,
                    generation,
                    elapsed,
                    results,
                    Some(session),
                    error,
                    cx,
                );
            });
        })
        .detach();
        CellRunStart::Started
    }

    /// The editor of the cell `cell_id` in the notebook at `index`, created on first use with the
    /// cell's text: SQL with the connection's highlighting, or Markdown. Its edits are mirrored
    /// into the cell. A cell whose kind changes gets a new editor.
    pub(super) fn ensure_notebook_editor(
        &mut self,
        index: usize,
        cell_id: &str,
        cx: &mut Context<'_, Self>,
    ) -> Option<Entity<ui::SqlEditor>> {
        let tab = self.notebooks.get(index)?;
        let tab_id = tab.id;
        let key = (tab_id, cell_id.to_string());
        if let Some(editor) = self.notebook_editors.get(&key) {
            return Some(editor.clone());
        }
        let cell = &tab
            .cells
            .iter()
            .find(|state| state.cell.id == cell_id)?
            .cell;
        let source = cell.source.clone();
        let sql = cell.kind == rustgrid_notebook::CellKind::Sql;
        let connection_index = tab.connection_index;
        let database = tab.database.clone();
        let schema = tab.schema.clone();
        let dialect = connection_index
            .map(|connection| self.driver_dialect(connection))
            .unwrap_or_default();
        let supports_schemas = connection_index
            .is_some_and(|connection| self.driver_supports(connection, DriverCapability::Schemas));
        let options = ui::SqlEditorOptions {
            language: if sql { "sql" } else { "markdown" }.into(),
            font_family: self.editor_font().into(),
            font_size: self.editor_font_size(),
            line_number: self.editor_line_numbers,
            readonly: false,
        };
        let weak = cx.weak_entity();
        let zoom_weak = weak.clone();
        let change_id = cell_id.to_string();
        let editor = cx.new(|cx| {
            let mut editor = ui::SqlEditor::new(cx)
                .options(options)
                .on_change(Rc::new(move |text, cx| {
                    let _ = weak.update(cx, |app, cx| {
                        if let Some(position) = app.notebook_position(tab_id) {
                            let tab = &mut app.notebooks[position];
                            let was_dirty = tab.dirty;
                            let old_lines = tab
                                .cells
                                .iter()
                                .find(|state| state.cell.id == change_id)
                                .map(|state| state.cell.source.lines().count());
                            tab.set_cell_source_by_id(&change_id, text);
                            // Re-render only when something drawn outside the editor changes: the
                            // tab's `*` on the first edit, or the editor's height (its line count).
                            if (!was_dirty && tab.dirty) || old_lines != Some(text.lines().count())
                            {
                                cx.notify();
                            }
                        }
                    });
                }))
                .on_zoom(Rc::new(move |step, cx| {
                    zoom_weak
                        .update(cx, |app, cx| app.zoom_editor_font(step, cx))
                        .unwrap_or(false)
                }));
            if sql {
                editor.set_scope(
                    connection_index,
                    database,
                    schema,
                    supports_schemas,
                    dialect,
                    false,
                    false,
                    cx,
                );
            }
            editor.set_text(source, cx);
            editor
        });
        self.notebook_editors.insert(key, editor.clone());
        Some(editor)
    }

    /// "Run" a cell from the keyboard: an SQL cell runs, a Markdown cell leaves its editor and
    /// renders (as in Jupyter). Returns whether there was such a cell.
    fn run_or_render_notebook_cell(
        &mut self,
        index: usize,
        cell: usize,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        let Some(tab) = self.notebooks.get_mut(index) else {
            return false;
        };
        let Some(state) = tab.cells.get(cell) else {
            return false;
        };
        let tab_id = tab.id;
        let cell_id = state.cell.id.clone();
        match state.cell.kind {
            rustgrid_notebook::CellKind::Sql => {
                self.run_notebook_cell(tab_id, &cell_id, cx);
            }
            rustgrid_notebook::CellKind::Markdown => {
                // Pick up the editor's last keystroke before deciding whether it can render.
                if let Some(editor) = self.notebook_editors.get(&(tab_id, cell_id.clone())) {
                    let text = editor.read(cx).live_text(cx);
                    tab.set_cell_source_by_id(&cell_id, &text);
                }
                tab.set_markdown_editing(cell, false);
            }
            rustgrid_notebook::CellKind::Raw => {}
        }
        cx.notify();
        true
    }

    /// Select the cell at `cell` and put the keyboard in its editor (SQL, or a Markdown cell being
    /// edited); a rendered cell is only selected.
    fn focus_notebook_cell(
        &mut self,
        index: usize,
        cell: usize,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(tab) = self.notebooks.get_mut(index) else {
            return;
        };
        tab.select_cell(cell);
        let Some(state) = tab.cells.get(cell) else {
            return;
        };
        let has_editor = match state.cell.kind {
            rustgrid_notebook::CellKind::Sql => true,
            rustgrid_notebook::CellKind::Markdown => state.editing,
            rustgrid_notebook::CellKind::Raw => false,
        };
        let cell_id = state.cell.id.clone();
        if has_editor && let Some(editor) = self.ensure_notebook_editor(index, &cell_id, cx) {
            editor.update(cx, |editor, cx| editor.focus_when_ready(window, cx));
        }
        cx.notify();
    }

    /// Ctrl+Enter: run (or render) the cell and stay on it.
    pub(super) fn notebook_key_run(
        &mut self,
        index: usize,
        cell: usize,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        self.run_or_render_notebook_cell(index, cell, cx)
    }

    /// Shift+Enter: run (or render) the cell and move to the next one, adding an SQL cell when it
    /// was the last.
    pub(super) fn notebook_key_run_advance(
        &mut self,
        index: usize,
        cell: usize,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        if !self.run_or_render_notebook_cell(index, cell, cx) {
            return false;
        }
        let Some(tab) = self.notebooks.get_mut(index) else {
            return true;
        };
        if cell + 1 >= tab.cells.len() {
            tab.insert_cell(cell + 1, rustgrid_notebook::Cell::new_sql(""));
        }
        self.focus_notebook_cell(index, cell + 1, window, cx);
        true
    }

    /// Alt+Enter: run (or render) the cell and insert a new SQL cell under it.
    pub(super) fn notebook_key_run_insert(
        &mut self,
        index: usize,
        cell: usize,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        if !self.run_or_render_notebook_cell(index, cell, cx) {
            return false;
        }
        if let Some(tab) = self.notebooks.get_mut(index) {
            tab.insert_cell(cell + 1, rustgrid_notebook::Cell::new_sql(""));
        }
        self.focus_notebook_cell(index, cell + 1, window, cx);
        true
    }

    /// Escape: leave a Markdown cell's editor (it renders). Anywhere else the key is left to the
    /// editor (it closes the completion popup), so this returns `false`.
    pub(super) fn notebook_key_leave_edit(
        &mut self,
        index: usize,
        cell: usize,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        let editing_markdown = self
            .notebooks
            .get(index)
            .and_then(|tab| tab.cells.get(cell))
            .is_some_and(|state| {
                state.cell.kind == rustgrid_notebook::CellKind::Markdown && state.editing
            });
        if editing_markdown {
            self.run_or_render_notebook_cell(index, cell, cx);
        }
        editing_markdown
    }

    /// Run the selected cell of the notebook at `index`.
    pub(super) fn run_selected_notebook_cell(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        let Some(tab) = self.notebooks.get(index) else {
            return;
        };
        let tab_id = tab.id;
        if let Some(cell_id) = tab
            .cells
            .get(tab.selected)
            .map(|state| state.cell.id.clone())
        {
            self.run_notebook_cell(tab_id, &cell_id, cx);
        }
    }

    /// Run All: run every SQL cell from the top, one after another, stopping at the first error.
    pub(super) fn run_all_notebook_cells(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        let Some(tab) = self.notebooks.get_mut(index) else {
            return;
        };
        if tab.running.is_some() {
            return;
        }
        let tab_id = tab.id;
        let first = tab.queue_run_all(0);
        self.run_queued_notebook_cells(tab_id, first, cx);
    }

    /// Start the first cell of the Run All queue that actually runs: empty cells are passed over,
    /// a cell that cannot start (no connection) ends the queue.
    fn run_queued_notebook_cells(
        &mut self,
        tab_id: u64,
        mut next: Option<String>,
        cx: &mut Context<'_, Self>,
    ) {
        while let Some(cell_id) = next {
            let outcome = self.run_notebook_cell(tab_id, &cell_id, cx);
            if outcome == CellRunStart::Started {
                return;
            }
            let Some(position) = self.notebook_position(tab_id) else {
                return;
            };
            next = self.notebooks[position].next_queued(outcome == CellRunStart::Skipped);
        }
    }

    /// Install a cell run's results and give the session back to the notebook. A stale run (the
    /// generation changed through Stop/Restart, or the tab was closed) installs nothing, and its
    /// session is dropped here, which releases it on the runtime.
    #[allow(clippy::too_many_arguments)]
    fn finish_notebook_run(
        &mut self,
        tab_id: u64,
        cell_id: &str,
        generation: u64,
        elapsed: std::time::Duration,
        results: Vec<QueryResult>,
        session: Option<ScriptSessionGuard>,
        error: Option<String>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(position) = self.notebook_position(tab_id) else {
            return;
        };
        let tab = &mut self.notebooks[position];
        if generation != tab.run_generation {
            return;
        }
        let succeeded = error.is_none();
        let installed = tab.finish_run(
            cell_id,
            generation,
            elapsed,
            &results,
            error,
            NOTEBOOK_OUTPUT_MAX_ROWS,
        );
        if installed {
            self.install_notebook_result_grids(position, cell_id, results, elapsed, cx);
        }
        // Only a session that still pins a connection is kept; an empty guard (a driver without
        // sessions, or one lost to a failed task) is opened again on the next run.
        if let Some(session) = session.filter(|session| session.session.is_some()) {
            self.notebook_sessions.insert(tab_id, session);
        }
        if installed {
            let next = self.notebooks[position].next_queued(succeeded);
            self.run_queued_notebook_cells(tab_id, next, cx);
        }
        cx.notify();
    }

    /// Create one read-only grid per result set of a finished cell run (statements without a
    /// result set get no grid) and replace the cell's previous grids with them.
    fn install_notebook_result_grids(
        &mut self,
        position: usize,
        cell_id: &str,
        results: Vec<QueryResult>,
        elapsed: std::time::Duration,
        cx: &mut Context<'_, Self>,
    ) {
        let tab = &self.notebooks[position];
        let database = tab.database.clone().unwrap_or_default();
        let connection_index = tab.connection_index;
        let connection = connection_index.and_then(|index| self.connection_arc(index));
        let mut grids = Vec::with_capacity(results.len());
        if let (Some(connection), Some(connection_index)) = (connection, connection_index) {
            let dialect = self.driver_dialect(connection_index);
            let connection_name = self
                .connections
                .get(connection_index)
                .map(|node| node.profile.name.clone())
                .unwrap_or_default();
            for result in results {
                if !result.has_result_set {
                    grids.push(None);
                    continue;
                }
                let id = self.push_query_result_grid(
                    connection.clone(),
                    dialect,
                    connection_name.clone(),
                    result,
                    database.clone(),
                    String::new(),
                    false,
                    elapsed,
                    cx,
                );
                grids.push(Some(id));
            }
        }
        self.set_notebook_cell_grids(position, cell_id, grids, cx);
    }

    /// Give the cell `cell_id` these grids and drop the ones it had, so no grid is left behind.
    fn set_notebook_cell_grids(
        &mut self,
        position: usize,
        cell_id: &str,
        grids: Vec<Option<u64>>,
        cx: &mut Context<'_, Self>,
    ) {
        let released = self.notebooks[position].replace_result_grids(cell_id, grids);
        self.remove_grids_by_id(&released, cx);
    }

    /// Stop the notebook's running cell: its remaining statements are skipped, its result is
    /// discarded and the session it was using is released (as a cancelled kernel would lose it).
    pub(super) fn stop_notebook(&mut self, tab_id: u64, cx: &mut Context<'_, Self>) {
        let Some(position) = self.notebook_position(tab_id) else {
            return;
        };
        if self.notebooks[position].stop_run() {
            cx.notify();
        }
    }

    /// Restart the notebook's session ("Restart kernel"): stop any running cell and drop the pinned
    /// session, so the next run starts on a fresh connection with no session state.
    pub(super) fn restart_notebook_session(&mut self, tab_id: u64, cx: &mut Context<'_, Self>) {
        let Some(position) = self.notebook_position(tab_id) else {
            return;
        };
        self.notebooks[position].stop_run();
        self.notebook_sessions.remove(&tab_id);
        cx.notify();
    }
}

impl AppView {
    /// Release what an open notebook holds outside its cells: stop a running cell, drop the
    /// pinned session (released on the runtime) and drop the cells' result grids. The cells and
    /// their saved outputs stay, so the notebook can run again once its connection is back.
    fn release_notebook_resources(&mut self, position: usize, cx: &mut Context<'_, Self>) {
        let Some(tab) = self.notebooks.get_mut(position) else {
            return;
        };
        tab.stop_run();
        let tab_id = tab.id;
        let grids = tab.take_result_grids();
        self.notebook_sessions.remove(&tab_id);
        self.remove_grids_by_id(&grids, cx);
    }

    /// Close notebook `tab_id`: release its session and grids and drop the tab, keeping the
    /// shown tab in place (or settling on a neighbour when this one was shown). Unsaved changes
    /// are the caller's to confirm.
    pub(super) fn close_notebook(&mut self, tab_id: u64, cx: &mut Context<'_, Self>) {
        let Some(position) = self.notebook_position(tab_id) else {
            return;
        };
        self.release_notebook_resources(position, cx);
        self.notebook_editors.retain(|(id, _), _| *id != tab_id);
        self.notebooks.remove(position);
        match self.active_notebook {
            Some(active) if active == position => {
                self.active_notebook = if self.notebooks.is_empty() {
                    None
                } else {
                    Some(position.min(self.notebooks.len() - 1))
                };
            }
            Some(active) if active > position => self.active_notebook = Some(active - 1),
            _ => {}
        }
        self.settle_after_tab_closed(cx);
        cx.notify();
    }

    /// Close every notebook tab (Close Other / Close All Tabs).
    pub(super) fn close_all_notebooks(&mut self, cx: &mut Context<'_, Self>) {
        while let Some(id) = self.notebooks.last().map(|tab| tab.id) {
            self.close_notebook(id, cx);
        }
    }

    /// Open a new, unsaved notebook bound to a connection (and database/schema), with one empty
    /// SQL cell, and show it.
    pub(super) fn open_new_notebook(
        &mut self,
        connection_index: Option<usize>,
        database: Option<String>,
        schema: Option<String>,
        cx: &mut Context<'_, Self>,
    ) {
        let id = self.next_notebook_id;
        self.next_notebook_id += 1;
        let notebook = rustgrid_notebook::Notebook {
            cells: vec![rustgrid_notebook::Cell::new_sql("")],
            ..Default::default()
        };
        let mut tab =
            crate::session::NotebookTab::from_notebook(id, notebook, None, connection_index);
        tab.database = database;
        tab.schema = schema;
        self.notebooks.push(tab);
        self.activate_notebook(self.notebooks.len() - 1, cx);
        self.clear_object_search_unless_kept(cx);
    }

    /// "New notebook" on a connection row: bound to the connection's current database.
    pub(super) fn open_new_notebook_for_connection(
        &mut self,
        connection_index: usize,
        cx: &mut Context<'_, Self>,
    ) {
        let database = self.default_query_database(connection_index, cx);
        let schema = self.default_query_schema(cx, connection_index, database.as_deref());
        self.open_new_notebook(Some(connection_index), database, schema, cx);
    }

    /// "New notebook" on a database row: bound to that database (no schema).
    pub(super) fn open_new_notebook_for_database(
        &mut self,
        connection_index: usize,
        database_index: usize,
        cx: &mut Context<'_, Self>,
    ) {
        let database = self.database_name(connection_index, database_index);
        self.open_new_notebook(Some(connection_index), database, None, cx);
    }

    /// Show the notebook at `index` in the content pane.
    pub(super) fn activate_notebook(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        if index >= self.notebooks.len() {
            return;
        }
        self.active_notebook = Some(index);
        self.active_query = None;
        self.active_design = None;
        self.active_grid = None;
        self.main_tab = MainTab::Queries;
        cx.notify();
    }

    /// Insert a new cell of `kind` under the selected one and select it.
    pub(super) fn add_notebook_cell(
        &mut self,
        index: usize,
        kind: rustgrid_notebook::CellKind,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(tab) = self.notebooks.get_mut(index) else {
            return;
        };
        let cell = match kind {
            rustgrid_notebook::CellKind::Markdown => rustgrid_notebook::Cell::new_markdown(""),
            _ => rustgrid_notebook::Cell::new_sql(""),
        };
        let at = if tab.cells.is_empty() {
            0
        } else {
            tab.selected + 1
        };
        tab.insert_cell(at, cell);
        cx.notify();
    }

    /// Select the cell at `cell` of the notebook at `index`.
    pub(super) fn select_notebook_cell(
        &mut self,
        index: usize,
        cell: usize,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(tab) = self.notebooks.get_mut(index)
            && tab.select_cell(cell)
        {
            cx.notify();
        }
    }

    /// Move the cell at `cell` one slot up (`up`) or down.
    pub(super) fn move_notebook_cell(
        &mut self,
        index: usize,
        cell: usize,
        up: bool,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(tab) = self.notebooks.get_mut(index) else {
            return;
        };
        let to = if up {
            cell.checked_sub(1)
        } else {
            Some(cell + 1).filter(|to| *to < tab.cells.len())
        };
        if let Some(to) = to {
            tab.move_cell(cell, to);
            cx.notify();
        }
    }

    /// Delete the cell at `cell`, with its editor and result grids. The running cell stays.
    pub(super) fn delete_notebook_cell(
        &mut self,
        index: usize,
        cell: usize,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(tab) = self.notebooks.get_mut(index) else {
            return;
        };
        let Some(cell_id) = tab.cells.get(cell).map(|state| state.cell.id.clone()) else {
            return;
        };
        let count = tab.cells.len();
        let grids = tab.remove_cell(cell);
        if tab.cells.len() == count {
            return;
        }
        let tab_id = tab.id;
        self.notebook_editors.remove(&(tab_id, cell_id));
        self.remove_grids_by_id(&grids, cx);
        cx.notify();
    }

    /// Turn the cell at `cell` into an SQL or Markdown cell. Its editor is rebuilt for the new
    /// language and its result grids go with its outputs.
    pub(super) fn set_notebook_cell_kind(
        &mut self,
        index: usize,
        cell: usize,
        kind: rustgrid_notebook::CellKind,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(tab) = self.notebooks.get_mut(index) else {
            return;
        };
        let Some(cell_id) = tab.cells.get(cell).map(|state| state.cell.id.clone()) else {
            return;
        };
        let before = tab.cells[cell].cell.kind;
        let grids = tab.set_cell_kind(cell, kind);
        if tab.cells[cell].cell.kind == before {
            return;
        }
        let tab_id = tab.id;
        self.notebook_editors.remove(&(tab_id, cell_id));
        self.remove_grids_by_id(&grids, cx);
        cx.notify();
    }

    /// Show a Markdown cell as its editor (`true`, the double-click) or rendered (`false`).
    pub(super) fn edit_notebook_markdown(
        &mut self,
        index: usize,
        cell: usize,
        editing: bool,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(tab) = self.notebooks.get_mut(index) {
            tab.set_markdown_editing(cell, editing);
            cx.notify();
        }
    }

    /// The connection at `connection_index` is being closed: every notebook bound to it loses its
    /// session and result grids (they would hold a dead connection), but stays open.
    pub(super) fn release_connection_notebooks(
        &mut self,
        connection_index: usize,
        cx: &mut Context<'_, Self>,
    ) {
        for position in 0..self.notebooks.len() {
            if self.notebooks[position].connection_index == Some(connection_index) {
                self.release_notebook_resources(position, cx);
            }
        }
    }

    /// The connection at `removed` was deleted from the list: keep every notebook's connection
    /// index naming the same profile, and leave the deleted connection's notebooks unbound.
    pub(super) fn notebooks_connection_removed(&mut self, removed: usize) {
        for tab in &mut self.notebooks {
            tab.connection_removed(removed);
        }
    }
}

/// The result sets of the statements that ran, in order, and the error that stopped the cell.
fn flatten_outcomes(outcomes: Vec<StatementOutcome>) -> (Vec<QueryResult>, Option<String>) {
    let mut results = Vec::new();
    for (_, _, outcome) in outcomes {
        match outcome {
            Ok(mut statement_results) => results.append(&mut statement_results),
            Err(error) => return (results, Some(error.to_string())),
        }
    }
    (results, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn affected(rows: u64) -> QueryResult {
        QueryResult {
            statement: "update t set a = 1".into(),
            columns: Vec::new(),
            rows: Vec::new(),
            rows_affected: rows,
            has_result_set: false,
            last_insert_id: None,
        }
    }

    #[test]
    fn outcomes_flatten_in_order() {
        let zero = std::time::Duration::ZERO;
        let outcomes: Vec<StatementOutcome> = vec![
            ("a".into(), zero, Ok(vec![affected(1)])),
            ("b".into(), zero, Ok(vec![affected(2), affected(3)])),
        ];
        let (results, error) = flatten_outcomes(outcomes);
        assert!(error.is_none());
        let rows: Vec<u64> = results.iter().map(|r| r.rows_affected).collect();
        assert_eq!(rows, vec![1, 2, 3]);
    }

    #[test]
    fn outcomes_stop_at_the_first_error() {
        let zero = std::time::Duration::ZERO;
        let outcomes: Vec<StatementOutcome> = vec![
            ("a".into(), zero, Ok(vec![affected(1)])),
            (
                "b".into(),
                zero,
                Err(Error::other(std::io::Error::other("boom"))),
            ),
            ("c".into(), zero, Ok(vec![affected(9)])),
        ];
        let (results, error) = flatten_outcomes(outcomes);
        assert_eq!(results.len(), 1);
        assert!(error.is_some_and(|message| message.contains("boom")));
    }
}

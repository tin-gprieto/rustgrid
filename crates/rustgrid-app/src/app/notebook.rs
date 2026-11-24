//! SQL notebooks: running a notebook's cells against its connection.
//!
//! A notebook keeps one pinned [`rustgrid_core::ScriptSession`] (in `AppView::notebook_sessions`,
//! by notebook id) across cell runs, so `USE`/`SET`/`BEGIN` and temporary tables carry from one
//! cell to the next, like a Jupyter kernel. A driver that cannot pin a session runs every
//! statement on the pool instead, with no state shared between cells. The session is taken out of
//! the map while a cell runs and only put back when the run is still current; a run that Stop or
//! Restart made stale drops it, and [`ScriptSessionGuard`] releases it on the Tokio runtime.

use super::query::{StatementOutcome, run_statements, split_for_run};
use super::runsql::ScriptSessionGuard;
use super::*;

/// How many rows of each result set a cell keeps in its outputs (and so writes to the file).
const NOTEBOOK_OUTPUT_MAX_ROWS: usize = 100;

#[allow(dead_code)] // wired to the notebook view in the next stage
impl AppView {
    fn notebook_position(&self, tab_id: u64) -> Option<usize> {
        self.notebooks.iter().position(|tab| tab.id == tab_id)
    }

    /// Run the SQL cell at `cell_index` of notebook `tab_id` on the notebook's session. A run
    /// already in progress, a non-SQL cell or an empty cell does nothing. Every statement of the
    /// cell runs; the first failure stops the cell (as Jupyter does).
    pub(super) fn run_notebook_cell(
        &mut self,
        tab_id: u64,
        cell_index: usize,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(position) = self.notebook_position(tab_id) else {
            return;
        };
        let tab = &self.notebooks[position];
        if tab.running.is_some() {
            return;
        }
        let Some(state) = tab.cells.get(cell_index) else {
            return;
        };
        if state.cell.kind != rustgrid_notebook::CellKind::Sql {
            return;
        }
        let cell_id = state.cell.id.clone();
        let source = state.cell.source.trim().to_string();
        if source.is_empty() {
            return;
        }
        let database = tab.database.clone();
        let connection_index = tab.connection_index;
        let Some(connection) = connection_index.and_then(|index| self.connection_arc(index)) else {
            self.notebooks[position].fail_cell(&cell_id, t!("query.not_connected").to_string());
            self.set_notebook_cell_grids(position, &cell_id, Vec::new(), cx);
            cx.notify();
            return;
        };
        let mysql = connection_index
            .is_some_and(|index| self.driver_dialect(index) == rustgrid_core::DriverDialect::Mysql);
        // A cell is an explicit unit of work, so the query tab's per-run statement limit does not
        // apply to it.
        let (statements, _) = split_for_run(&source, &connection, mysql, 0);
        if statements.is_empty() {
            return;
        }

        let Some((generation, cancel)) = self.notebooks[position].begin_run(&cell_id) else {
            return;
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

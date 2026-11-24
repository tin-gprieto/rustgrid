use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::ScrollHandle;
use rustgrid_core::{
    CellValue, ColumnInfo, Connection, ConnectionProfile, DriverDialect, FilterCondition,
    FilterConjunction, FilterNode, FilterOperator, QueryResult, RoutineDetails, RoutineInfo,
    RoutineKind, SortColumn, TableInfo, TableStatus, ViewDetails,
};

#[derive(Default)]
pub enum Loadable<T> {
    #[default]
    Idle,
    Loading,
    Loaded(T),
    Failed(String),
}

/// A connection's live state. There is no "failed" state: a failed attempt closes the node again
/// (its error is reported by the connect flow), so the tree shows closed, connecting or connected.
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected(Arc<dyn Connection>),
}

pub struct ConnectionNode {
    pub profile: ConnectionProfile,
    pub password: Option<String>,
    pub password_saved: bool,
    pub status: ConnectionStatus,
    pub databases: Loadable<Vec<DatabaseNode>>,
    pub expanded: bool,
}

pub struct DatabaseNode {
    pub name: String,
    pub tables: Loadable<Vec<TableInfo>>,
    /// The database's `SHOW TABLE STATUS`-style table overviews, for the Tables 详细列表.
    pub table_statuses: Loadable<Vec<(String, TableStatus)>>,
    /// The database's stored routines (functions and procedures), loaded lazily when the
    /// Functions tab or the connection tree's Functions category needs them.
    pub routines: Loadable<Vec<RoutineInfo>>,
    pub opened: bool,
    pub expanded: bool,
    pub categories: CategoryExpansion,
    /// The schemas the connection tree has expanded (SQL Server). Empty for schema-less
    /// engines, where the tree renders the database's categories directly.
    pub expanded_schemas: BTreeSet<String>,
    /// The schemas the user has opened (SQL Server): their tree icon is highlighted and their
    /// objects are shown in the content pane. Empty for schema-less engines.
    pub opened_schemas: BTreeSet<String>,
    /// The database's schemas, loaded when it is opened so a freshly created (still empty) schema
    /// is visible in the tree. `None` for schema-less engines.
    pub schemas: Option<Loadable<Vec<String>>>,
    /// The next object page's cursor for a key-value engine, when one is left over.
    pub object_cursor: Option<String>,
    /// The namespace filter (`user:*`) applied to the object list, if any.
    pub object_pattern: Option<String>,
    /// The key-value namespaces (key prefixes) of this database, with their key counts.
    pub namespaces: Loadable<Vec<rustgrid_core::ObjectNamespace>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Tables,
    Views,
    Functions,
    Queries,
    Backups,
}

impl Category {
    pub const ALL: [Category; 5] = [
        Category::Tables,
        Category::Views,
        Category::Functions,
        Category::Queries,
        Category::Backups,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Tables => "common.tables",
            Category::Views => "common.views",
            Category::Functions => "common.functions",
            Category::Queries => "common.queries",
            Category::Backups => "common.backups",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Category::Tables => "t",
            Category::Views => "v",
            Category::Functions => "f",
            Category::Queries => "q",
            Category::Backups => "b",
        }
    }

    pub fn icon_path(self) -> &'static str {
        match self {
            Category::Tables => "icons/tables.svg",
            Category::Views => "icons/views.svg",
            Category::Functions => "icons/functions.svg",
            Category::Queries => "icons/queries.svg",
            Category::Backups => "icons/backups.svg",
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct CategoryExpansion {
    pub tables: bool,
    pub views: bool,
    pub functions: bool,
    pub queries: bool,
    pub backups: bool,
}

impl CategoryExpansion {
    pub fn get(&self, category: Category) -> bool {
        match category {
            Category::Tables => self.tables,
            Category::Views => self.views,
            Category::Functions => self.functions,
            Category::Queries => self.queries,
            Category::Backups => self.backups,
        }
    }

    pub fn toggle(&mut self, category: Category) {
        let slot = match category {
            Category::Tables => &mut self.tables,
            Category::Views => &mut self.views,
            Category::Functions => &mut self.functions,
            Category::Queries => &mut self.queries,
            Category::Backups => &mut self.backups,
        };
        *slot = !*slot;
    }
}

/// One rectangular block of cells (a drag/shift selection).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellRange {
    pub anchor: (usize, usize),
    pub cursor: (usize, usize),
}

impl CellRange {
    pub fn new(row: usize, col: usize) -> Self {
        Self {
            anchor: (row, col),
            cursor: (row, col),
        }
    }

    pub fn rows(&self) -> (usize, usize) {
        (
            self.anchor.0.min(self.cursor.0),
            self.anchor.0.max(self.cursor.0),
        )
    }

    pub fn cols(&self) -> (usize, usize) {
        (
            self.anchor.1.min(self.cursor.1),
            self.anchor.1.max(self.cursor.1),
        )
    }

    pub fn contains(&self, row: usize, col: usize) -> bool {
        let (start_row, end_row) = self.rows();
        let (start_col, end_col) = self.cols();
        row >= start_row && row <= end_row && col >= start_col && col <= end_col
    }
}

/// An Excel-like selection: one or more rectangular ranges. `active` indexes the range that a
/// shift-click or drag extends, and whose `cursor` is the "current" cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellSelection {
    pub ranges: Vec<CellRange>,
    pub active: usize,
}

impl CellSelection {
    pub fn new(row: usize, col: usize) -> Self {
        Self {
            ranges: vec![CellRange::new(row, col)],
            active: 0,
        }
    }

    pub fn single(anchor: (usize, usize), cursor: (usize, usize)) -> Self {
        Self {
            ranges: vec![CellRange { anchor, cursor }],
            active: 0,
        }
    }

    /// The range that shift/drag extends.
    pub fn active(&self) -> &CellRange {
        &self.ranges[self.active.min(self.ranges.len().saturating_sub(1))]
    }

    pub fn active_cursor(&self) -> (usize, usize) {
        self.active().cursor
    }

    /// Whether the selection covers exactly one cell. The wheel walks the cursor while it does,
    /// and scrolls the viewport (keeping the selection intact) once more than one cell is selected.
    pub fn is_single_cell(&self) -> bool {
        self.ranges.len() == 1 && {
            let range = &self.ranges[0];
            range.anchor == range.cursor
        }
    }

    pub fn contains(&self, row: usize, col: usize) -> bool {
        self.ranges.iter().any(|range| range.contains(row, col))
    }

    /// Overall bounding rows across every range (used for status/headers).
    pub fn rows(&self) -> (usize, usize) {
        self.ranges
            .iter()
            .fold((usize::MAX, 0), |(min, max), range| {
                let (start, end) = range.rows();
                (min.min(start), max.max(end))
            })
    }

    /// Overall bounding columns across every range.
    pub fn cols(&self) -> (usize, usize) {
        self.ranges
            .iter()
            .fold((usize::MAX, 0), |(min, max), range| {
                let (start, end) = range.cols();
                (min.min(start), max.max(end))
            })
    }

    /// Remove `span` from every range, splitting a range the span cuts through into the pieces
    /// around it. Returns whether any cell was actually removed. Ctrl+click uses this so a click
    /// inside 全选's single big range deselects just the clicked cell (like Excel) instead of
    /// dropping the whole selection.
    pub fn subtract(&mut self, span: CellRange) -> bool {
        let (span_start_row, span_end_row) = span.rows();
        let (span_start_col, span_end_col) = span.cols();
        let mut pieces = Vec::new();
        let mut changed = false;
        for range in &self.ranges {
            let (start_row, end_row) = range.rows();
            let (start_col, end_col) = range.cols();
            let hit_start_row = start_row.max(span_start_row);
            let hit_end_row = end_row.min(span_end_row);
            let hit_start_col = start_col.max(span_start_col);
            let hit_end_col = end_col.min(span_end_col);
            if hit_start_row > hit_end_row || hit_start_col > hit_end_col {
                pieces.push(*range);
                continue;
            }
            changed = true;
            if start_row < hit_start_row {
                pieces.push(CellRange {
                    anchor: (start_row, start_col),
                    cursor: (hit_start_row - 1, end_col),
                });
            }
            if hit_end_row < end_row {
                pieces.push(CellRange {
                    anchor: (hit_end_row + 1, start_col),
                    cursor: (end_row, end_col),
                });
            }
            if start_col < hit_start_col {
                pieces.push(CellRange {
                    anchor: (hit_start_row, start_col),
                    cursor: (hit_end_row, hit_start_col - 1),
                });
            }
            if hit_end_col < end_col {
                pieces.push(CellRange {
                    anchor: (hit_start_row, hit_end_col + 1),
                    cursor: (hit_end_row, end_col),
                });
            }
        }
        if changed {
            self.ranges = pieces;
            if self.ranges.is_empty() {
                self.active = 0;
            } else {
                self.active = self.active.min(self.ranges.len() - 1);
            }
        }
        changed
    }

    /// Every selected cell, de-duplicated across ranges.
    pub fn cells(&self) -> Vec<(usize, usize)> {
        let mut cells: Vec<(usize, usize)> = Vec::new();
        for range in &self.ranges {
            let (start_row, end_row) = range.rows();
            let (start_col, end_col) = range.cols();
            for row in start_row..=end_row {
                for col in start_col..=end_col {
                    if !cells.contains(&(row, col)) {
                        cells.push((row, col));
                    }
                }
            }
        }
        cells
    }

    /// The distinct row indices touched by any range, sorted ascending.
    pub fn row_indices(&self) -> Vec<usize> {
        let mut rows = std::collections::BTreeSet::new();
        for range in &self.ranges {
            let (start, end) = range.rows();
            for row in start..=end {
                rows.insert(row);
            }
        }
        rows.into_iter().collect()
    }

    /// The distinct column indices touched by any range, sorted ascending.
    pub fn col_indices(&self) -> Vec<usize> {
        let mut cols = std::collections::BTreeSet::new();
        for range in &self.ranges {
            let (start, end) = range.cols();
            for col in start..=end {
                cols.insert(col);
            }
        }
        cols.into_iter().collect()
    }
}

/// Which sub-tab of a routine editor is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RoutineTab {
    /// The editable `CREATE ...` statement.
    Definition,
    /// Read-only metadata.
    Info,
    /// The SQL the Save button runs.
    Sql,
}

impl RoutineTab {
    /// Every sub-tab, in the order the editor presents them.
    pub const ALL: [RoutineTab; 3] = [RoutineTab::Definition, RoutineTab::Info, RoutineTab::Sql];

    /// The i18n key for the sub-tab's label.
    pub fn label_key(self) -> &'static str {
        match self {
            RoutineTab::Definition => "routine.tab.definition",
            RoutineTab::Info => "routine.tab.info",
            RoutineTab::Sql => "routine.tab.sql",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            RoutineTab::Definition => "routine-tab-definition",
            RoutineTab::Info => "routine-tab-info",
            RoutineTab::Sql => "routine-tab-sql",
        }
    }
}

/// The routine-editor state carried by a [`QueryTab`] whose `routine` is set. A routine editor
/// reuses the SQL editor (and its tab) but adds the routine's identity, metadata and the
/// 定义/信息/SQL 预览 sub-tabs.
pub struct RoutineTabState {
    pub kind: RoutineKind,
    /// The routine's current name (the name Save will create).
    pub name: String,
    /// The routine's name in the database, `None` until a new routine is saved.
    pub original_name: Option<String>,
    pub original_kind: Option<RoutineKind>,
    /// The routine's full loaded state (metadata + session settings), for the 信息 tab.
    pub details: Option<RoutineDetails>,
    pub tab: RoutineTab,
    /// Whether the definition editor wraps long lines.
    pub word_wrap: bool,
    /// Whether the find bar is showing.
    pub find_open: bool,
    pub find_query: String,
    /// A save is in flight.
    pub saving: bool,
}

impl RoutineTabState {
    pub fn new(kind: RoutineKind, name: String) -> Self {
        Self {
            kind,
            name,
            original_name: None,
            original_kind: None,
            details: None,
            tab: RoutineTab::Definition,
            word_wrap: true,
            find_open: false,
            find_query: String::new(),
            saving: false,
        }
    }
}

/// Which sub-tab of a view designer is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewTab {
    /// The editable `CREATE ... VIEW` statement.
    Definition,
    /// Read-only creation settings and metadata.
    Advanced,
    /// The SQL the Save button runs.
    Sql,
}

impl ViewTab {
    /// Every sub-tab, in the order the designer presents them.
    pub const ALL: [ViewTab; 3] = [ViewTab::Definition, ViewTab::Advanced, ViewTab::Sql];

    /// The i18n key for the sub-tab's label.
    pub fn label_key(self) -> &'static str {
        match self {
            ViewTab::Definition => "view.tab.definition",
            ViewTab::Advanced => "view.tab.advanced",
            ViewTab::Sql => "view.tab.sql",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            ViewTab::Definition => "view-tab-definition",
            ViewTab::Advanced => "view-tab-advanced",
            ViewTab::Sql => "view-tab-sql",
        }
    }
}

/// Which sub-tab of the view designer's bottom explain panel is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewExplainTab {
    /// The executed `EXPLAIN` statement and its status.
    Info,
    /// The `EXPLAIN` result table.
    Result,
}

impl ViewExplainTab {
    /// The i18n key for the sub-tab's label.
    pub fn label_key(self) -> &'static str {
        match self {
            ViewExplainTab::Info => "view.explain.tab.info",
            ViewExplainTab::Result => "view.explain.tab.result",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            ViewExplainTab::Info => "view-explain-tab-info",
            ViewExplainTab::Result => "view-explain-tab-result",
        }
    }
}

/// The view-designer state carried by a [`QueryTab`] whose `view` is set. A view designer reuses
/// the SQL editor (and its tab) but adds the view's identity, metadata and the
/// 定义/高级/SQL 预览 sub-tabs.
#[derive(Clone)]
pub struct ViewTabState {
    /// The view's current name (the name Save will create).
    pub name: String,
    /// The view's name in the database, `None` until a new view is saved.
    pub original_name: Option<String>,
    /// The view's full loaded state (metadata + definition), for the 高级 tab.
    pub details: Option<ViewDetails>,
    pub tab: ViewTab,
    /// A save is in flight.
    pub saving: bool,

    /// Whether the bottom 信息/解释 panel is showing.
    pub explain_open: bool,
    /// Which bottom panel sub-tab is active.
    pub explain_tab: ViewExplainTab,
    /// The `EXPLAIN` statement that was run (shown on the 信息 tab).
    pub explain_sql: String,
    /// An explain run is in flight.
    pub explain_running: bool,
    pub explain_elapsed: Option<std::time::Duration>,
    pub explain_error: Option<String>,
    /// The id of the grid in `AppView::grids` showing the explain plan, if any.
    pub explain_grid_id: Option<u64>,
}

impl ViewTabState {
    pub fn new(name: String) -> Self {
        Self {
            name,
            original_name: None,
            details: None,
            tab: ViewTab::Definition,
            saving: false,
            explain_open: false,
            explain_tab: ViewExplainTab::Info,
            explain_sql: String::new(),
            explain_running: false,
            explain_elapsed: None,
            explain_error: None,
            explain_grid_id: None,
        }
    }
}

/// The query result panel's tab strip (信息 plus one 结果 tag per result set): its horizontal
/// scroll, and whether the last layout found more tabs than fit. When they do not fit, the strip
/// shows ◀ / ▶ paging buttons (see `AppView::render_query_result_tabs`), like the main tab bar.
#[derive(Default)]
pub struct QueryResultTabs {
    pub scroll: ScrollHandle,
    /// Written by the strip's measurement canvas after each layout and read on the next render.
    pub overflow: Rc<Cell<bool>>,
}

/// One connection of a group query run. Every target owns a result page in the bottom panel, so a
/// failed connection (or a missing password) has somewhere to report itself instead of aborting
/// the whole run.
pub struct BatchTarget {
    /// The current index into `AppView::connections`, resolved when the run started.
    pub connection_index: Option<usize>,
    pub name: String,
    pub state: BatchTargetState,
    pub elapsed: Option<std::time::Duration>,
    /// A connection-level failure (driver missing, connection failed, password required).
    pub error: Option<String>,
    /// One entry per executed statement's result set, or its failure.
    pub results: Vec<BatchResult>,
    /// Which entry of `results` this connection's page shows.
    pub active_result: usize,
    /// Statements skipped because of the single-run statement limit.
    pub skipped: usize,
}

/// The state of one connection in a group query run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BatchTargetState {
    Pending,
    Connecting,
    Running,
    Done,
    Failed,
    NeedsPassword,
    Stopped,
}

/// One statement outcome inside a [`BatchTarget`]'s result page.
pub struct BatchResult {
    pub statement: String,
    pub elapsed: std::time::Duration,
    pub error: Option<String>,
    pub grid_id: Option<u64>,
    pub has_result_set: bool,
    pub row_count: usize,
    pub rows_affected: u64,
}

impl BatchTarget {
    pub fn pending(connection_index: Option<usize>, name: String) -> Self {
        Self {
            connection_index,
            name,
            state: BatchTargetState::Pending,
            elapsed: None,
            error: None,
            results: Vec::new(),
            active_result: 0,
            skipped: 0,
        }
    }

    /// The grid of the statement selected in this connection's page.
    pub fn active_grid_id(&self) -> Option<u64> {
        self.results
            .get(self.active_result)
            .and_then(|result| result.grid_id)
    }

    /// Whether this connection's page should show an error: the connection failed, or one of its
    /// statements did ("查询失败的在标签页内展示").
    pub fn has_error(&self) -> bool {
        matches!(
            self.state,
            BatchTargetState::Failed | BatchTargetState::NeedsPassword
        ) || self.results.iter().any(|result| result.error.is_some())
    }
}

/// A query tab that runs the same SQL on every connection of a connection-tree group, one result
/// page per connection. The group is re-resolved on every run, so members added or removed after
/// the tab was opened take effect immediately.
pub struct BatchQuery {
    pub group_id: u64,
    pub group_name: String,
    pub targets: Vec<BatchTarget>,
}

impl BatchQuery {
    /// Every grid created by the last run, for cleanup.
    pub fn grid_ids(&self) -> Vec<u64> {
        self.targets
            .iter()
            .flat_map(|target| target.results.iter().filter_map(|result| result.grid_id))
            .collect()
    }

    pub fn has_result_grid(&self) -> bool {
        self.targets
            .iter()
            .any(|target| target.results.iter().any(|result| result.grid_id.is_some()))
    }

    /// The grid for a result-panel page (`1` is the first connection; `0` is 信息).
    pub fn grid_for_page(&self, page: usize) -> Option<u64> {
        if page == 0 {
            return None;
        }
        self.targets.get(page - 1)?.active_grid_id()
    }
}

/// An open SQL editor tab. `sql` is the editable document; `caret` and `anchor` are byte
/// offsets into it, so they can be matched against `TextLayout` indices directly.
pub struct QueryTab {
    pub id: u64,
    /// The saved query name, once this tab was saved to (or opened from) the saved-query list.
    pub name: Option<String>,
    /// The `.sql` file this tab is bound to, once it was saved (or opened from the list). While this
    /// is set, Save overwrites the file in place instead of asking for a name and location again.
    pub saved_path: Option<std::path::PathBuf>,
    pub connection_index: Option<usize>,
    pub database: Option<String>,
    /// The schema the editor is scoped to (SQL Server). `None` leaves unqualified objects to the
    /// engine's default schema; it filters completion and resolves unqualified table references.
    pub schema: Option<String>,
    pub sql: String,
    /// The text as last saved/loaded. `sql != baseline` means there are unsaved edits, which
    /// drives the tab's dirty marker and the close confirmation.
    pub baseline: String,
    pub caret: usize,
    pub anchor: usize,
    pub selecting: bool,
    pub running: bool,
    /// Bumped on every run and on stop. A late result whose generation no longer matches the
    /// tab's is discarded, so "Stop" is not silently overwritten by the query finishing.
    pub run_generation: u64,
    /// One entry per statement the last run executed, in order. A lightweight summary (not the
    /// rows), so the 信息 tab can report each statement without duplicating the grid's data.
    pub results: Vec<QueryResultSummary>,
    /// The grid id in `AppView::grids` for each `results` entry (`None` for a statement that
    /// produced no result set).
    pub result_grids: Vec<Option<u64>>,
    /// The bottom result panel's active tab: `0` is 信息, `1..=n` is the `n`-th result set.
    pub active_result: usize,
    /// The result tab strip's scroll state (it pages with ◀ / ▶ when the tabs do not fit).
    pub result_tabs: QueryResultTabs,
    /// The last run's error, if it failed.
    pub result_error: Option<String>,
    /// The script that was run, shown on the 信息 tab.
    pub last_sql: String,
    /// How long the last run took, shown on the 信息 tab and the toolbar.
    pub last_elapsed: Option<std::time::Duration>,
    /// When the running query started, for the toolbar's live timer.
    pub run_started: Option<std::time::Instant>,
    /// The text before and after the last 美化SQL, so formatting again with the formatted text
    /// untouched restores the original.
    pub format_undo: Option<(String, String)>,
    /// Statements the last run skipped because of the statement limit (the 信息 tab says so).
    pub skipped_statements: usize,
    /// Set by Stop: the running script runs no further statement.
    pub run_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// The id of the grid in `AppView::grids` that shows this tab's active result set, if any.
    pub grid_id: Option<u64>,
    /// `Some` when this tab edits a stored routine instead of a free-form query.
    pub routine: Option<RoutineTabState>,
    /// `Some` when this tab designs a database view instead of a free-form query.
    pub view: Option<ViewTabState>,
    /// `Some` when this tab runs on every connection of a connection-tree group (one result page
    /// per connection) instead of on a single connection.
    pub batch: Option<BatchQuery>,
}

impl QueryTab {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            name: None,
            saved_path: None,
            connection_index: None,
            database: None,
            schema: None,
            sql: String::new(),
            baseline: String::new(),
            caret: 0,
            anchor: 0,
            selecting: false,
            running: false,
            run_generation: 0,
            results: Vec::new(),
            result_grids: Vec::new(),
            active_result: 0,
            result_tabs: QueryResultTabs::default(),
            result_error: None,
            last_sql: String::new(),
            last_elapsed: None,
            run_started: None,
            run_cancel: None,
            skipped_statements: 0,
            format_undo: None,
            grid_id: None,
            routine: None,
            view: None,
            batch: None,
        }
    }

    /// The byte range of the current selection, normalised to `start..end`.
    pub fn selection(&self) -> (usize, usize) {
        (self.anchor.min(self.caret), self.anchor.max(self.caret))
    }

    /// Whether this tab has edits that have not been saved (or that were never persisted).
    pub fn is_dirty(&self) -> bool {
        self.sql != self.baseline
    }

    /// Every grid this tab created: a single-connection query's result sets, or every connection
    /// page of a group query.
    pub fn all_grid_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self.result_grids.iter().flatten().copied().collect();
        if let Some(batch) = &self.batch {
            for id in batch.grid_ids() {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        if let Some(id) = self.grid_id
            && !ids.contains(&id)
        {
            ids.push(id);
        }
        ids
    }

    /// Whether the bottom result panel has any grid to export.
    pub fn has_result_grid(&self) -> bool {
        self.result_grids.iter().any(|grid| grid.is_some())
            || self
                .batch
                .as_ref()
                .is_some_and(|batch| batch.has_result_grid())
    }
}

/// One cell of an open notebook: the model cell plus UI/run state that is never saved.
#[allow(dead_code)] // wired to the UI and execution in later notebook stages
pub struct NotebookCellState {
    pub cell: rustgrid_notebook::Cell,
    /// Markdown cells: editing the source instead of showing it rendered.
    pub editing: bool,
    /// The grid id in `AppView::grids` for each of the last run's result sets (`None` for a
    /// statement that produced no result set).
    pub result_grids: Vec<Option<u64>>,
    /// The last run's error, if it failed.
    pub error: Option<String>,
    /// How long the last run took.
    pub elapsed: Option<std::time::Duration>,
    /// Parameters cells: the rows of the name / value table. A row whose name is not a valid
    /// parameter name (still being typed, or blank) stays here but is left out of the source.
    pub param_rows: Vec<rustgrid_notebook::Param>,
}

#[allow(dead_code)] // wired to the UI and execution in later notebook stages
impl NotebookCellState {
    pub fn new(cell: rustgrid_notebook::Cell) -> Self {
        let mut param_rows = cell.parameters();
        // A new parameters cell starts with one row to type into.
        if cell.kind == rustgrid_notebook::CellKind::Parameters && param_rows.is_empty() {
            param_rows.push(blank_param());
        }
        Self {
            cell,
            editing: false,
            result_grids: Vec::new(),
            error: None,
            elapsed: None,
            param_rows,
        }
    }

    /// Rewrite a parameters cell's source from its rows; returns whether it changed.
    fn sync_param_source(&mut self) -> bool {
        let valid: Vec<_> = self
            .param_rows
            .iter()
            .filter(|param| rustgrid_notebook::is_param_name(&param.name))
            .cloned()
            .collect();
        let source = rustgrid_notebook::format_parameters(&valid);
        if source == self.cell.source {
            return false;
        }
        self.cell.source = source;
        true
    }
}

fn blank_param() -> rustgrid_notebook::Param {
    rustgrid_notebook::Param {
        name: String::new(),
        value: rustgrid_notebook::ParamValue::Text(String::new()),
    }
}

/// An open SQL notebook tab. The cells live in `cells`; everything else the file carries (unknown
/// metadata, format version, unknown top-level keys) stays in `base` so saving loses nothing.
#[allow(dead_code)] // wired to the UI and execution in later notebook stages
pub struct NotebookTab {
    pub id: u64,
    /// The `.ipynb` file this tab is bound to; `None` until it is first saved.
    pub path: Option<std::path::PathBuf>,
    pub connection_index: Option<usize>,
    pub database: Option<String>,
    pub schema: Option<String>,
    pub cells: Vec<NotebookCellState>,
    /// The selected cell.
    pub selected: usize,
    /// The index of the cell being run, if any.
    pub running: Option<usize>,
    /// Bumped on every run and on stop; a late result whose generation no longer matches is
    /// discarded (same pattern as [`QueryTab::run_generation`]).
    pub run_generation: u64,
    /// The last `execution_count` handed out; the next run uses this plus one.
    pub execution_counter: u32,
    /// Unsaved edits exist.
    pub dirty: bool,
    /// Set by Stop: the running cell skips its remaining statements.
    pub run_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Run All: the ids of the SQL cells still to run after the running one, in order.
    run_queue: std::collections::VecDeque<String>,
    /// A Run All is in progress (its queue may already be empty while its last cell runs).
    running_all: bool,
    /// The connection id the file was loaded with. The id may name a profile this machine does
    /// not have (`connection_index` is then `None`), and it must survive a save so the binding is
    /// not stripped from the file; an explicit [`NotebookTab::set_binding`] clears it.
    loaded_connection_id: Option<String>,
    /// The loaded notebook with its cells and binding removed: carries the metadata, format
    /// version and top-level keys that [`NotebookTab::to_notebook`] writes back unchanged.
    base: rustgrid_notebook::Notebook,
    /// The file's `%load_ext sql` cells: kept out of the cell list (they are plumbing for
    /// JupySQL, not content) and written back first, unchanged, on save.
    loader_cells: Vec<rustgrid_notebook::Cell>,
}

#[allow(dead_code)] // wired to the UI and execution in later notebook stages
impl NotebookTab {
    pub fn from_notebook(
        id: u64,
        mut notebook: rustgrid_notebook::Notebook,
        path: Option<std::path::PathBuf>,
        connection_index: Option<usize>,
    ) -> Self {
        let (loader_cells, cells): (Vec<_>, Vec<_>) = std::mem::take(&mut notebook.cells)
            .into_iter()
            .partition(rustgrid_notebook::Cell::is_sql_extension_loader);
        let binding = notebook.binding.take();
        let execution_counter = cells
            .iter()
            .filter_map(|cell| cell.execution_count)
            .max()
            .unwrap_or(0);
        Self {
            id,
            path,
            connection_index,
            database: binding.as_ref().and_then(|b| b.database.clone()),
            schema: binding.as_ref().and_then(|b| b.schema.clone()),
            loaded_connection_id: binding.map(|b| b.connection_id),
            cells: cells.into_iter().map(NotebookCellState::new).collect(),
            selected: 0,
            running: None,
            run_generation: 0,
            execution_counter,
            dirty: false,
            run_cancel: None,
            run_queue: std::collections::VecDeque::new(),
            running_all: false,
            base: notebook,
            loader_cells,
        }
    }

    /// The notebook to write to disk. The caller resolves `connection_id` from the bound
    /// connection profile. `None` means "could not resolve": the connection id the file was
    /// loaded with is kept, so an unknown profile never strips the binding. A deliberate unbind
    /// goes through [`NotebookTab::set_binding`], which forgets the loaded id.
    pub fn to_notebook(&self, connection_id: Option<&str>) -> rustgrid_notebook::Notebook {
        let mut notebook = self.base.clone();
        notebook.cells = self
            .loader_cells
            .iter()
            .cloned()
            .chain(self.cells.iter().map(|state| state.cell.clone()))
            .collect();
        let connection_id = connection_id
            .map(str::to_owned)
            .or_else(|| self.loaded_connection_id.clone());
        notebook.binding = connection_id.map(|id| rustgrid_notebook::NotebookBinding {
            connection_id: id,
            database: self.database.clone(),
            schema: self.schema.clone(),
        });
        notebook
    }

    /// Mirror an editor's text into the cell `cell_id`; only a real change marks the tab dirty.
    pub fn set_cell_source_by_id(&mut self, cell_id: &str, source: &str) {
        if let Some(state) = self.cells.iter_mut().find(|state| state.cell.id == cell_id)
            && state.cell.source != source
        {
            state.cell.source = source.to_string();
            self.dirty = true;
        }
    }

    /// Edit row `row` of the parameters cell `cell_id`: its name and/or its value (typed text, see
    /// [`rustgrid_notebook::ParamValue::from_input`]). Only a change to the saved source marks
    /// the tab dirty.
    pub fn set_param(
        &mut self,
        cell_id: &str,
        row: usize,
        name: Option<&str>,
        value: Option<&str>,
    ) {
        let Some(state) = self.cells.iter_mut().find(|state| state.cell.id == cell_id) else {
            return;
        };
        let Some(param) = state.param_rows.get_mut(row) else {
            return;
        };
        if let Some(name) = name {
            param.name = name.trim().to_string();
        }
        if let Some(value) = value {
            param.value = rustgrid_notebook::ParamValue::from_input(value);
        }
        if state.sync_param_source() {
            self.dirty = true;
        }
    }

    /// Add an empty row under row `after` of the parameters cell at `index` (at the end when
    /// `after` is past it).
    pub fn add_param_row(&mut self, index: usize, after: usize) {
        if let Some(state) = self.cells.get_mut(index)
            && state.cell.kind == rustgrid_notebook::CellKind::Parameters
        {
            let at = (after + 1).min(state.param_rows.len());
            state.param_rows.insert(at, blank_param());
        }
    }

    /// Remove row `row` of the parameters cell at `index`. The last row is emptied rather than
    /// removed, so the table always has a row to type into.
    pub fn remove_param_row(&mut self, index: usize, row: usize) {
        let Some(state) = self.cells.get_mut(index) else {
            return;
        };
        if row >= state.param_rows.len() {
            return;
        }
        if state.param_rows.len() == 1 {
            state.param_rows[0] = blank_param();
        } else {
            state.param_rows.remove(row);
        }
        if state.sync_param_source() {
            self.dirty = true;
        }
    }

    /// The parameters an SQL cell at `index` sees: those of every parameters cell above it, in
    /// order, so a later definition of a name wins in [`rustgrid_notebook::substitute`].
    pub fn params_before(&self, index: usize) -> Vec<rustgrid_notebook::Param> {
        self.cells
            .iter()
            .take(index)
            .flat_map(|state| state.cell.parameters())
            .collect()
    }

    /// Run All from `from`: queue every SQL cell at or after it and return the first one to run.
    pub fn queue_run_all(&mut self, from: usize) -> Option<String> {
        self.run_queue = self
            .cells
            .iter()
            .skip(from)
            .filter(|state| state.cell.kind == rustgrid_notebook::CellKind::Sql)
            .map(|state| state.cell.id.clone())
            .collect();
        self.running_all = true;
        self.run_queue.pop_front()
    }

    /// After the cell `finished` ended: the next cell to run, or `None` when the queue is done. A
    /// failed cell ends Run All (as in Jupyter): the rest of the queue is dropped and the failed
    /// cell is selected (and unfolded), so the error is what the user looks at.
    pub fn next_queued(&mut self, finished: &str, succeeded: bool) -> Option<String> {
        if !succeeded {
            if self.running_all
                && let Some(index) = self
                    .cells
                    .iter()
                    .position(|state| state.cell.id == finished)
            {
                self.select_cell(index);
            }
            self.run_queue.clear();
        }
        let next = self.run_queue.pop_front();
        if next.is_none() {
            self.running_all = false;
        }
        next
    }

    pub fn set_cell_source(&mut self, index: usize, source: String) {
        if let Some(state) = self.cells.get_mut(index) {
            state.cell.source = source;
            self.dirty = true;
        }
    }

    /// Insert at `index` (clamped to the end) and select the new cell.
    pub fn insert_cell(&mut self, index: usize, cell: rustgrid_notebook::Cell) {
        let index = index.min(self.cells.len());
        let markdown = cell.kind == rustgrid_notebook::CellKind::Markdown;
        let mut state = NotebookCellState::new(cell);
        // A new Markdown cell opens in its editor: there is nothing to render yet.
        state.editing = markdown;
        self.cells.insert(index, state);
        if let Some(running) = self.running.as_mut()
            && *running >= index
        {
            *running += 1;
        }
        self.selected = index;
        self.dirty = true;
        // A cell added inside a folded section must not be typed into while invisible.
        self.reveal_cell(index);
    }

    /// Remove the cell at `index`. Returns the ids of the removed cell's result grids, which the
    /// caller must drop from `AppView::grids`. The running cell cannot be removed.
    #[must_use]
    pub fn remove_cell(&mut self, index: usize) -> Vec<u64> {
        if index >= self.cells.len() || self.running == Some(index) {
            return Vec::new();
        }
        let removed = self.cells.remove(index);
        if let Some(running) = self.running.as_mut()
            && *running > index
        {
            *running -= 1;
        }
        if index < self.selected {
            self.selected -= 1;
        }
        self.selected = self.selected.min(self.cells.len().saturating_sub(1));
        self.dirty = true;
        removed.result_grids.into_iter().flatten().collect()
    }

    /// Set the result grids of the cell `cell_id` and return the ids of the grids it showed
    /// before, which the caller must drop from `AppView::grids`. An unknown cell returns `grids`'
    /// own ids, so nothing the caller created is left without an owner.
    #[must_use]
    pub fn replace_result_grids(&mut self, cell_id: &str, grids: Vec<Option<u64>>) -> Vec<u64> {
        match self.cells.iter_mut().find(|state| state.cell.id == cell_id) {
            Some(state) => std::mem::replace(&mut state.result_grids, grids)
                .into_iter()
                .flatten()
                .collect(),
            None => grids.into_iter().flatten().collect(),
        }
    }

    /// Take every result grid id the notebook's cells own, leaving the cells without grids (their
    /// saved `outputs` stay). The caller drops the returned grids from `AppView::grids`.
    #[must_use]
    pub fn take_result_grids(&mut self) -> Vec<u64> {
        self.cells
            .iter_mut()
            .flat_map(|state| {
                std::mem::take(&mut state.result_grids)
                    .into_iter()
                    .flatten()
            })
            .collect()
    }

    /// Keep `connection_index` naming the same profile after the connection at `removed` was
    /// deleted from the connection list. A notebook bound to the deleted connection is left
    /// unbound (its cells and the binding the file was loaded with are kept). Returns whether it
    /// was bound to it.
    pub fn connection_removed(&mut self, removed: usize) -> bool {
        match self.connection_index {
            Some(index) if index == removed => {
                self.connection_index = None;
                true
            }
            Some(index) if index > removed => {
                self.connection_index = Some(index - 1);
                false
            }
            _ => false,
        }
    }

    /// Move a cell to position `to` and keep it selected.
    pub fn move_cell(&mut self, from: usize, to: usize) {
        if from >= self.cells.len() || to >= self.cells.len() || from == to {
            return;
        }
        let state = self.cells.remove(from);
        self.cells.insert(to, state);
        // The running cell keeps pointing at the same cell.
        if let Some(running) = self.running.as_mut() {
            if *running == from {
                *running = to;
            } else if from < *running && *running <= to {
                *running -= 1;
            } else if to <= *running && *running < from {
                *running += 1;
            }
        }
        self.selected = to;
        self.dirty = true;
        self.reveal_cell(to);
    }

    /// Turn the cell at `index` into an SQL or Markdown cell. Its outputs and execution count go
    /// (they belonged to the other kind); the ids of its result grids are returned for the caller
    /// to drop. A Raw cell, the running cell and a no-op change are left alone.
    #[must_use]
    pub fn set_cell_kind(&mut self, index: usize, kind: rustgrid_notebook::CellKind) -> Vec<u64> {
        use rustgrid_notebook::CellKind;
        if matches!(kind, CellKind::Raw | CellKind::Parameters) || self.running == Some(index) {
            return Vec::new();
        }
        let Some(state) = self.cells.get_mut(index) else {
            return Vec::new();
        };
        if state.cell.kind == kind
            || matches!(state.cell.kind, CellKind::Raw | CellKind::Parameters)
        {
            return Vec::new();
        }
        state.cell.kind = kind;
        state.cell.magic_args.clear();
        state.cell.outputs.clear();
        state.cell.execution_count = None;
        state.error = None;
        state.elapsed = None;
        state.editing = kind == CellKind::Markdown;
        // A folded heading turned into SQL no longer heads a section; do not let it come back
        // folded if it is turned into Markdown again.
        state.cell.set_collapsed(false);
        self.dirty = true;
        std::mem::take(&mut state.result_grids)
            .into_iter()
            .flatten()
            .collect()
    }

    /// Show the Markdown cell at `index` as its editor (`true`) or rendered (`false`).
    pub fn set_markdown_editing(&mut self, index: usize, editing: bool) {
        if let Some(state) = self.cells.get_mut(index)
            && state.cell.kind == rustgrid_notebook::CellKind::Markdown
        {
            state.editing = editing;
        }
    }

    /// Select the cell at `index`. Leaving a Markdown cell that is being edited renders it again
    /// (an empty one stays in its editor). Returns whether anything changed.
    pub fn select_cell(&mut self, index: usize) -> bool {
        if index >= self.cells.len() || index == self.selected {
            return false;
        }
        if let Some(previous) = self.cells.get_mut(self.selected)
            && previous.editing
            && !previous.cell.source.trim().is_empty()
        {
            previous.editing = false;
        }
        self.selected = index;
        self.reveal_cell(index);
        true
    }

    /// Which cells a folded section hides (one flag per cell).
    pub fn hidden_cells(&self) -> Vec<bool> {
        rustgrid_notebook::hidden_cells(self.cells.iter().map(|state| &state.cell))
    }

    /// The index just past the section headed by the cell at `index`.
    pub fn section_end(&self, index: usize) -> usize {
        rustgrid_notebook::section_end(self.cells.iter().map(|state| &state.cell), index)
    }

    /// Fold or unfold the section headed by the Markdown heading at `index`. A selection the fold
    /// hides moves onto the heading. Returns whether anything changed (a non-heading cannot fold).
    pub fn toggle_section(&mut self, index: usize) -> bool {
        let end = self.section_end(index);
        let Some(state) = self.cells.get_mut(index) else {
            return false;
        };
        if state.cell.heading_level().is_none() {
            return false;
        }
        let collapse = !state.cell.is_collapsed();
        state.cell.set_collapsed(collapse);
        if collapse && (index + 1..end).contains(&self.selected) {
            self.selected = index;
        }
        self.dirty = true;
        true
    }

    /// Where a cell added "below" the cell at `index` goes: after a folded section's last cell
    /// (as Colab does), so adding never unfolds it; otherwise right under the cell.
    pub fn insert_point_after(&self, index: usize) -> usize {
        match self.cells.get(index) {
            Some(state) if state.cell.is_collapsed() => self.section_end(index),
            Some(_) => index + 1,
            None => self.cells.len(),
        }
    }

    /// The first cell after `index` that no folded section hides.
    pub fn next_visible(&self, index: usize) -> Option<usize> {
        let hidden = self.hidden_cells();
        (index + 1..self.cells.len()).find(|&next| !hidden[next])
    }

    /// Unfold every section that hides the cell at `index`, so it can be seen and edited.
    pub fn reveal_cell(&mut self, index: usize) {
        for heading in 0..index.min(self.cells.len()) {
            if self.cells[heading].cell.is_collapsed() && self.section_end(heading) > index {
                self.cells[heading].cell.set_collapsed(false);
                self.dirty = true;
            }
        }
    }

    /// Mark the cell `cell_id` as running and return the run's generation and cancel flag. The
    /// caller hands both back to [`NotebookTab::finish_run`] / checks the flag between statements.
    pub fn begin_run(
        &mut self,
        cell_id: &str,
    ) -> Option<(u64, Arc<std::sync::atomic::AtomicBool>)> {
        let index = self
            .cells
            .iter()
            .position(|state| state.cell.id == cell_id)?;
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.running = Some(index);
        self.run_generation = self.run_generation.wrapping_add(1);
        self.run_cancel = Some(cancel.clone());
        let state = &mut self.cells[index];
        state.error = None;
        state.elapsed = None;
        Some((self.run_generation, cancel))
    }

    /// Install a finished run on the cell `cell_id` (found by id, so a cell moved while it ran
    /// still gets its own results). `results` are the result sets of the statements that ran,
    /// `error` the failure that stopped the run, if any. Returns `false` — and changes nothing —
    /// when the run is stale (Stop/Restart bumped the generation) or the cell was removed.
    pub fn finish_run(
        &mut self,
        cell_id: &str,
        generation: u64,
        elapsed: std::time::Duration,
        results: &[rustgrid_core::QueryResult],
        error: Option<String>,
        max_rows: usize,
    ) -> bool {
        if generation != self.run_generation {
            return false;
        }
        self.running = None;
        self.run_cancel = None;
        let Some(state) = self.cells.iter_mut().find(|state| state.cell.id == cell_id) else {
            return false;
        };
        self.execution_counter = self.execution_counter.saturating_add(1);
        let mut outputs = rustgrid_notebook::results_to_outputs(results, max_rows);
        if let Some(message) = &error {
            outputs.push(rustgrid_notebook::CellOutput::Error {
                message: message.clone(),
            });
        }
        state.cell.outputs = outputs;
        state.cell.execution_count = Some(self.execution_counter);
        state.error = error;
        state.elapsed = Some(elapsed);
        self.dirty = true;
        true
    }

    /// Fail the cell `cell_id` without running it (e.g. the connection is not open).
    pub fn fail_cell(&mut self, cell_id: &str, message: String) {
        if let Some(state) = self.cells.iter_mut().find(|state| state.cell.id == cell_id) {
            state.cell.outputs = vec![rustgrid_notebook::CellOutput::Error {
                message: message.clone(),
            }];
            state.error = Some(message);
            state.elapsed = None;
            self.dirty = true;
        }
    }

    /// Stop the running cell: its remaining statements are skipped and its late result is
    /// discarded. Returns whether a run was in progress.
    pub fn stop_run(&mut self) -> bool {
        let was_running = self.running.is_some();
        if let Some(cancel) = self.run_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.run_queue.clear();
        self.running_all = false;
        self.run_generation = self.run_generation.wrapping_add(1);
        self.running = None;
        was_running
    }

    /// The connection id the notebook was loaded or last saved with, kept while its connection is
    /// missing so a later save (or the session) does not lose it.
    pub fn saved_connection_id(&self) -> Option<&str> {
        self.loaded_connection_id.as_deref()
    }

    /// The notebook was written to `path` bound to `connection_id`: it is clean and that file is
    /// now where Save writes.
    pub fn mark_saved(&mut self, path: std::path::PathBuf, connection_id: Option<String>) {
        self.path = Some(path);
        if connection_id.is_some() {
            self.loaded_connection_id = connection_id;
        }
        self.dirty = false;
    }

    pub fn set_binding(
        &mut self,
        connection_index: Option<usize>,
        database: Option<String>,
        schema: Option<String>,
    ) {
        self.connection_index = connection_index;
        self.loaded_connection_id = None;
        self.database = database;
        self.schema = schema;
        self.dirty = true;
    }
}

/// A result set from a query run, with the editability and target table resolved from the
/// statement it came from. Built on the async side (where the catalog lookup happens) and handed to
/// `AppView::apply_query_results` to create one grid per entry.
pub struct QueryResultPlan {
    pub result: QueryResult,
    pub editable: bool,
    /// The database the grid's edits should target (the statement's schema, else the query's).
    pub database: String,
    /// The single table inferred from the statement, empty when it is not editable.
    pub table: String,
}

/// A cheap snapshot of one executed statement, enough for the 信息 tab. Kept separate from
/// [`QueryResult`] so the rows are not held twice (the grid owns them).
#[derive(Clone)]
pub struct QueryResultSummary {
    /// The statement this entry reports on, as sent.
    pub statement: String,
    /// How long that statement took.
    pub elapsed: std::time::Duration,
    /// The statement's error, when it failed (then there is no result set).
    pub error: Option<String>,
    pub has_result_set: bool,
    pub row_count: usize,
    pub rows_affected: u64,
}

/// One entry of a query run, built on the async side: a statement's result set (or its
/// outcome), or its error. A statement that returned several result sets yields several entries.
pub struct RunEntry {
    pub statement: String,
    pub elapsed: std::time::Duration,
    pub outcome: Result<QueryResultPlan, String>,
}

/// What a run executes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RunScope {
    /// The whole editor.
    All,
    /// The selection when there is one, else the whole editor (Navicat's Run, Ctrl+R).
    SelectionOrAll,
    /// The selection when there is one, else the statement at the caret (Run Selected).
    SelectionOrStatement,
}

/// One undo step: the touched cells and their previous pending edit value
/// (`None` means there was no pending edit for that cell).
pub type EditAction = Vec<((usize, usize), Option<Option<String>>)>;

/// One row of the sort criteria panel: which column, which direction, and whether it is
/// currently part of the query's `ORDER BY`.
#[derive(Clone, PartialEq, Eq)]
pub struct SortRule {
    pub column: String,
    pub descending: bool,
    pub enabled: bool,
}

impl SortRule {
    pub fn new(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            descending: false,
            enabled: true,
        }
    }
}

/// A table grid's field layout: the order every field is displayed in, and which of them are
/// hidden. Applied by projecting `GridState::columns`/`column_widths` out of the table's natural
/// column order; `GridState::rows` stay in natural order, so changing the layout re-projects
/// metadata only (no refetch and no row copy).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct GridFieldLayout {
    /// Every field name in display order (fields the layout does not know are appended).
    pub order: Vec<String>,
    /// The hidden field names.
    pub hidden: BTreeSet<String>,
}

impl GridFieldLayout {
    /// The layout's display order resolved against the engine's natural `columns`: the layout's own
    /// order first (restricted to fields the table actually has), then every remaining field in
    /// table order.
    pub fn resolved_order(&self, columns: &[ColumnInfo]) -> Vec<String> {
        let mut order: Vec<String> = Vec::with_capacity(columns.len());
        for name in &self.order {
            if !order.contains(name) && columns.iter().any(|column| &column.name == name) {
                order.push(name.clone());
            }
        }
        for column in columns {
            if !order.contains(&column.name) {
                order.push(column.name.clone());
            }
        }
        order
    }

    /// Whether the layout leaves the table exactly as the engine returns it: no field hidden and no
    /// field out of place. Drives the toolbar icon's "configured" state.
    pub fn is_default(&self, columns: &[ColumnInfo]) -> bool {
        if !self.hidden.is_empty() {
            return false;
        }
        if self.order.is_empty() {
            return true;
        }
        self.order.len() == columns.len()
            && self
                .order
                .iter()
                .zip(columns)
                .all(|(name, column)| *name == column.name)
    }

    /// The display order with the frozen fields pinned to the front, in the order they were
    /// frozen, then every other field in the layout's own order. The frozen names are kept out of
    /// [`Self::order`] by the caller, so unfreezing restores each field's original position
    /// without another snapshot.
    pub fn effective_order(&self, columns: &[ColumnInfo], frozen: &[String]) -> Vec<String> {
        let order = self.resolved_order(columns);
        let mut effective: Vec<String> = Vec::with_capacity(order.len());
        for name in frozen {
            if self.hidden.contains(name) {
                continue;
            }
            if columns.iter().any(|column| &column.name == name) && !effective.contains(name) {
                effective.push(name.clone());
            }
        }
        for name in order {
            if !effective.contains(&name) {
                effective.push(name);
            }
        }
        effective
    }

    /// Like [`Self::permutation`], but with `frozen`'s visible fields pinned to the front of the
    /// display order. `frozen` is in the order the user froze them, so the first frozen field is
    /// the first column, the second the second, and so on.
    pub fn permutation_with_frozen(
        &self,
        columns: &[ColumnInfo],
        frozen: &[String],
    ) -> (Vec<usize>, usize) {
        let order = self.effective_order(columns, frozen);
        self.permutation_for_order(columns, &order)
    }

    fn permutation_for_order(
        &self,
        columns: &[ColumnInfo],
        order: &[String],
    ) -> (Vec<usize>, usize) {
        let mut visible: Vec<usize> = Vec::with_capacity(columns.len());
        let mut hidden: Vec<usize> = Vec::new();
        for name in order {
            let Some(index) = columns.iter().position(|column| &column.name == name) else {
                continue;
            };
            if self.hidden.contains(name) {
                hidden.push(index);
            } else {
                visible.push(index);
            }
        }
        if visible.is_empty() {
            return ((0..columns.len()).collect(), columns.len());
        }
        let visible_count = visible.len();
        visible.extend(hidden);
        (visible, visible_count)
    }

    /// Build the persisted layout from a display order that starts with the frozen fields. The
    /// frozen names are taken out of `display` and put back at the positions they hold in this
    /// layout, so the field order a 取消冻结 restores is the one that existed before freezing.
    pub fn with_display_order(
        &self,
        display: &[String],
        hidden: BTreeSet<String>,
        frozen: &[String],
        columns: &[ColumnInfo],
    ) -> Self {
        let previous = self.resolved_order(columns);
        let mut order: Vec<String> = display
            .iter()
            .filter(|name| !frozen.iter().any(|field| field == *name))
            .cloned()
            .collect();
        for name in frozen {
            let position = previous
                .iter()
                .position(|field| field == name)
                .unwrap_or(order.len())
                .min(order.len());
            order.insert(position, name.clone());
        }
        Self { order, hidden }
    }

    /// Show or hide one field.
    pub fn set_hidden(&mut self, name: &str, hidden: bool) {
        if hidden {
            self.hidden.insert(name.to_string());
        } else {
            self.hidden.remove(name);
        }
    }

    /// Move the field at `from` to `to` in the resolved display order.
    pub fn move_field(&mut self, order: &[String], from: usize, to: usize) {
        if from == to || from >= order.len() || to >= order.len() {
            return;
        }
        let mut next = order.to_vec();
        let name = next.remove(from);
        next.insert(to.min(next.len()), name);
        self.order = next;
    }

    /// Flip every field's visibility (反选). The inversion never hides the last visible field: when
    /// every field was shown the first one stays, so a grid can never end up with no columns.
    pub fn invert_visibility(&mut self) {
        if self.order.len() <= 1 {
            return;
        }
        let mut inverted: BTreeSet<String> = self
            .order
            .iter()
            .filter(|name| !self.hidden.contains(*name))
            .cloned()
            .collect();
        if inverted.len() >= self.order.len() {
            inverted.remove(&self.order[0]);
        }
        self.hidden = inverted;
    }
}

/// Apply `order` (the display position each element's *source* index belongs at) to `values` in
/// place, walking the permutation's cycles so no element is copied. A non-permutation is ignored,
/// which also keeps the cycle walk from looping forever.
pub fn permute_in_place<T>(values: &mut [T], order: &[usize]) {
    if values.len() != order.len() {
        return;
    }
    // The cycle walk needs where each element goes, i.e. the inverse of `order`.
    let mut dest = vec![usize::MAX; order.len()];
    for (slot, source) in order.iter().enumerate() {
        if *source >= order.len() || dest[*source] != usize::MAX {
            return;
        }
        dest[*source] = slot;
    }
    for index in 0..dest.len() {
        while dest[index] != index {
            let other = dest[index];
            values.swap(index, other);
            dest.swap(index, other);
        }
    }
}

/// The permutation to apply to the currently displayed columns (whose display order is
/// `current_order`, each entry indexing the table's natural columns) to put them into
/// `next_order`. Composing through the current positions is what lets `reproject_fields` re-order
/// `columns`/`column_widths` in place without knowing which field each entry is.
fn field_permutation(current_order: &[usize], next_order: &[usize], count: usize) -> Vec<usize> {
    let mut position = vec![0usize; count];
    for (slot, source) in current_order.iter().enumerate() {
        if *source < count {
            position[*source] = slot;
        }
    }
    next_order
        .iter()
        .map(|source| position.get(*source).copied().unwrap_or(*source))
        .collect()
}

/// The engine-agnostic state of one grid tab: the loaded page, the pending edits, and the
/// presentation state (selection, filters, sort, field layout).
pub struct GridState {
    pub id: u64,
    pub connection: Arc<dyn Connection>,
    /// The engine's SQL dialect, for the status line's shown statement (identifier quoting and
    /// paging syntax), so it reads the engine's own SQL instead of MySQL's.
    pub dialect: DriverDialect,
    pub connection_name: String,
    /// The connection profile's id, which keys the persisted field layout. Empty for a grid whose
    /// columns are not a table's (a query result), so no field layout is applied to it.
    pub connection_id: String,
    pub database: String,
    pub table: String,
    pub is_view: bool,
    /// The key-value object's overview (type / TTL / memory), when this grid shows a key. `None`
    /// for SQL tables and query results.
    pub key_status: Option<rustgrid_core::TableStatus>,
    /// Whether this grid's connection is a document store (MongoDB). The row form then offers the
    /// JSON view, which shows the selected row as one document instead of per-field editors.
    pub documents: bool,
    /// The selected row's full document, as pretty JSON (document stores only). Loaded on demand
    /// by the row form because the grid's columns are only a sample of the document's fields.
    pub document_json: Option<String>,
    /// The row key `document_json` was loaded for, so a selection change reloads it.
    pub document_key: Option<String>,
    /// Whether a document fetch is in flight.
    pub document_loading: bool,
    /// The last document fetch/save error, shown above the JSON editor.
    pub document_error: Option<String>,
    /// Unsaved JSON edits per page row (document stores). Like `edits`, they stay staged while the
    /// user moves around the page and are dropped by a reload.
    pub document_edits: BTreeMap<usize, String>,
    pub page_index: u64,
    pub page_size: u64,
    pub loading: bool,
    pub error: Option<String>,
    /// The displayed columns: the table's columns re-ordered by [`Self::field_layout`], with the
    /// hidden ones kept at the tail so unhiding them needs no refetch.
    pub columns: Vec<ColumnInfo>,
    /// The table's columns exactly as the engine returns them (natural order). The 字段筛选 popup
    /// lists these, and a layout change is resolved against them.
    pub all_columns: Vec<ColumnInfo>,
    /// The applied field layout (display order plus the hidden set).
    pub field_layout: GridFieldLayout,
    /// Display position -> index into [`Self::all_columns`], for the whole column list (hidden
    /// fields included).
    pub column_order: Vec<usize>,
    /// How many leading entries of [`Self::columns`] are visible; the rest are hidden fields.
    pub visible_columns: usize,
    /// The fields the user froze from the header's context menu, in the order they were frozen.
    /// They are pinned to the front of the display order (first frozen field = first column) and
    /// stay put while the other columns pan horizontally. Kept by name so a field-layout change
    /// (hide/reorder) never points them at the wrong column; [`Self::field_layout`] itself keeps
    /// the unfrozen order, so clearing this list restores every field to its original place.
    pub frozen_columns: Vec<String>,
    pub rows: Arc<Vec<Vec<CellValue>>>,
    pub column_widths: Vec<f32>,
    /// Set once the user drags a column edge. Keeps the widths for the rest of this grid's life
    /// so paging/refreshing does not snap them back to the auto-fitted values.
    pub manual_column_widths: bool,
    pub total_rows: Option<u64>,
    pub selection: Option<CellSelection>,
    pub edits: BTreeMap<(usize, usize), Option<String>>,
    /// Undo history: each entry restores the prior edit value (or lack thereof) for its cells.
    pub undo: Vec<EditAction>,
    /// Redo history: inverse actions pushed by undo, cleared on any new edit.
    pub redo: Vec<EditAction>,
    /// `Some` when this grid presents an ad-hoc query result instead of a table page.
    pub sql: Option<String>,
    /// Whether the grid toolbar (transaction/filter/sort/...) is shown.
    pub show_toolbar: bool,
    /// Whether the grid's bottom action bar and status line are shown. Off for grids embedded in
    /// another view (e.g. the view designer's explain panel), where they would be redundant.
    pub show_footer: bool,
    /// Whether cells may be edited. Query grids only allow this with an inferred table.
    pub editable: bool,
    /// The `ORDER BY` applied to the table when pages are fetched.
    pub sort_rules: Vec<SortRule>,
    /// Whether the sort criteria panel is expanded below the toolbar.
    pub sort_open: bool,
    /// The panel's working copy of the rules while it is open; applied on `Apply`.
    pub sort_draft: Vec<SortRule>,
    /// The `WHERE` tree applied to the table when pages are fetched.
    pub filters: Vec<FilterNode>,
    /// Whether the filter builder panel is expanded below the toolbar.
    pub filter_open: bool,
    /// The panel's working copy of the tree while it is open; applied on `Apply`.
    pub filter_draft: Vec<FilterNode>,
    /// Wall-clock execution time of the statement that produced the rows currently on screen
    /// (a query result, or the table page's `fetch_page`), shown beside the pager.
    pub elapsed: Option<std::time::Duration>,
}

pub fn compute_column_widths(columns: &[ColumnInfo], rows: &[Vec<CellValue>]) -> Vec<f32> {
    let mut widths: Vec<usize> = columns
        .iter()
        .map(|column| display_width(&column.name))
        .collect();

    for row in rows.iter().take(200) {
        for (index, cell) in row.iter().enumerate() {
            let Some(slot) = widths.get_mut(index) else {
                break;
            };
            let text = cell.as_display();
            let first_line: String = text.lines().next().unwrap_or("").chars().take(48).collect();
            *slot = (*slot).max(display_width(&first_line));
        }
    }

    widths
        .into_iter()
        .map(|width| (width as f32 * 7.2 + 22.0 + SORT_BADGE_ALLOWANCE).clamp(56.0, 260.0))
        .collect()
}

/// Horizontal room the header reserves for its sort badge (icon + gap), so column names are
/// not truncated when the badge is revealed.
const SORT_BADGE_ALLOWANCE: f32 = 20.0;

/// The natural-order widths for a freshly fetched page: content-fitted, unless the user has dragged
/// column edges — then each column keeps the width it had, matched by name so paging/sorting does
/// not snap the widths back. The same carry-over applies when the page keeps the same column set:
/// sorting changes the rows, not the schema, so a column must not shrink because the new first page
/// happens to hold shorter values (its later values then clipped).
pub fn page_column_widths(
    columns: &[ColumnInfo],
    rows: &[Vec<CellValue>],
    previous: &[ColumnInfo],
    previous_widths: &[f32],
    manual: bool,
) -> Vec<f32> {
    let mut widths = compute_column_widths(columns, rows);
    let same_columns = previous.len() == columns.len()
        && columns.iter().all(|column| {
            previous
                .iter()
                .any(|candidate| candidate.name == column.name)
        });
    if manual || same_columns {
        for (index, column) in columns.iter().enumerate() {
            let carried = previous
                .iter()
                .position(|candidate| candidate.name == column.name)
                .and_then(|slot| previous_widths.get(slot).copied());
            if let Some(width) = carried {
                widths[index] = width;
            }
        }
    }
    widths
}

fn display_width(text: &str) -> usize {
    text.chars()
        .map(|character| if (character as u32) >= 0x1100 { 2 } else { 1 })
        .sum()
}

fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Quote a possibly schema-qualified name (`public.users`) part by part, so the display shows
/// `"public"."users"` rather than one identifier with a dot inside it.
fn quote_qualified(dialect: DriverDialect, name: &str) -> String {
    name.split('.')
        .map(|part| dialect.quote_identifier(part))
        .collect::<Vec<_>>()
        .join(".")
}

/// The `SELECT` a table page runs, written in `dialect`'s own SQL so the grid's status line is
/// truthful for every engine: identifiers quoted the engine's way, and paging in the engine's
/// syntax (MySQL's `LIMIT o,n`, PostgreSQL/SQLite's `LIMIT n OFFSET o`, SQL Server/Oracle's
/// `OFFSET … FETCH NEXT`).
///
/// A schema-qualified engine's table name already carries its schema (`public.users`), and the
/// database is the connection's context rather than a name prefix (PostgreSQL/Oracle reject a
/// three-part name). MySQL qualifies with the database and SQLite with nothing, since its
/// "database" is the file path.
#[allow(clippy::too_many_arguments)]
pub(crate) fn page_display_sql(
    dialect: DriverDialect,
    database: &str,
    table: &str,
    filters: &[FilterNode],
    sorts: &[SortColumn],
    columns: &[ColumnInfo],
    offset: u64,
    page_size: u64,
) -> String {
    let relation = match dialect {
        // ClickHouse and MySQL both qualify an object with its database, using backticks.
        DriverDialect::Mysql | DriverDialect::ClickHouse => {
            if database.is_empty() {
                dialect.quote_identifier(table)
            } else {
                format!(
                    "{}.{}",
                    dialect.quote_identifier(database),
                    dialect.quote_identifier(table)
                )
            }
        }
        DriverDialect::Sqlite => dialect.quote_identifier(table),
        DriverDialect::SqlServer => {
            if database.is_empty() {
                quote_qualified(dialect, table)
            } else {
                format!(
                    "{}.{}",
                    dialect.quote_identifier(database),
                    quote_qualified(dialect, table)
                )
            }
        }
        DriverDialect::Postgres | DriverDialect::Oracle => quote_qualified(dialect, table),
        DriverDialect::Generic => {
            if database.is_empty() {
                dialect.quote_identifier(table)
            } else {
                format!(
                    "{}.{}",
                    dialect.quote_identifier(database),
                    dialect.quote_identifier(table)
                )
            }
        }
    };

    let mut sql = format!("SELECT * FROM {relation}");
    sql.push_str(&filter_display_clause(filters, dialect));

    let mut order = sorts
        .iter()
        .map(|sort| {
            format!(
                "{} {}",
                dialect.quote_identifier(&sort.column),
                if sort.descending { "DESC" } else { "ASC" }
            )
        })
        .collect::<Vec<_>>();
    // `OFFSET … FETCH NEXT` is only legal after an `ORDER BY`. With no sort of the user's, the
    // engine orders by the primary key (both drivers do), and without one SQL Server accepts a
    // constant ordering while Oracle takes a positional one.
    if order.is_empty() && dialect.paging_needs_order() {
        order = columns
            .iter()
            .filter(|column| column.primary_key)
            .map(|column| dialect.quote_identifier(&column.name))
            .collect();
        if order.is_empty() {
            order.push(match dialect {
                DriverDialect::Oracle => "1".to_string(),
                _ => "(SELECT NULL)".to_string(),
            });
        }
    }
    if !order.is_empty() {
        sql.push_str(" ORDER BY ");
        sql.push_str(&order.join(", "));
    }

    sql.push(' ');
    sql.push_str(&dialect.paging_clause(offset, page_size));
    sql
}

/// A human-readable ` WHERE ...` for the status bar. Values are inlined here, unlike the
/// parameterized query the driver builds.
fn filter_display_clause(filter: &[FilterNode], dialect: DriverDialect) -> String {
    match filter_display_fragment(filter, dialect) {
        Some(expr) => format!(" WHERE {expr}"),
        None => String::new(),
    }
}

fn filter_display_fragment(filter: &[FilterNode], dialect: DriverDialect) -> Option<String> {
    let mut clauses: Vec<String> = Vec::new();
    for node in filter {
        let piece = match node {
            FilterNode::Condition(condition) => condition_display(condition, dialect),
            FilterNode::Group(group) => {
                if !group.enabled {
                    None
                } else {
                    filter_display_fragment(&group.children, dialect)
                        .map(|inner| format!("({inner})"))
                }
            }
        };
        let Some(piece) = piece else {
            continue;
        };
        if clauses.is_empty() {
            clauses.push(piece);
        } else {
            let conjunction = match node.conjunction() {
                FilterConjunction::And => "AND",
                FilterConjunction::Or => "OR",
            };
            clauses.push(format!("{conjunction} {piece}"));
        }
    }
    (!clauses.is_empty()).then(|| clauses.join(" "))
}

fn condition_display(condition: &FilterCondition, dialect: DriverDialect) -> Option<String> {
    if !condition.enabled || condition.column.is_empty() {
        return None;
    }
    let operator = condition.operator;
    if operator.needs_value() && condition.value.is_empty() {
        return None;
    }
    if operator.needs_second_value() && condition.value2.is_empty() {
        return None;
    }
    let column = dialect.quote_identifier(&condition.column);
    let value = quote_literal(&condition.value);
    let piece = match operator {
        FilterOperator::Equal => format!("{column} = {value}"),
        FilterOperator::NotEqual => format!("{column} <> {value}"),
        FilterOperator::LessThan => format!("{column} < {value}"),
        FilterOperator::LessOrEqual => format!("{column} <= {value}"),
        FilterOperator::GreaterThan => format!("{column} > {value}"),
        FilterOperator::GreaterOrEqual => format!("{column} >= {value}"),
        FilterOperator::Contains => {
            format!(
                "{column} LIKE {}",
                quote_literal(&format!("%{}%", condition.value))
            )
        }
        FilterOperator::NotContains => format!(
            "{column} NOT LIKE {}",
            quote_literal(&format!("%{}%", condition.value))
        ),
        FilterOperator::StartsWith => {
            format!(
                "{column} LIKE {}",
                quote_literal(&format!("{}%", condition.value))
            )
        }
        FilterOperator::NotStartsWith => format!(
            "{column} NOT LIKE {}",
            quote_literal(&format!("{}%", condition.value))
        ),
        FilterOperator::EndsWith => {
            format!(
                "{column} LIKE {}",
                quote_literal(&format!("%{}", condition.value))
            )
        }
        FilterOperator::NotEndsWith => format!(
            "{column} NOT LIKE {}",
            quote_literal(&format!("%{}", condition.value))
        ),
        FilterOperator::IsNull => format!("{column} IS NULL"),
        FilterOperator::IsNotNull => format!("{column} IS NOT NULL"),
        FilterOperator::IsEmpty => format!("({column} IS NULL OR {column} = '')"),
        FilterOperator::IsNotEmpty => format!("({column} IS NOT NULL AND {column} <> '')"),
        FilterOperator::Between => format!(
            "{column} BETWEEN {value} AND {}",
            quote_literal(&condition.value2)
        ),
        FilterOperator::NotBetween => format!(
            "{column} NOT BETWEEN {value} AND {}",
            quote_literal(&condition.value2)
        ),
        FilterOperator::InList => {
            let values = condition.list_values();
            if values.is_empty() {
                return None;
            }
            let list = values
                .iter()
                .map(|item| quote_literal(item))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{column} IN ({list})")
        }
        FilterOperator::NotInList => {
            let values = condition.list_values();
            if values.is_empty() {
                return None;
            }
            let list = values
                .iter()
                .map(|item| quote_literal(item))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{column} NOT IN ({list})")
        }
    };
    Some(piece)
}

impl GridState {
    pub fn has_next(&self) -> bool {
        match self.total_rows {
            Some(total) => (self.page_index + 1) * self.page_size < total,
            None => !self.rows.is_empty(),
        }
    }

    pub fn last_page(&self) -> Option<u64> {
        self.total_rows.map(|total| {
            if total == 0 {
                0
            } else {
                (total - 1) / self.page_size
            }
        })
    }

    pub fn sql(&self) -> String {
        if let Some(sql) = &self.sql {
            return sql.clone();
        }
        let offset = self.page_index.saturating_mul(self.page_size);
        page_display_sql(
            self.dialect,
            &self.database,
            &self.table,
            &self.filters,
            &self.sort_columns(),
            &self.all_columns,
            offset,
            self.page_size,
        )
    }

    /// The filter tree as an engine-agnostic `WHERE`. Disabled/incomplete nodes are pruned here;
    /// the driver also skips them and drops any group left empty.
    pub fn filter_conditions(&self) -> Vec<FilterNode> {
        self.filters.clone()
    }

    /// The enabled sort rules, in panel order, as an engine-agnostic `ORDER BY`.
    pub fn sort_columns(&self) -> Vec<SortColumn> {
        self.sort_rules
            .iter()
            .filter(|rule| rule.enabled && !rule.column.is_empty())
            .map(|rule| SortColumn {
                column: rule.column.clone(),
                descending: rule.descending,
            })
            .collect()
    }

    /// The natural (engine) column index a display column reads its cell values from.
    pub fn source_col(&self, col: usize) -> usize {
        self.column_order.get(col).copied().unwrap_or(col)
    }

    /// How many leading display columns are frozen. The frozen fields are projected to the front
    /// by [`Self::reproject_fields`], so this is also the split between the fixed and pannable
    /// column groups.
    pub fn frozen_count(&self) -> usize {
        self.frozen_columns
            .iter()
            .filter(|name| {
                self.columns
                    .iter()
                    .take(self.visible_columns)
                    .any(|column| &column.name == *name)
            })
            .count()
    }

    /// Whether `name` is one of the fields frozen in this grid.
    pub fn is_frozen_column(&self, name: &str) -> bool {
        self.frozen_columns.iter().any(|frozen| frozen == name)
    }

    /// The display order the 字段筛选 popup shows: frozen fields first, then the layout order.
    pub fn effective_field_order(&self) -> Vec<String> {
        self.field_layout
            .effective_order(&self.all_columns, &self.frozen_columns)
    }

    /// Whether the 字段筛选 popup holds a layout that differs from the table's natural columns.
    pub fn field_layout_configured(&self) -> bool {
        !self.field_layout.is_default(&self.all_columns)
    }

    /// Install a freshly fetched page in the table's natural order and project the field layout on
    /// top of it. `widths` are the natural-order widths; they are re-ordered alongside the columns.
    pub fn install_page(
        &mut self,
        columns: Vec<ColumnInfo>,
        rows: Vec<Vec<CellValue>>,
        widths: Vec<f32>,
    ) {
        self.all_columns = columns;
        self.column_order = (0..self.all_columns.len()).collect();
        self.columns = self.all_columns.clone();
        self.column_widths = widths;
        self.visible_columns = self.columns.len();
        self.rows = Arc::new(rows);
        self.reproject_fields();
    }

    /// Re-order `columns`/`column_widths` into the field layout's display order (visible fields
    /// first). `rows` are untouched: they keep the engine's order and are read through
    /// [`Self::source_col`].
    pub fn reproject_fields(&mut self) {
        if self.all_columns.is_empty() {
            return;
        }
        let (permutation, visible) = self
            .field_layout
            .permutation_with_frozen(&self.all_columns, &self.frozen_columns);
        if permutation.len() != self.columns.len() || self.column_order.len() != permutation.len() {
            // A stale permutation (e.g. the first page after the layout changed shape): start from
            // the natural order instead of composing with a mapping that no longer applies. The
            // widths are dropped too — they belong to the order being discarded — so the render
            // falls back to the default width per column.
            self.column_order = (0..self.all_columns.len()).collect();
            self.columns = self.all_columns.clone();
            self.column_widths.clear();
        }
        let composed = field_permutation(&self.column_order, &permutation, self.all_columns.len());
        permute_in_place(&mut self.columns, &composed);
        permute_in_place(&mut self.column_widths, &composed);
        self.column_order = permutation;
        self.visible_columns = visible;
        self.field_layout.order = self.field_layout.resolved_order(&self.all_columns);
        // A frozen field that the new layout hides (or the table no longer has) must not come back
        // frozen when it is shown again.
        self.frozen_columns.retain(|name| {
            self.all_columns.iter().any(|column| &column.name == name)
                && !self.field_layout.hidden.contains(name)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod notebook_tab {
        use super::super::*;
        use rustgrid_notebook::NotebookBinding;

        const FIXTURE: &str =
            include_str!("../../rustgrid-notebook/tests/fixtures/jupysql_basic.ipynb");

        fn loaded() -> rustgrid_notebook::Notebook {
            rustgrid_notebook::read_ipynb(FIXTURE).expect("fixture reads")
        }

        // A notebook as read back from a file bound to "c1": the writer owns the
        // `metadata.rustgrid` key, so normalise through it.
        fn bound_to_c1() -> rustgrid_notebook::Notebook {
            let mut notebook = loaded();
            notebook.binding = Some(NotebookBinding {
                connection_id: "c1".into(),
                database: Some("db".into()),
                schema: Some("s".into()),
            });
            rustgrid_notebook::read_ipynb(&rustgrid_notebook::write_ipynb(&notebook)).unwrap()
        }

        #[test]
        fn round_trip_is_lossless() {
            let original = bound_to_c1();
            assert_eq!(original.binding.as_ref().unwrap().connection_id, "c1");
            let tab = NotebookTab::from_notebook(1, original.clone(), None, Some(0));
            assert_eq!(tab.database.as_deref(), Some("db"));
            assert_eq!(tab.schema.as_deref(), Some("s"));
            assert!(!tab.dirty);
            assert_eq!(tab.to_notebook(Some("c1")), original);
            let written = rustgrid_notebook::write_ipynb(&tab.to_notebook(Some("c1")));
            assert_eq!(rustgrid_notebook::read_ipynb(&written).unwrap(), original);
        }

        #[test]
        fn unbound_notebook_has_no_binding() {
            let mut notebook = loaded();
            notebook.binding = None;
            let tab = NotebookTab::from_notebook(1, notebook, None, None);
            assert!(tab.to_notebook(None).binding.is_none());
        }

        #[test]
        fn unresolved_connection_keeps_loaded_binding() {
            let original = bound_to_c1();
            let tab = NotebookTab::from_notebook(1, original.clone(), None, None);
            assert_eq!(tab.to_notebook(None), original);
            let written = rustgrid_notebook::write_ipynb(&tab.to_notebook(None));
            let reread = rustgrid_notebook::read_ipynb(&written).unwrap();
            let binding = reread.binding.expect("binding survives");
            assert_eq!(binding.connection_id, "c1");
            assert_eq!(binding.database.as_deref(), Some("db"));
            assert_eq!(binding.schema.as_deref(), Some("s"));
        }

        #[test]
        fn explicit_unbind_drops_binding() {
            let mut tab = NotebookTab::from_notebook(1, bound_to_c1(), None, None);
            tab.set_binding(None, None, None);
            assert!(tab.to_notebook(None).binding.is_none());
        }

        #[test]
        fn execution_counter_starts_at_max_count() {
            let notebook = loaded();
            let max = notebook
                .cells
                .iter()
                .filter_map(|c| c.execution_count)
                .max()
                .unwrap_or(0);
            let tab = NotebookTab::from_notebook(1, notebook, None, None);
            assert_eq!(tab.execution_counter, max);
            assert!(max > 0);
        }

        #[test]
        fn helpers_mark_dirty() {
            let fresh = || NotebookTab::from_notebook(1, loaded(), None, None);
            let mut tab = fresh();
            tab.set_cell_source(0, "select 2".into());
            assert!(tab.dirty);
            assert_eq!(tab.cells[0].cell.source, "select 2");

            let mut tab = fresh();
            let n = tab.cells.len();
            tab.insert_cell(1, rustgrid_notebook::Cell::new_sql("x"));
            assert!(tab.dirty);
            assert_eq!(tab.cells.len(), n + 1);
            assert_eq!(tab.selected, 1);

            let mut tab = fresh();
            let _ = tab.remove_cell(0);
            assert!(tab.dirty);
            assert_eq!(tab.cells.len(), n - 1);

            let mut tab = fresh();
            tab.selected = 3;
            let _ = tab.remove_cell(0);
            assert_eq!(tab.selected, 2);

            let mut tab = fresh();
            let first = tab.cells[0].cell.id.clone();
            tab.move_cell(0, 1);
            assert!(tab.dirty);
            assert_eq!(tab.cells[1].cell.id, first);

            let mut tab = fresh();
            tab.set_binding(Some(3), Some("d".into()), None);
            assert!(tab.dirty);
            assert_eq!(tab.connection_index, Some(3));
            assert_eq!(tab.database.as_deref(), Some("d"));
        }

        #[test]
        fn the_jupysql_loader_cell_is_hidden_and_written_back_once() {
            let text = rustgrid_notebook::write_ipynb(&rustgrid_notebook::Notebook {
                cells: vec![rustgrid_notebook::Cell::new_sql("select 1")],
                ..Default::default()
            });
            let notebook = rustgrid_notebook::read_ipynb(&text).unwrap();
            assert_eq!(notebook.cells.len(), 2, "the writer added %load_ext sql");
            let loader_id = notebook.cells[0].id.clone();

            let tab = NotebookTab::from_notebook(1, notebook, None, None);
            assert_eq!(tab.cells.len(), 1);
            assert_eq!(tab.cells[0].cell.source, "select 1");

            let again = rustgrid_notebook::read_ipynb(&rustgrid_notebook::write_ipynb(
                &tab.to_notebook(None),
            ))
            .unwrap();
            assert_eq!(again.cells.len(), 2, "no second loader cell");
            assert_eq!(again.cells[0].id, loader_id, "the original loader is kept");
        }

        #[test]
        fn parameter_rows_edit_the_source_and_reach_the_cells_below() {
            let notebook = rustgrid_notebook::Notebook {
                cells: vec![
                    rustgrid_notebook::Cell::new_sql("select 0"),
                    rustgrid_notebook::Cell::new_parameters(),
                    rustgrid_notebook::Cell::new_sql("select '{{c}}'"),
                ],
                ..Default::default()
            };
            let mut tab = NotebookTab::from_notebook(1, notebook, None, None);
            let id = tab.cells[1].cell.id.clone();
            assert_eq!(tab.cells[1].param_rows.len(), 1, "a blank row to type into");

            // A name still being typed is kept in the table but not in the source.
            tab.set_param(&id, 0, Some("1"), Some("AR"));
            assert_eq!(tab.cells[1].cell.source, "");
            assert!(!tab.dirty);
            tab.set_param(&id, 0, Some("c"), None);
            assert_eq!(tab.cells[1].cell.source, "c = \"AR\"");
            assert!(tab.dirty);

            tab.add_param_row(1, 0);
            tab.set_param(&id, 1, Some("n"), Some("7"));
            assert_eq!(tab.cells[1].cell.source, "c = \"AR\"\nn = 7");

            assert!(tab.params_before(1).is_empty());
            let params = tab.params_before(2);
            assert_eq!(
                rustgrid_notebook::substitute("{{c}}-{{n}}", &params).unwrap(),
                "AR-7"
            );

            tab.remove_param_row(1, 0);
            assert_eq!(tab.cells[1].cell.source, "n = 7");
            tab.remove_param_row(1, 0);
            assert_eq!(tab.cells[1].param_rows.len(), 1);
            assert_eq!(tab.cells[1].cell.source, "");

            // A parameters cell cannot be retyped.
            assert!(
                tab.set_cell_kind(1, rustgrid_notebook::CellKind::Sql)
                    .is_empty()
            );
            assert_eq!(
                tab.cells[1].cell.kind,
                rustgrid_notebook::CellKind::Parameters
            );
        }

        fn sql_tab() -> NotebookTab {
            let notebook = rustgrid_notebook::Notebook {
                cells: vec![
                    rustgrid_notebook::Cell::new_sql("select 1"),
                    rustgrid_notebook::Cell::new_sql("select 2"),
                ],
                ..Default::default()
            };
            NotebookTab::from_notebook(1, notebook, None, None)
        }

        fn result_set() -> rustgrid_core::QueryResult {
            rustgrid_core::QueryResult {
                statement: "select 1".into(),
                columns: vec![rustgrid_core::ColumnInfo {
                    name: "a".into(),
                    data_type: String::new(),
                    nullable: true,
                    primary_key: false,
                    auto_increment: false,
                    generated: false,
                    comment: String::new(),
                }],
                rows: vec![vec![rustgrid_core::CellValue::Text("1".into())]],
                rows_affected: 0,
                has_result_set: true,
                last_insert_id: None,
            }
        }

        #[test]
        fn finished_run_fills_the_cell() {
            let mut tab = sql_tab();
            let id = tab.cells[1].cell.id.clone();
            let (generation, _) = tab.begin_run(&id).unwrap();
            assert_eq!(tab.running, Some(1));
            let elapsed = std::time::Duration::from_millis(5);
            assert!(tab.finish_run(&id, generation, elapsed, &[result_set()], None, 100));
            assert_eq!(tab.running, None);
            assert!(tab.dirty);
            let state = &tab.cells[1];
            assert_eq!(state.cell.execution_count, Some(1));
            assert_eq!(state.elapsed, Some(elapsed));
            assert!(state.error.is_none());
            assert!(matches!(
                state.cell.outputs.as_slice(),
                [rustgrid_notebook::CellOutput::Table { .. }]
            ));

            // The counter keeps counting across cells.
            let other = tab.cells[0].cell.id.clone();
            let (generation, _) = tab.begin_run(&other).unwrap();
            assert!(tab.finish_run(&other, generation, elapsed, &[], None, 100));
            assert_eq!(tab.cells[0].cell.execution_count, Some(2));
        }

        #[test]
        fn failed_run_keeps_earlier_results_and_appends_the_error() {
            let mut tab = sql_tab();
            let id = tab.cells[0].cell.id.clone();
            let (generation, _) = tab.begin_run(&id).unwrap();
            let ok = tab.finish_run(
                &id,
                generation,
                std::time::Duration::ZERO,
                &[result_set()],
                Some("boom".into()),
                100,
            );
            assert!(ok);
            let state = &tab.cells[0];
            assert_eq!(state.error.as_deref(), Some("boom"));
            assert!(matches!(
                state.cell.outputs.as_slice(),
                [
                    rustgrid_notebook::CellOutput::Table { .. },
                    rustgrid_notebook::CellOutput::Error { message }
                ] if message == "boom"
            ));
        }

        #[test]
        fn stale_run_is_discarded_after_stop() {
            let mut tab = sql_tab();
            let id = tab.cells[0].cell.id.clone();
            let (generation, cancel) = tab.begin_run(&id).unwrap();
            assert!(tab.stop_run());
            assert!(cancel.load(std::sync::atomic::Ordering::Relaxed));
            assert_eq!(tab.running, None);
            let installed = tab.finish_run(
                &id,
                generation,
                std::time::Duration::ZERO,
                &[result_set()],
                None,
                100,
            );
            assert!(!installed);
            assert!(tab.cells[0].cell.outputs.is_empty());
            assert_eq!(tab.cells[0].cell.execution_count, None);
            assert!(!tab.stop_run());
        }

        #[test]
        fn result_follows_a_moved_cell() {
            let mut tab = sql_tab();
            let id = tab.cells[0].cell.id.clone();
            let (generation, _) = tab.begin_run(&id).unwrap();
            tab.move_cell(0, 1);
            assert!(tab.finish_run(
                &id,
                generation,
                std::time::Duration::ZERO,
                &[result_set()],
                None,
                100
            ));
            assert_eq!(tab.cells[1].cell.execution_count, Some(1));
            assert_eq!(tab.cells[0].cell.execution_count, None);
        }

        #[test]
        fn result_grids_are_handed_back_when_replaced_or_removed() {
            let mut tab = sql_tab();
            let first = tab.cells[0].cell.id.clone();
            let second = tab.cells[1].cell.id.clone();
            assert!(
                tab.replace_result_grids(&first, vec![Some(1), None, Some(2)])
                    .is_empty()
            );
            assert!(tab.replace_result_grids(&second, vec![Some(3)]).is_empty());

            assert_eq!(tab.replace_result_grids(&first, vec![Some(4)]), vec![1, 2]);

            // A cell that is gone cannot own the new grids.
            assert_eq!(tab.replace_result_grids("missing", vec![Some(5)]), vec![5]);

            assert_eq!(tab.remove_cell(1), vec![3]);
            assert!(tab.remove_cell(9).is_empty());
            assert_eq!(tab.take_result_grids(), vec![4]);
        }

        #[test]
        fn taking_result_grids_keeps_outputs() {
            let mut tab = sql_tab();
            let id = tab.cells[0].cell.id.clone();
            let (generation, _) = tab.begin_run(&id).unwrap();
            assert!(tab.finish_run(
                &id,
                generation,
                std::time::Duration::ZERO,
                &[result_set()],
                None,
                100
            ));
            let _ = tab.replace_result_grids(&id, vec![Some(7)]);
            let second = tab.cells[1].cell.id.clone();
            let _ = tab.replace_result_grids(&second, vec![None, Some(8)]);
            assert_eq!(tab.take_result_grids(), vec![7, 8]);
            assert!(tab.take_result_grids().is_empty());
            assert!(tab.cells.iter().all(|state| state.result_grids.is_empty()));
            assert_eq!(tab.cells[0].cell.outputs.len(), 1);
        }

        #[test]
        fn deleting_a_connection_shifts_or_unbinds() {
            let mut tab = sql_tab();
            tab.connection_index = Some(2);
            assert!(!tab.connection_removed(3));
            assert_eq!(tab.connection_index, Some(2));
            assert!(!tab.connection_removed(0));
            assert_eq!(tab.connection_index, Some(1));
            assert!(tab.connection_removed(1));
            assert_eq!(tab.connection_index, None);
            assert!(!tab.connection_removed(0));
            assert!(!tab.dirty);
        }

        #[test]
        fn run_all_queues_sql_cells_and_stops_on_error_or_stop() {
            let mut tab = sql_tab();
            tab.insert_cell(1, rustgrid_notebook::Cell::new_markdown("# notes"));
            let first = tab.cells[0].cell.id.clone();
            let last = tab.cells[2].cell.id.clone();
            assert_eq!(tab.queue_run_all(0), Some(first));
            assert_eq!(tab.next_queued("", true), Some(last.clone()));
            assert_eq!(tab.next_queued("", true), None);

            // From a later cell only the cells below it run.
            assert_eq!(tab.queue_run_all(1), Some(last));
            assert_eq!(tab.next_queued("", true), None);

            // A failure drops the rest and selects the failed cell.
            let first = tab.queue_run_all(0).unwrap();
            tab.selected = 2;
            assert_eq!(tab.next_queued(&first, false), None);
            assert_eq!(tab.selected, 0);
            // A single run that fails does not move the selection.
            assert_eq!(tab.next_queued(&tab.cells[2].cell.id.clone(), false), None);
            assert_eq!(tab.selected, 0);

            // So does Stop.
            assert!(tab.queue_run_all(0).is_some());
            tab.stop_run();
            assert_eq!(tab.next_queued("", true), None);
        }

        #[test]
        fn editor_text_marks_dirty_only_on_change() {
            let mut tab = sql_tab();
            let id = tab.cells[0].cell.id.clone();
            tab.set_cell_source_by_id(&id, "select 1");
            assert!(!tab.dirty);
            tab.set_cell_source_by_id(&id, "select 10");
            assert!(tab.dirty);
            assert_eq!(tab.cells[0].cell.source, "select 10");
        }

        #[test]
        fn running_cell_follows_inserts_moves_and_removals() {
            let mut tab = sql_tab();
            tab.insert_cell(2, rustgrid_notebook::Cell::new_sql("select 3"));
            let id = tab.cells[1].cell.id.clone();
            tab.begin_run(&id).unwrap();
            assert_eq!(tab.running, Some(1));

            tab.insert_cell(0, rustgrid_notebook::Cell::new_sql("first"));
            assert_eq!(tab.running, Some(2));
            tab.move_cell(2, 0);
            assert_eq!(tab.running, Some(0));
            tab.move_cell(1, 3);
            assert_eq!(tab.running, Some(0));
            tab.move_cell(3, 0);
            assert_eq!(tab.running, Some(1));
            assert_eq!(tab.cells[1].cell.id, id);

            // The running cell cannot be removed; removing one above it shifts it.
            assert!(tab.remove_cell(1).is_empty());
            assert_eq!(tab.cells.len(), 4);
            let _ = tab.remove_cell(0);
            assert_eq!(tab.running, Some(0));
            assert_eq!(tab.cells[0].cell.id, id);
        }

        #[test]
        fn changing_a_cell_kind_drops_its_results() {
            use rustgrid_notebook::CellKind;
            let mut tab = sql_tab();
            let id = tab.cells[0].cell.id.clone();
            let (generation, _) = tab.begin_run(&id).unwrap();
            // The running cell keeps its kind.
            assert!(tab.set_cell_kind(0, CellKind::Markdown).is_empty());
            assert_eq!(tab.cells[0].cell.kind, CellKind::Sql);
            assert!(tab.finish_run(
                &id,
                generation,
                std::time::Duration::ZERO,
                &[result_set()],
                None,
                100
            ));
            let _ = tab.replace_result_grids(&id, vec![Some(9)]);
            tab.dirty = false;

            assert_eq!(tab.set_cell_kind(0, CellKind::Markdown), vec![9]);
            let state = &tab.cells[0];
            assert_eq!(state.cell.kind, CellKind::Markdown);
            assert!(state.cell.outputs.is_empty());
            assert_eq!(state.cell.execution_count, None);
            assert!(state.editing);
            assert!(tab.dirty);
            // Same kind or Raw: nothing happens.
            assert!(tab.set_cell_kind(0, CellKind::Markdown).is_empty());
            assert!(tab.set_cell_kind(0, CellKind::Raw).is_empty());
            assert!(tab.set_cell_kind(0, CellKind::Sql).is_empty());
            assert_eq!(tab.cells[0].cell.kind, CellKind::Sql);
        }

        #[test]
        fn leaving_a_markdown_cell_renders_it() {
            let mut tab = sql_tab();
            tab.insert_cell(1, rustgrid_notebook::Cell::new_markdown(""));
            assert!(tab.cells[1].editing);
            assert_eq!(tab.selected, 1);
            // Empty: stays in its editor.
            assert!(tab.select_cell(0));
            assert!(tab.cells[1].editing);
            assert!(tab.select_cell(1));
            let id = tab.cells[1].cell.id.clone();
            tab.set_cell_source_by_id(&id, "# Title");
            assert!(tab.select_cell(2));
            assert!(!tab.cells[1].editing);
            assert!(!tab.select_cell(2));
            assert!(!tab.select_cell(9));
        }

        #[test]
        fn folding_a_section_hides_its_cells_and_selecting_one_reveals_it() {
            let mut tab = sql_tab();
            tab.insert_cell(0, rustgrid_notebook::Cell::new_markdown("## Data"));
            tab.insert_cell(3, rustgrid_notebook::Cell::new_markdown("## Next"));
            // [## Data, sql, sql, ## Next]
            tab.selected = 2;
            assert!(tab.toggle_section(0));
            assert_eq!(tab.hidden_cells(), vec![false, true, true, false]);
            assert_eq!(tab.selected, 0, "the hidden selection moves to the heading");
            assert!(!tab.toggle_section(1), "an SQL cell heads no section");

            tab.select_cell(2);
            assert_eq!(tab.hidden_cells(), vec![false; 4]);
            assert!(!tab.cells[0].cell.is_collapsed());

            // Adding below a folded heading goes after its section and keeps it folded.
            assert!(tab.toggle_section(0));
            assert_eq!(tab.insert_point_after(0), 3);
            assert_eq!(tab.next_visible(0), Some(3));
            // A cell inserted inside a folded section unfolds it.
            tab.insert_cell(1, rustgrid_notebook::Cell::new_sql(""));
            assert!(!tab.cells[0].cell.is_collapsed());
        }

        #[test]
        fn saving_cleans_the_tab_and_keeps_its_binding() {
            let mut tab = sql_tab();
            tab.database = Some("shop".into());
            let id = tab.cells[0].cell.id.clone();
            tab.set_cell_source_by_id(&id, "SELECT 2");
            assert!(tab.dirty);
            tab.mark_saved("/tmp/a.ipynb".into(), Some("conn-1".into()));
            assert!(!tab.dirty);
            assert_eq!(
                tab.path.as_deref(),
                Some(std::path::Path::new("/tmp/a.ipynb"))
            );
            // A later save without a live connection keeps the id it was saved with.
            let binding = tab.to_notebook(None).binding.expect("binding");
            assert_eq!(binding.connection_id, "conn-1");
            assert_eq!(binding.database.as_deref(), Some("shop"));
        }

        #[test]
        fn failing_a_cell_records_the_error() {
            let mut tab = sql_tab();
            let id = tab.cells[0].cell.id.clone();
            tab.fail_cell(&id, "not connected".into());
            assert_eq!(tab.cells[0].error.as_deref(), Some("not connected"));
            assert_eq!(tab.cells[0].cell.outputs.len(), 1);
            assert!(tab.dirty);
        }

        #[test]
        fn removing_the_only_cell_resets_selection() {
            let mut tab =
                NotebookTab::from_notebook(1, rustgrid_notebook::Notebook::default(), None, None);
            assert!(tab.cells.is_empty());
            tab.insert_cell(0, rustgrid_notebook::Cell::new_sql("a"));
            let _ = tab.remove_cell(0);
            assert!(tab.cells.is_empty());
            assert_eq!(tab.selected, 0);
        }
    }

    fn columns(names: &[&str]) -> Vec<ColumnInfo> {
        names
            .iter()
            .map(|name| ColumnInfo {
                name: name.to_string(),
                data_type: "int".to_string(),
                nullable: true,
                primary_key: false,
                auto_increment: false,
                generated: false,
                comment: String::new(),
            })
            .collect()
    }

    fn hidden(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn batch_pages_map_to_their_connection_grids() {
        let mut tab = QueryTab::new(1);
        let mut target = BatchTarget::pending(Some(0), "server-a".into());
        target.results.push(BatchResult {
            statement: "select 1".into(),
            elapsed: std::time::Duration::ZERO,
            error: None,
            grid_id: Some(7),
            has_result_set: true,
            row_count: 1,
            rows_affected: 0,
        });
        tab.batch = Some(BatchQuery {
            group_id: 1,
            group_name: "group".into(),
            targets: vec![target],
        });

        assert_eq!(tab.batch.as_ref().unwrap().grid_for_page(0), None);
        assert_eq!(tab.batch.as_ref().unwrap().grid_for_page(1), Some(7));
        assert_eq!(tab.batch.as_ref().unwrap().grid_for_page(2), None);
        assert_eq!(tab.all_grid_ids(), vec![7]);
        assert!(tab.has_result_grid());
    }

    #[test]
    fn sorting_keeps_the_widths_of_the_same_column_set() {
        let previous = columns(&["BatchID", "LineID"]);
        let previous_widths = vec![130.0, 80.0];
        let rows = vec![vec![
            CellValue::Text("x".to_string()),
            CellValue::Text("0E".to_string()),
        ]];
        let widths = page_column_widths(&previous, &rows, &previous, &previous_widths, false);
        assert_eq!(widths, previous_widths);
    }

    #[test]
    fn permuting_places_each_element_at_its_sources_position() {
        let mut values = vec!['a', 'b', 'c', 'd'];
        permute_in_place(&mut values, &[3, 1, 0, 2]);
        assert_eq!(values, vec!['d', 'b', 'a', 'c']);
    }

    #[test]
    fn permuting_ignores_a_non_permutation() {
        let mut values = vec!['a', 'b', 'c'];
        permute_in_place(&mut values, &[1, 1, 0]);
        assert_eq!(values, vec!['a', 'b', 'c']);
    }

    #[test]
    fn a_layout_orders_the_visible_fields_first() {
        let layout = GridFieldLayout {
            order: vec!["c".to_string(), "a".to_string(), "b".to_string()],
            hidden: hidden(&["a"]),
        };
        let (permutation, visible) =
            layout.permutation_with_frozen(&columns(&["a", "b", "c"]), &[]);
        // `c` and `b` are visible and lead; the hidden `a` is kept after them.
        assert_eq!(permutation, vec![2, 1, 0]);
        assert_eq!(visible, 2);
    }

    #[test]
    fn a_layout_that_hides_every_field_falls_back_to_the_table_order() {
        let layout = GridFieldLayout {
            order: vec!["a".to_string(), "b".to_string()],
            hidden: hidden(&["a", "b"]),
        };
        let (permutation, visible) = layout.permutation_with_frozen(&columns(&["a", "b"]), &[]);
        assert_eq!(permutation, vec![0, 1]);
        assert_eq!(visible, 2);
    }

    #[test]
    fn frozen_fields_lead_the_display_order_in_the_order_they_were_frozen() {
        let layout = GridFieldLayout::default();
        let table = columns(&["a", "b", "c", "d"]);
        let frozen = vec!["c".to_string(), "a".to_string()];
        assert_eq!(
            layout.effective_order(&table, &frozen),
            vec!["c", "a", "b", "d"]
        );
        let (permutation, visible) = layout.permutation_with_frozen(&table, &frozen);
        assert_eq!(permutation, vec![2, 0, 1, 3]);
        assert_eq!(visible, 4);
    }

    #[test]
    fn dropping_the_frozen_set_restores_the_layout_order() {
        let layout = GridFieldLayout::default();
        let table = columns(&["a", "b", "c", "d"]);
        let frozen = vec!["d".to_string(), "b".to_string()];
        assert_eq!(
            layout.effective_order(&table, &frozen),
            vec!["d", "b", "a", "c"]
        );
        assert_eq!(
            layout.effective_order(&table, &[]),
            vec!["a", "b", "c", "d"]
        );
    }

    #[test]
    fn applying_a_frozen_display_order_keeps_the_unfrozen_positions() {
        let table = columns(&["a", "b", "c", "d"]);
        let layout = GridFieldLayout::default();
        let display = vec![
            "c".to_string(),
            "a".to_string(),
            "d".to_string(),
            "b".to_string(),
        ];
        let rebuilt =
            layout.with_display_order(&display, BTreeSet::new(), &["c".to_string()], &table);
        // `c` returns to its old index; the other fields keep the order the user dropped them in.
        assert_eq!(rebuilt.order, vec!["a", "d", "c", "b"]);
        assert_eq!(
            rebuilt.effective_order(&table, &["c".to_string()]),
            vec!["c", "a", "d", "b"]
        );
        assert_eq!(
            rebuilt.effective_order(&table, &[]),
            vec!["a", "d", "c", "b"]
        );
    }

    #[test]
    fn a_hidden_frozen_field_is_not_pinned() {
        let layout = GridFieldLayout {
            order: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            hidden: hidden(&["b"]),
        };
        let table = columns(&["a", "b", "c"]);
        let frozen = vec!["b".to_string(), "c".to_string()];
        // The hidden field stays at the tail; the visible frozen field still leads.
        assert_eq!(
            layout.permutation_with_frozen(&table, &frozen),
            (vec![2, 0, 1], 2)
        );
    }

    #[test]
    fn resolved_order_appends_fields_the_layout_does_not_know() {
        let layout = GridFieldLayout {
            order: vec!["b".to_string(), "gone".to_string()],
            hidden: BTreeSet::new(),
        };
        assert_eq!(
            layout.resolved_order(&columns(&["a", "b", "c"])),
            vec!["b", "a", "c"]
        );
    }

    #[test]
    fn only_the_tables_own_order_is_the_default() {
        let table = columns(&["a", "b"]);
        assert!(GridFieldLayout::default().is_default(&table));
        assert!(
            GridFieldLayout {
                order: vec!["a".to_string(), "b".to_string()],
                hidden: BTreeSet::new(),
            }
            .is_default(&table)
        );
        assert!(
            !GridFieldLayout {
                order: vec!["b".to_string(), "a".to_string()],
                hidden: BTreeSet::new(),
            }
            .is_default(&table)
        );
        assert!(
            !GridFieldLayout {
                order: vec!["a".to_string(), "b".to_string()],
                hidden: hidden(&["a"]),
            }
            .is_default(&table)
        );
    }

    #[test]
    fn reprojecting_composes_with_the_order_already_on_screen() {
        // The columns are currently displayed as `b, a, c`; the layout wants `c, b, a`.
        let composed = field_permutation(&[1, 0, 2], &[2, 1, 0], 3);
        let mut values = vec!["b", "a", "c"];
        permute_in_place(&mut values, &composed);
        assert_eq!(values, vec!["c", "b", "a"]);
    }

    #[test]
    fn moving_a_field_reorders_the_working_list() {
        let mut layout = GridFieldLayout::default();
        let order = ["a".to_string(), "b".to_string(), "c".to_string()];
        layout.move_field(&order, 0, 2);
        assert_eq!(layout.order, vec!["b", "c", "a"]);
        let moved = layout.order.clone();
        layout.move_field(&moved, 2, 0);
        assert_eq!(layout.order, vec!["a", "b", "c"]);
    }

    #[test]
    fn inverting_a_field_layout_never_hides_every_field() {
        let table = columns(&["a", "b", "c"]);
        let mut layout = GridFieldLayout::default();
        layout.order = layout.resolved_order(&table);
        // Everything was shown, so the inversion hides all but the first field.
        layout.invert_visibility();
        assert_eq!(layout.hidden, hidden(&["b", "c"]));
        // Inverting back shows exactly the field the first inversion kept.
        layout.invert_visibility();
        assert_eq!(layout.hidden, hidden(&["a"]));
        // A single-field table has nothing to invert.
        let single = columns(&["a"]);
        let mut only = GridFieldLayout::default();
        only.order = only.resolved_order(&single);
        only.invert_visibility();
        assert!(only.hidden.is_empty());
    }

    /// Each engine's paging statement is shown with its own quoting and paging syntax, so a
    /// PostgreSQL page no longer reads as MySQL's `LIMIT 0,1000`.
    #[test]
    fn the_status_statement_speaks_the_connections_dialect() {
        let mysql = page_display_sql(
            DriverDialect::Mysql,
            "test",
            "gen_table",
            &[],
            &[],
            &[],
            0,
            1000,
        );
        assert_eq!(mysql, "SELECT * FROM `test`.`gen_table` LIMIT 0,1000");

        let postgres = page_display_sql(
            DriverDialect::Postgres,
            "test",
            "public.gen_table",
            &[],
            &[],
            &[],
            0,
            1000,
        );
        assert_eq!(
            postgres,
            "SELECT * FROM \"public\".\"gen_table\" LIMIT 1000 OFFSET 0"
        );

        let sqlite = page_display_sql(
            DriverDialect::Sqlite,
            "C:\\data\\app.db",
            "users",
            &[],
            &[],
            &[],
            20,
            10,
        );
        assert_eq!(sqlite, "SELECT * FROM `users` LIMIT 10 OFFSET 20");

        let oracle = page_display_sql(
            DriverDialect::Oracle,
            "FREEPDB1",
            "SCOTT.EMP",
            &[],
            &[],
            &[],
            0,
            100,
        );
        assert_eq!(
            oracle,
            "SELECT * FROM \"SCOTT\".\"EMP\" ORDER BY 1 OFFSET 0 ROWS FETCH NEXT 100 ROWS ONLY"
        );
    }

    /// SQL Server's `OFFSET … FETCH NEXT` needs an `ORDER BY`; the display orders by the primary
    /// key when the table has one (as the driver does) and falls back to a constant order.
    #[test]
    fn a_sql_server_page_orders_before_fetching() {
        let mut keyed = columns(&["id", "name"]);
        keyed[0].primary_key = true;
        assert_eq!(
            page_display_sql(
                DriverDialect::SqlServer,
                "app",
                "dbo.users",
                &[],
                &[],
                &keyed,
                40,
                20,
            ),
            "SELECT * FROM [app].[dbo].[users] ORDER BY [id] OFFSET 40 ROWS FETCH NEXT 20 ROWS ONLY"
        );
        assert_eq!(
            page_display_sql(
                DriverDialect::SqlServer,
                "app",
                "dbo.logs",
                &[],
                &[],
                &columns(&["message"]),
                0,
                20,
            ),
            "SELECT * FROM [app].[dbo].[logs] ORDER BY (SELECT NULL) \
             OFFSET 0 ROWS FETCH NEXT 20 ROWS ONLY"
        );
    }

    /// The shown `WHERE` quotes its columns the engine's way and keeps the applied sort order.
    #[test]
    fn the_status_statement_quotes_filters_and_sorts_per_dialect() {
        let filter = vec![FilterNode::Condition(FilterCondition {
            column: "name".to_string(),
            operator: FilterOperator::Equal,
            value: "ann".to_string(),
            ..FilterCondition::default()
        })];
        let sorts = vec![SortColumn {
            column: "id".to_string(),
            descending: true,
        }];
        assert_eq!(
            page_display_sql(
                DriverDialect::SqlServer,
                "app",
                "dbo.users",
                &filter,
                &sorts,
                &columns(&["id", "name"]),
                0,
                20,
            ),
            "SELECT * FROM [app].[dbo].[users] WHERE [name] = 'ann' ORDER BY [id] DESC \
             OFFSET 0 ROWS FETCH NEXT 20 ROWS ONLY"
        );
    }

    #[test]
    fn subtracting_a_cell_splits_a_select_all_range_in_four() {
        let mut selection = CellSelection::single((0, 0), (9, 2));
        assert!(selection.subtract(CellRange {
            anchor: (5, 1),
            cursor: (5, 1),
        }));
        assert_eq!(selection.ranges.len(), 4);
        assert!(!selection.contains(5, 1));
        assert_eq!(selection.cells().len(), 30 - 1);
        assert_eq!(
            selection.ranges,
            vec![
                CellRange {
                    anchor: (0, 0),
                    cursor: (4, 2),
                },
                CellRange {
                    anchor: (6, 0),
                    cursor: (9, 2),
                },
                CellRange {
                    anchor: (5, 0),
                    cursor: (5, 0),
                },
                CellRange {
                    anchor: (5, 2),
                    cursor: (5, 2),
                },
            ]
        );
    }

    #[test]
    fn only_a_one_cell_selection_counts_as_single_cell() {
        assert!(CellSelection::new(3, 4).is_single_cell());
        assert!(CellSelection::single((3, 4), (3, 4)).is_single_cell());
        assert!(!CellSelection::single((3, 4), (3, 5)).is_single_cell());
        assert!(!CellSelection::single((3, 4), (4, 4)).is_single_cell());

        // Several single-cell ranges (Ctrl+click) are not a cursor: the wheel scrolls instead.
        let mut selection = CellSelection::new(0, 0);
        selection.ranges.push(CellRange::new(5, 5));
        assert!(!selection.is_single_cell());
    }

    #[test]
    fn subtracting_a_cell_drops_a_single_cell_range() {
        let mut selection = CellSelection::single((2, 2), (2, 2));
        assert!(selection.subtract(CellRange {
            anchor: (2, 2),
            cursor: (2, 2),
        }));
        assert!(selection.ranges.is_empty());
        assert!(!selection.contains(2, 2));
    }

    #[test]
    fn subtracting_a_row_span_removes_the_row_from_select_all() {
        let mut selection = CellSelection::single((0, 0), (3, 2));
        assert!(selection.subtract(CellRange {
            anchor: (1, 0),
            cursor: (1, 2),
        }));
        assert!(!selection.contains(1, 0));
        assert!(!selection.contains(1, 2));
        assert_eq!(selection.cells().len(), 12 - 3);
    }

    #[test]
    fn subtracting_something_unselected_changes_nothing() {
        let mut selection = CellSelection::single((0, 0), (1, 1));
        assert!(!selection.subtract(CellRange {
            anchor: (5, 5),
            cursor: (5, 5),
        }));
        assert_eq!(
            selection.ranges,
            vec![CellRange {
                anchor: (0, 0),
                cursor: (1, 1),
            }]
        );
    }
}

//! Bringing external `.sql` files into the Queries list: the toolbar's Import, the list's context
//! menu, and a `.sql` dropped on the window or passed on the command line.
//!
//! An imported file is **copied** into the current scope's query folder (the original is never
//! touched) under its own file name, with ` (2)`, ` (3)`, ... when that name is taken, so nothing
//! is overwritten. Reading and writing run on Tokio's blocking pool.

use std::path::{Path, PathBuf};

use super::*;

/// A script larger than this is refused: it belongs in Run SQL File, not in the query editor.
const MAX_IMPORT_BYTES: u64 = 10 * 1024 * 1024;

/// Where imported queries are filed: a group's folder, or a connection's database (and schema).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct QueryTarget {
    pub group_id: Option<u64>,
    pub connection_id: String,
    pub database: String,
    pub schema: Option<String>,
}

impl QueryTarget {
    fn saved_query(&self, name: String, sql: String) -> SavedQuery {
        SavedQuery {
            name,
            sql,
            connection_id: self.connection_id.clone(),
            group_id: self.group_id,
            database: self.database.clone(),
            schema: self.schema.clone(),
        }
    }

    fn path(&self, config: &ConfigStore, name: &str) -> PathBuf {
        match self.group_id {
            Some(group_id) => config.group_query_file_path(group_id, name),
            None => config.query_file_path(
                &self.connection_id,
                &self.database,
                self.schema.as_deref(),
                name,
            ),
        }
    }
}

/// Why one file was not imported.
#[derive(Debug, PartialEq, Eq)]
enum ImportError {
    TooLarge,
    Io(String),
}

/// One file's outcome: the saved file, or the source and why it failed.
type ImportOutcome = Result<PathBuf, (PathBuf, ImportError)>;

/// `stem`, or the first of `stem (2)`, `stem (3)`, ... that `taken` does not reject.
fn unique_name(stem: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(stem) {
        return stem.to_string();
    }
    (2u64..)
        .map(|number| format!("{stem} ({number})"))
        .find(|candidate| !taken(candidate))
        .expect("an unused name always exists")
}

/// The query name a file is imported under: its file name without the extension.
fn query_name(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().trim().to_string())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "query".to_string())
}

/// Read a script as text: UTF-8, falling back to GB18030 like the Import Wizard.
fn read_script(path: &Path) -> Result<String, ImportError> {
    let size = std::fs::metadata(path)
        .map_err(|error| ImportError::Io(error.to_string()))?
        .len();
    if size > MAX_IMPORT_BYTES {
        return Err(ImportError::TooLarge);
    }
    let bytes = std::fs::read(path).map_err(|error| ImportError::Io(error.to_string()))?;
    Ok(rustgrid_import::decode_text(&bytes))
}

/// Copy every file in `paths` into `target`, one by one, so two files with the same name in one
/// batch still get distinct names.
fn import_files(
    config: &ConfigStore,
    target: &QueryTarget,
    paths: Vec<PathBuf>,
) -> Vec<ImportOutcome> {
    paths
        .into_iter()
        .map(|path| {
            let sql = read_script(&path).map_err(|error| (path.clone(), error))?;
            let name = unique_name(&query_name(&path), |name| {
                target.path(config, name).exists()
            });
            config
                .save_query_file(&target.saved_query(name, sql))
                .map_err(|error| (path, ImportError::Io(error.to_string())))
        })
        .collect()
}

impl AppView {
    /// The scope the Queries list files new queries in: the selected group, else the open
    /// database (and schema) of the tree's selection, else the group of the selected connection.
    pub(super) fn query_import_target(&self, cx: &App) -> Option<QueryTarget> {
        let group = |group_id| QueryTarget {
            group_id: Some(group_id),
            connection_id: String::new(),
            database: String::new(),
            schema: None,
        };
        if let Some(group_id) = self.single_selected_group(cx) {
            return Some(group(group_id));
        }
        if let Some((connection_id, database, schema)) = self.query_scope(cx) {
            return Some(QueryTarget {
                group_id: None,
                connection_id,
                database,
                schema,
            });
        }
        self.selected_connection_group(cx).map(group)
    }

    /// Import…: pick `.sql` files and copy them into the current scope.
    pub(super) fn import_query_files_dialog(&mut self, cx: &mut Context<'_, Self>) {
        let Some(target) = self.query_import_target(cx) else {
            return;
        };
        let title = t!("query.import_title").to_string();
        cx.spawn(async move |this, cx| {
            let dialog = super::file_dialog::open_dialog(title, "SQL", &["sql"]);
            let Some(handles) = dialog.pick_files().await else {
                return;
            };
            let paths = handles
                .iter()
                .map(|handle| handle.path().to_path_buf())
                .collect();
            let _ = this.update(cx, |app, cx| app.import_query_paths(paths, target, cx));
        })
        .detach();
    }

    /// Copy `paths` into `target`, then refresh the list, select what was imported and say how
    /// it went.
    pub(super) fn import_query_paths(
        &mut self,
        paths: Vec<PathBuf>,
        target: QueryTarget,
        cx: &mut Context<'_, Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        let config = self.config.clone();
        let runtime = self.runtime.clone();
        cx.spawn(async move |this, cx| {
            let outcomes = runtime
                .spawn_blocking(move || import_files(&config, &target, paths))
                .await;
            let _ = this.update(cx, |app, cx| {
                let outcomes = match outcomes {
                    Ok(outcomes) => outcomes,
                    Err(error) => {
                        app.error_dialog = Some(error.to_string());
                        cx.notify();
                        return;
                    }
                };
                app.finish_query_import(outcomes, cx);
            });
        })
        .detach();
    }

    fn finish_query_import(&mut self, outcomes: Vec<ImportOutcome>, cx: &mut Context<'_, Self>) {
        let (imported, failed): (Vec<_>, Vec<_>) = outcomes.into_iter().partition(Result::is_ok);
        let imported: Vec<PathBuf> = imported.into_iter().filter_map(Result::ok).collect();
        self.refresh_query_files(cx);
        let mut first = true;
        for path in &imported {
            if let Some(index) = self.query_files.iter().position(|file| &file.path == path) {
                if first {
                    self.select_query_one(index);
                    first = false;
                } else {
                    self.query_selection
                        .add(super::query::query_file_key(&self.query_files[index]));
                }
            }
        }
        if !imported.is_empty() {
            self.toast(
                ToastKind::Success,
                t!("query.imported", count = imported.len()).to_string(),
                cx,
            );
        }
        if !failed.is_empty() {
            let reasons: Vec<String> = failed
                .into_iter()
                .filter_map(Result::err)
                .map(|(path, error)| {
                    let name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    match error {
                        ImportError::TooLarge => t!("query.import_too_large", name = name),
                        ImportError::Io(error) => {
                            t!("query.import_failed_file", name = name, error = error)
                        }
                    }
                    .to_string()
                })
                .collect();
            self.error_dialog = Some(reasons.join("\n"));
        }
        cx.notify();
    }

    /// A `.sql` with no Queries scope to file it in (dropped, or passed on the command line, while
    /// no database or group is selected): open each in a new, unsaved query tab so its text is
    /// not lost.
    pub(super) fn open_sql_files_unsaved(
        &mut self,
        paths: Vec<PathBuf>,
        cx: &mut Context<'_, Self>,
    ) {
        let runtime = self.runtime.clone();
        cx.spawn(async move |this, cx| {
            let read = runtime
                .spawn_blocking(move || {
                    paths
                        .into_iter()
                        .map(|path| {
                            let text = read_script(&path);
                            (path, text)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |app, cx| {
                let Ok(read) = read else {
                    return;
                };
                let mut errors = Vec::new();
                for (path, text) in read {
                    let name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    match text {
                        Ok(sql) => app.open_unsaved_query_text(sql, cx),
                        Err(ImportError::TooLarge) => {
                            errors.push(t!("query.import_too_large", name = name).to_string())
                        }
                        Err(ImportError::Io(error)) => errors.push(
                            t!("query.import_failed_file", name = name, error = error).to_string(),
                        ),
                    }
                }
                if !errors.is_empty() {
                    app.error_dialog = Some(errors.join("\n"));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A new query tab (on the default connection, like New Query) holding `sql`, unsaved.
    fn open_unsaved_query_text(&mut self, sql: String, cx: &mut Context<'_, Self>) {
        let connection_index = self.default_query_connection(cx);
        let database = connection_index.and_then(|index| self.default_query_database(index, cx));
        let schema = connection_index
            .and_then(|index| self.default_query_schema(cx, index, database.as_deref()));
        self.open_query_with(connection_index, database, schema, cx);
        if let Some(active) = self.active_query
            && let Some(tab) = self.queries.get_mut(active)
        {
            tab.sql = sql;
            tab.caret = 0;
            tab.anchor = 0;
            self.sync_query_editor_text(active, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_name_appends_the_first_free_number() {
        let taken = ["a", "a (2)", "b (2)"];
        let is_taken = |name: &str| taken.contains(&name);
        assert_eq!(unique_name("c", is_taken), "c");
        assert_eq!(unique_name("a", is_taken), "a (3)");
        assert_eq!(unique_name("b", is_taken), "b");
    }

    #[test]
    fn query_name_is_the_file_stem() {
        assert_eq!(query_name(Path::new("/x/report.sql")), "report");
        assert_eq!(query_name(Path::new("/x/my.report.SQL")), "my.report");
        assert_eq!(query_name(Path::new("/x/.sql")), ".sql");
    }

    #[test]
    fn import_copies_files_and_never_overwrites() {
        let root = std::env::temp_dir().join(format!(
            "rustgrid-query-import-{}",
            crate::form::now_nanos()
        ));
        let source = root.join("source");
        let other = source.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(source.join("a.sql"), "select 1").unwrap();
        std::fs::write(other.join("a.sql"), "select 2").unwrap();
        // Not UTF-8: GB18030 for "中".
        std::fs::write(source.join("b.sql"), [0xD6, 0xD0]).unwrap();
        let big = source.join("big.sql");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_IMPORT_BYTES + 1)
            .unwrap();

        let config = ConfigStore::with_root(root.join("config"));
        let target = QueryTarget {
            group_id: None,
            connection_id: "c1".into(),
            database: "db".into(),
            schema: None,
        };
        // An existing query with the same name is kept.
        config
            .save_query_file(&target.saved_query("a".into(), "keep".into()))
            .unwrap();

        let outcomes = import_files(
            &config,
            &target,
            vec![
                source.join("a.sql"),
                other.join("a.sql"),
                source.join("b.sql"),
                big.clone(),
            ],
        );
        let read = |name: &str| std::fs::read_to_string(target.path(&config, name)).unwrap();
        assert_eq!(read("a"), "keep");
        assert_eq!(read("a (2)"), "select 1");
        assert_eq!(read("a (3)"), "select 2");
        assert_eq!(read("b"), "中");
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 3);
        assert_eq!(outcomes[3], Err((big, ImportError::TooLarge)));
        assert_eq!(
            std::fs::read_to_string(source.join("a.sql")).unwrap(),
            "select 1"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}

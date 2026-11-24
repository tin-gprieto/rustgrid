# SQL notebooks

A notebook mixes **SQL cells** and **Markdown cells** in one tab, runs the SQL against a connection
and keeps each result under its cell. It is saved as a Jupyter `.ipynb` in the format
[JupySQL](https://jupysql.ploomber.io/) uses, so the same file opens in Jupyter Notebook,
JupyterLab or VS Code and its `%%sql` cells run there too.

## Creating and opening

- **New Notebook** — the toolbar button next to New Query (bound to the connection and database a
  new query would use), or the right-click menu of a connection or a database.
- **Open Notebook…** — the right-click menu of the connection tree's blank area, a connection or a
  database. The picker starts in RustGrid's notebook folder.
- **Drag a `.ipynb` onto the window**, or launch RustGrid with one as its argument ("Open with",
  dragging it onto the executable). A running instance receives the file.
- **The Queries list** — notebooks saved in RustGrid's notebook folder are listed beside the saved
  queries of their connection (for every database of it); double-click opens one.

The notebook toolbar carries the same **connection / database / schema** pickers as a query tab.
Changing the connection or database starts a fresh session on the next run. A file saved by RustGrid
remembers its connection by profile id: opening it binds and connects that connection again. A file
from elsewhere (or whose connection was deleted) opens unbound and asks you to choose one.

Notebooks need a SQL engine: Redis and MongoDB connections do not offer them.

## Cells

| Cell | What it does |
| --- | --- |
| **SQL** | Runs its statements in order on the notebook's session and shows every result set as a grid (up to 10 rows high, scrolling past that); a statement without rows shows its affected-row count. The first failing statement stops the cell and is shown in red. |
| **Markdown** | Rendered text. Double-click to edit; ✓ in its header, `Ctrl+Enter` or selecting another cell renders it again. |
| **Parameters** | A name / value table (`+` adds a row under one, `−` removes it). `{{name}}` in the SQL cells **below** it is replaced by the value before the cell runs; when a name is defined in several parameters cells, the last one above wins. Nothing runs in the cell itself. |
| **Raw** | Any other code cell of an opened file (Python, `%sql` line magics, …) or an nbformat `raw` cell. Shown read-only and kept as it is; RustGrid never runs it. |

Each cell header offers change type (SQL ↔ Markdown; not for Parameters and Raw cells), move up / down, delete, and — when the cell has
results — **Export results**, which opens the Export Wizard (several result sets can go into one
`.xlsx` workbook). SQL cells complete tables, columns and keywords like the query editor.

**Run All** runs every SQL cell from the top and stops at the first cell that fails, which it
selects (unfolding its section); **Stop** cancels the running cell; **Restart** drops the session.

### Parameters

```
country = "AR"          ← the Parameters cell, as saved
year = 2025

SELECT * FROM sales WHERE country = '{{country}}' AND year = {{ year }}
```

The value is pasted into the SQL **as text**, exactly as JupySQL does — it is not a bound
parameter, so it gives no protection against SQL injection, and text needs its own quotes
(`'{{country}}'`). The whole cell text is expanded, string literals and comments included. Only a
plain name is expanded (spaces inside the braces are allowed); anything else between `{{ }}` is left
as written. A `{{name}}` that no parameters cell above defines fails the cell before anything runs
(and stops Run All). A value that reads as a number, `True`, `False` or `None` is saved as that
Python literal; anything else is saved as a string. A row whose name is not a valid identifier yet
is kept in the table but left out of the file.

### Collapsible sections

As in Google Colab and JupyterLab, a Markdown cell whose first heading is `#`…`######` heads a
section: it runs until the next heading of the same or a higher level (fewer `#`). The chevron in the
heading's header folds it, hiding every cell of the section — subsections, text and SQL — behind an
"N cells hidden" note. Adding a cell below a folded heading puts it after the section; going to a
hidden cell unfolds what hides it. The folded state is saved in the file under JupyterLab's
`jp-MarkdownHeadingCollapsed` cell metadata, so JupyterLab honours it and the other way round.

## Keyboard

| Key | SQL cell | Markdown cell being edited |
| --- | --- | --- |
| `Ctrl+Enter` (`⌘Enter`) | run | render |
| `Shift+Enter` | run, go to the next visible cell (adding an SQL cell at the end) | render, go on |
| `Alt+Enter` | run, insert an SQL cell below | render, insert below |
| `Esc` | (closes the completion popup) | render |
| `Ctrl+S` / `Ctrl+Shift+S` | Save / Save As | Save / Save As |
| `Tab` / `Ctrl+Space` | accept / request a completion | — |

All of them are rebindable under **Options → Shortcuts → Notebook**. They act while the caret is in a
cell's editor (Save from anywhere in the notebook).

## Sessions

A notebook keeps **one pinned database session** while it is open, so `SET`, `USE`, temporary
tables and an open transaction carry from one cell to the next — the same session model as the query
editor. This holds on **MySQL / MariaDB, PostgreSQL, SQLite and ClickHouse**. **SQL Server, Oracle and
ODBC** have no pinned script session yet: each statement takes a pooled connection, so session state
does not survive between statements. Disconnecting a connection releases the sessions of its
notebooks; the notebooks stay open and run again once it is back.

## Saving and the file format

**Save** (`Ctrl+S`) writes the file; a new notebook (and **Save As**) asks where, starting in
`<config dir>/notebooks/<connection id>/`. Notebooks saved there are the ones the Queries list shows;
a notebook can be saved anywhere else too. Closing a notebook with unsaved changes asks first, and
the workspace session reopens open notebooks at the next start (an unsaved one comes back with its
unsaved cells).

The file is nbformat 4.5 for a Python kernel:

- an SQL cell is a code cell starting with `%%sql` (anything after `%%sql` on that line is kept but
  not interpreted by RustGrid);
- a Parameters cell is a code cell tagged `parameters` (papermill's convention) with one Python
  assignment per line (`country = "AR"`), so in Jupyter it defines the variables JupySQL expands.
  A tagged cell with any other line (a comment, a call, …) opens as a Raw cell;
- a `%load_ext sql` cell is added at the top when the file has none, so JupySQL loads. RustGrid
  does not show a cell that only loads the extension, and writes it back unchanged on save;
- results are `execute_result` outputs with `text/html` and `text/plain` tables, errors are `error`
  outputs — up to **Rows kept per result** rows each (Options → General → Notebooks, default 100;
  0 for no limit). With **Save results in the notebook file** off the file carries no outputs or
  execution counts, like Jupyter's *Clear All Outputs*;
- `metadata.rustgrid` holds the connection's **profile id**, database and schema — never a host,
  user or password;
- everything RustGrid does not model (other metadata, unknown outputs, extra keys) is written back
  unchanged.

To run a RustGrid notebook in Jupyter, install JupySQL and run `%sql <connection url>` once before
the `%%sql` cells: the file names the connection only by RustGrid's id, so Jupyter needs its own URL.

## Limitations

- No Python: only SQL and Markdown cells run. Other code cells are kept, not executed.
- `%%sql` arguments (`--save`, `--with`, `<<` variables, a connection name, …) are not
  interpreted: those cells run as plain SQL. `{{ }}` only takes a parameter name (no Jinja
  expressions or filters).
- No pinned session on SQL Server, Oracle and ODBC (see *Sessions*).
- Result grids in a notebook are read-only.

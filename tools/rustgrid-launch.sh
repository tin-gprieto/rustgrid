#!/usr/bin/env sh
# Compila RustGrid (cargo build, incremental) y lo abre. Lo usa el acceso directo creado con
# `tools/install-desktop-entry.sh --build`, así el acceso directo siempre abre la rama actual.
#
# Si la compilación falla se avisa con una notificación y no se abre nada; la salida completa
# queda en ~/.cache/rustgrid/launch-build.log.
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ICON="$ROOT/crates/rustgrid-app/assets/logo.png"
BIN="$ROOT/target/debug/RustGrid"
LOG_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/rustgrid"
LOG="$LOG_DIR/launch-build.log"

# Los lanzadores gráficos no siempre cargan el PATH de la shell.
PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
export PATH

notify() {
  command -v notify-send >/dev/null 2>&1 &&
    notify-send -a RustGrid -i "$ICON" "$@" 2>/dev/null || true
}

# Instancia GUI ya abierta (se ignoran los procesos `--mcp-stdio`): el nuevo binario solo le
# pasaría el control, así que la versión recién compilada no se vería hasta cerrarla.
gui_running() {
  pgrep -x RustGrid -a 2>/dev/null | grep -v -- '--mcp-stdio' >/dev/null
}

if ! command -v cargo >/dev/null 2>&1; then
  notify "RustGrid" "No se encontró cargo; no se puede compilar."
  exit 1
fi

mkdir -p "$LOG_DIR"
notify "RustGrid" "Compilando la rama actual…"

if ! (cd "$ROOT" && nice -n 10 cargo build -p rustgrid-app) >"$LOG" 2>&1; then
  notify -u critical "RustGrid: la compilación falló" "Detalles en $LOG"
  exit 1
fi

if gui_running; then
  notify "RustGrid ya está abierto" \
    "Compilado, pero la ventana abierta sigue con la versión anterior. Ciérrala y vuelve a abrir el acceso directo."
fi

exec "$BIN" "$@"

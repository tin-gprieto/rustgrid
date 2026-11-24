#!/usr/bin/env sh
# Crea un acceso directo de RustGrid (menú de aplicaciones + escritorio) en Linux.
#
# Uso:
#   ./tools/install-desktop-entry.sh            # usa target/release/RustGrid o, si no existe, target/debug/RustGrid
#   ./tools/install-desktop-entry.sh /ruta/RustGrid
#   ./tools/install-desktop-entry.sh --build    # compila la rama actual (cargo build) antes de abrir
#   ./tools/install-desktop-entry.sh --remove   # elimina los accesos directos
#
# También se puede ejecutar con doble clic desde el explorador de archivos
# (marcarlo como ejecutable: chmod +x tools/install-desktop-entry.sh).
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ICON="$ROOT/crates/rustgrid-app/assets/logo.png"
APPS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ENTRY_NAME="rustgrid.desktop"

# Carpeta Escritorio según el idioma del sistema (Desktop, Escritorio, ...).
if command -v xdg-user-dir >/dev/null 2>&1; then
  DESKTOP_DIR="$(xdg-user-dir DESKTOP)"
else
  DESKTOP_DIR="$HOME/Desktop"
fi

if [ "${1:-}" = "--remove" ]; then
  rm -f "$APPS_DIR/$ENTRY_NAME" "$DESKTOP_DIR/$ENTRY_NAME"
  command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS_DIR" || true
  echo "Accesos directos eliminados."
  exit 0
fi

if [ "${1:-}" = "--build" ]; then
  # El acceso directo pasa por tools/rustgrid-launch.sh, que compila y luego abre el binario.
  BIN="$ROOT/tools/rustgrid-launch.sh"
  chmod +x "$BIN"
elif [ -n "${1:-}" ]; then
  BIN="$1"
elif [ -x "$ROOT/target/release/RustGrid" ]; then
  BIN="$ROOT/target/release/RustGrid"
else
  BIN="$ROOT/target/debug/RustGrid"
fi

if [ ! -x "$BIN" ]; then
  echo "No se encontró el ejecutable: $BIN" >&2
  echo "Compílalo primero (cargo build -j 2) o pasa su ruta como argumento." >&2
  exit 1
fi

BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"

mkdir -p "$APPS_DIR"
cat > "$APPS_DIR/$ENTRY_NAME" <<EOF
[Desktop Entry]
Type=Application
Version=1.0
Name=RustGrid
GenericName=Database Manager
Comment=Gestor de bases de datos
Exec="$BIN" %F
Icon=$ICON
Terminal=false
Categories=Development;Database;
StartupWMClass=RustGrid
EOF
chmod +x "$APPS_DIR/$ENTRY_NAME"

# Copia al escritorio y la marca como confiable (GNOME exige "Allow Launching").
if [ -d "$DESKTOP_DIR" ]; then
  cp "$APPS_DIR/$ENTRY_NAME" "$DESKTOP_DIR/$ENTRY_NAME"
  chmod +x "$DESKTOP_DIR/$ENTRY_NAME"
  command -v gio >/dev/null 2>&1 &&
    gio set "$DESKTOP_DIR/$ENTRY_NAME" metadata::trusted true 2>/dev/null || true
fi

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS_DIR" || true

echo "Acceso directo creado:"
echo "  Menú de aplicaciones: $APPS_DIR/$ENTRY_NAME"
[ -d "$DESKTOP_DIR" ] && echo "  Escritorio:           $DESKTOP_DIR/$ENTRY_NAME"
echo "Ejecutable: $BIN"

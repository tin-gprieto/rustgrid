# Compilar RustGrid en Linux

Requisitos del sistema y pasos para compilar `RustGrid` en Linux. Los paquetes de
sistema son necesarios porque GPUI (la UI) enlaza contra bibliotecas nativas, y
SQLCipher (SQLite cifrado) se compila desde C.

## 1. Herramientas de compilación

| Requisito | Para qué | Notas |
|---|---|---|
| Rust estable ≥ 1.94 | Compilador | `rust-toolchain.toml` fija `stable`; instalar con [rustup](https://rustup.rs) |
| `gcc` / `cc` y `make` | Compilar C (SQLCipher, `ring`, `zstd`, tree-sitter) | Paquete `build-essential` |
| `pkg-config` | Localizar bibliotecas nativas | Sin él, `fontconfig-sys` falla |
| `git` | Clonar el repositorio | |

## 2. Bibliotecas de desarrollo

Se necesitan los paquetes `-dev` (traen el `.pc` y el symlink `.so` que usa el linker).

### Debian / Ubuntu

```sh
sudo apt update
sudo apt install -y build-essential pkg-config git \
  libfontconfig1-dev \
  libxkbcommon-dev libxkbcommon-x11-dev \
  libxcb1-dev libxcb-xkb-dev libx11-xcb-dev \
  libwayland-dev
```

### Fedora

```sh
sudo dnf install -y gcc gcc-c++ make pkgconf-pkg-config git \
  fontconfig-devel \
  libxkbcommon-devel libxkbcommon-x11-devel \
  libxcb-devel libwayland-client-devel wayland-devel
```

### Arch Linux

```sh
sudo pacman -S --needed base-devel git pkgconf \
  fontconfig libxkbcommon libxkbcommon-x11 libxcb wayland
```

## 3. Rust

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustc --version   # debe ser >= 1.94
```

## 4. Compilar y ejecutar

```sh
cargo build -j 2          # ver "Memoria" abajo
cargo run -p rustgrid-app # el binario resultante es RustGrid
```

## 5. Memoria (importante)

`gpui-fast` es un crate muy grande. Con el paralelismo por defecto de Cargo
(un job por núcleo) se pueden agotar ~8 GB de RAM y el kernel mata `rustc`:

```
error: could not compile `gpui-fast` (lib) ... (signal: 9, SIGKILL: kill)
```

Soluciones:

- Limitar el paralelismo: `cargo build -j 2` (o `-j 1`).
- Hacerlo permanente con un `.cargo/config.toml` local (está en `.gitignore`):

  ```toml
  [build]
  jobs = 2
  ```
- Cerrar aplicaciones pesadas (navegador, IDE) durante la compilación.
- Agregar swap si hace falta, por ejemplo 8 GB:

  ```sh
  sudo fallocate -l 8G /swapfile2 && sudo chmod 600 /swapfile2
  sudo mkswap /swapfile2 && sudo swapon /swapfile2
  ```

Reiniciar la compilación es seguro: Cargo conserva lo ya compilado.

## 6. Requisitos en tiempo de ejecución (opcionales)

- **Diálogos de archivos/carpetas** (exportar, importar, `.rgc`): `rfd` usa el
  portal XDG; conviene tener `xdg-desktop-portal` y el backend de tu escritorio
  (`xdg-desktop-portal-gnome`, `-kde`, `-gtk`, ...).
- **Gráficos**: un driver Vulkan o GL funcional (Mesa en la mayoría de equipos).
- **Conexiones ODBC**: solo si se usa el driver genérico; requiere `unixODBC`
  (`libodbc.so`) y el driver del motor. Se carga en runtime; no es necesario para compilar.

## 7. Errores frecuentes

| Error | Causa | Solución |
|---|---|---|
| `Package fontconfig was not found in the pkg-config search path` | Falta `libfontconfig1-dev` / `pkg-config` | Instalar los paquetes de la sección 2 |
| `rust-lld: error: unable to find library -lxkbcommon` (o `-lxkbcommon-x11`, `-lxcb`) | Faltan los paquetes `-dev` de xkbcommon / xcb | Instalar los paquetes de la sección 2 |
| `signal: 9, SIGKILL` compilando `gpui-fast` | Sin memoria (OOM) | Ver sección 5 |
| `unable to find library -l<otra>` | Falta otro paquete `-dev` | Buscar el paquete que provee `lib<otra>.so` (`apt-file search lib<otra>.so`) e instalarlo |

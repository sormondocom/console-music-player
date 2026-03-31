# console-music-player

<p align="center">
  <img src="assets/mascot.svg" alt="console-music-player mascot — an iPod Classic showing the TUI" width="200"/>
</p>

A cross-platform terminal music player with iPod Classic / Shuffle support and
MOD tracker playback, written in Rust.

---

## Features

- Full-featured TUI (ratatui): library browser, player pane, device pane
- Local library scanner: MP3, M4A, AAC, FLAC, OGG, Opus, WAV, AIFF
- **MOD tracker support**: MOD, XM, IT, S3M, MO3 and 20+ legacy formats
- In-place tag editor (`[E]`): title, artist, album, year, genre
- iPod Classic / Nano / Mini / Shuffle upload via USB Mass Storage
- iTunesDB & iTunesSD read/write — no iTunes required
- iPod health scan and database repair
- Playlists saved as JSON

---

## Dependencies

### Required — always

- **Rust 1.75+** — [rustup.rs](https://rustup.rs)
- **rodio / symphonia** — handled by Cargo, no system library needed

### Required for MOD tracker playback (`tracker` feature, on by default)

The `tracker` feature links against **libopenmpt**, a C++ library.
You must install it before running `cargo build`.

#### Windows

**Option A — vcpkg (recommended)**

```powershell
git clone https://github.com/microsoft/vcpkg
.\vcpkg\bootstrap-vcpkg.bat
.\vcpkg\vcpkg install libopenmpt:x64-windows
$env:VCPKG_ROOT = "$PWD\vcpkg"       # or set permanently in System env vars
```

**Option B — pre-built binaries from OpenMPT**

1. Download the latest `libopenmpt-*-bin-win64.zip` from
   <https://lib.openmpt.org/libopenmpt/download/>
2. Extract and copy `libopenmpt.dll` + `libopenmpt.lib` into your project root
   (or anywhere on `%LIB%` / `%PATH%`).

#### Linux

```bash
# Debian / Ubuntu
sudo apt install libopenmpt-dev

# Fedora / RHEL
sudo dnf install libopenmpt-devel

# Arch
sudo pacman -S libopenmpt
```

#### macOS

```bash
brew install libopenmpt
```

---

## Building

```bash
# With tracker support (default — requires libopenmpt above)
cargo build --release

# Without tracker support (pure Rust, no C++ dep)
cargo build --release --no-default-features

# Run in debug mode with a music library
cargo run --bin cmp -- --library /path/to/Music
```

---

## Controls

### Library screen

| Key | Action |
|-----|--------|
| `↑` / `↓` / `j` / `k` | Navigate tracks |
| `Enter` | Play focused track |
| `Space` | Toggle track selection (for transfer / playlist) |
| `E` | Edit tags (title, artist, album, year, genre) |
| `P` | Pause / resume |
| `[` / `]` | Volume down / up |
| `Tab` | Switch focus Library ↔ Devices |
| `T` | Transfer selected tracks to iPod |
| `S` | Manage source directories |
| `L` | Browse / load playlists |
| `W` | Save current selection as playlist |
| `R` | Rescan library (or clear playlist filter) |
| `D` | Rescan connected devices |
| `I` | Browse iPod library |
| `X` | Scan iPod health |
| `N` | Initialise fresh iTunesDB on iPod |
| `U` | Dump iTunesDB contents to log |
| `Q` | Quit |

### Tag editor overlay (`E`)

| Key | Action |
|-----|--------|
| `Tab` / `↓` / `j` | Next field |
| `↑` / `k` | Previous field |
| `Enter` | Save tags to file |
| `Esc` | Cancel, discard changes |

---

## Supported tracker formats

| Extension | Format | Generation |
|-----------|--------|-----------|
| `.mod` | Amiga ProTracker / NoiseTracker | 1987 |
| `.xm` | FastTracker 2 Extended Module | 1994 |
| `.it` | Impulse Tracker | 1995 |
| `.s3m` | Scream Tracker 3 | 1994 |
| `.mo3` | Compressed MOD/XM/IT/S3M | 2001+ |
| `.mptm` | OpenMPT native | 2004+ |
| `.669`, `.amf`, `.ams`, `.dbm`, `.dmf`, `.dsm`, `.far`, `.mdl`, `.med`, `.mtm`, `.okt`, `.ptm`, `.stm`, `.ult`, `.umx`, `.wow` | Various legacy formats | various |

Playback is provided by **libopenmpt** — the reference-quality tracker engine
used by OpenMPT, VLC, and most modern media players that support these formats.

---

## iPod support

Supported models (USB Mass Storage, no iTunes needed):

| Family | DB format | Folder layout |
|--------|-----------|---------------|
| Classic 1st–6th gen | iTunesDB | `iPod_Control/Music/Fxx/` |
| Mini 1st–2nd gen | iTunesDB | `iPod_Control/Music/Fxx/` |
| Nano 1st–5th gen | iTunesDB | `iPod_Control/Music/Fxx/` |
| Shuffle 1st–3rd gen | iTunesSD | `iPod_Control/Music/` |

iPod touch uses the iOS/AFC protocol and is not supported by this path.

### Windows note

Windows requires iTunes (or the Apple Mobile Device Support package) to be
installed so that the Apple USB driver is available. The iPod must be mounted
as a drive letter — disk mode is enabled automatically on Classic/Nano/Mini
models when connected.

---

## License

GPL-3.0 — see [LICENSE](LICENSE).

---

## Developer notes

> This section is for contributors and maintainers. End users only need the
> **Dependencies** and **Building** sections above.

### Repository layout

```
console-music-player/
├── src/                    # Main binary (TUI, player, library, UI)
│   ├── main.rs             # Entry point, event loop, DLL probe
│   ├── app.rs              # App state machine
│   ├── ui/                 # Ratatui rendering
│   ├── player/             # rodio audio backend
│   ├── tracker/            # libopenmpt wrapper + pure-Rust metadata parsers
│   ├── library/            # Scanner, lofty tag reader/writer
│   └── ...
├── ipod-rs/                # Workspace crate: iTunesDB / iTunesSD / detect
│   └── src/
│       ├── itunesdb.rs     # Binary DB read/write (atomic via .tmp rename)
│       ├── itunessd.rs     # Shuffle SD file read/write
│       └── detect.rs       # iPod health scan (O(n) via HashSet)
├── deps/                   # Vendored native libraries (not committed to git)
│   ├── openmpt.lib         # MSVC import library for libopenmpt
│   └── libopenmpt.dll      # Runtime DLL — copied to target dir by build.rs
├── build.rs                # Copies DLL to output dir; adds /DELAYLOAD on MSVC
├── Cargo.toml              # `tracker` feature gates the openmpt dep
└── .vscode/
    ├── tasks.json          # Build + run tasks (see below)
    └── launch.json         # Attach-only configs (launch via tasks, not F5)
```

### Cargo features

| Feature | Default | Effect |
|---------|---------|--------|
| `tracker` | yes | Enables MOD/XM/IT/S3M playback via libopenmpt |

Build without the tracker feature to get a pure-Rust binary with no C++ dep:

```bash
cargo build --no-default-features
```

### Windows DLL handling

`libopenmpt.dll` is a load-time dependency when the `tracker` feature is
enabled. To make this developer-friendly, two mechanisms work together:

**1. `/DELAYLOAD` linker flag** (`build.rs`)

On MSVC targets, `build.rs` adds `/DELAYLOAD:openmpt.dll` and links
`delayimp.lib`. This defers DLL resolution to the first openmpt call rather
than process start, so `main()` runs even when the DLL is missing.

**2. Runtime DLL probe** (`src/main.rs: check_openmpt_dll`)

`main()` calls `LoadLibraryW("libopenmpt.dll")` before any tracker code
executes. If the DLL is absent, the app prints the exe path, a download URL,
and the `--no-default-features` fallback, then exits with code 1. No cryptic
OS crash dialog.

**3. Task-level pre-flight** (`.vscode/tasks.json: check: libopenmpt.dll`)

The `run (tracker)` VS Code task runs a PowerShell check first. If
`$CARGO_TARGET_DIR\debug\libopenmpt.dll` is missing it prints coloured
instructions and aborts before the build even starts.

**Setting up `deps/` on a new machine (Windows)**

```powershell
# Download the Windows dev package from lib.openmpt.org/libopenmpt/download/
# (the file named libopenmpt-*-dev.zip, not the plain bin zip)
# Extract these files from bin/amd64/ into deps/:
#
#   openmpt.lib            (MSVC import library — from lib/amd64/ inside the zip)
#   libopenmpt.dll         (main runtime DLL)
#   openmpt-mpg123.dll     (MP3 decoder — required by libopenmpt.dll)
#   openmpt-ogg.dll        (Ogg container — required by libopenmpt.dll)
#   openmpt-vorbis.dll     (Vorbis decoder — required by libopenmpt.dll)
#   openmpt-zlib.dll       (zlib — required by libopenmpt.dll)
#
# All six files must be present. libopenmpt.dll will silently fail to load if
# the companion DLLs are missing — this was the original launch failure.
#
# build.rs copies all five DLLs next to cmp.exe automatically on each build.
```

`deps/` is in `.gitignore` — do not commit DLLs or the import lib.

### VS Code workflow

The project deliberately avoids `cppvsdbg` launch configurations for running
the app. `cppvsdbg` injects a debugger into every process it spawns, which
breaks raw-mode TUI applications (crossterm / ratatui) on Windows regardless
of the `console` setting.

**`CARGO_TARGET_DIR` override**

The system-level `CARGO_TARGET_DIR` environment variable may redirect cargo
output outside the workspace (e.g. `D:\rust\cargo`). All tasks override it to
`${workspaceFolder}/target` so that `launch.json` and the DLL pre-flight check
can use a fixed, predictable path. If you change the build task, keep this
override or the exe path in `launch.json` will not exist.

**Running** — use tasks, not F5:

| Task | What it does |
|------|-------------|
| `run` | `cargo run --no-default-features` — no DLL needed, always works |
| `run (tracker)` | DLL pre-flight → build → `cargo run --features tracker` |
| `run (release, tracker)` | Same but `--release` |

Trigger via **Terminal → Run Task** or `Ctrl+Shift+P → Tasks: Run Test Task`.

**Debugging** — `launch.json` only contains attach configs:

```
F5 → Attach to cmp (debug) → pick the running cmp.exe process
```

Start the app first via a run task, then attach.

### iTunesDB / iTunesSD internals

All DB writes go through `ipod_rs::atomic_write`, which writes to a `.tmp`
sibling file and renames atomically. This prevents partial writes from
corrupting the iPod database if the process or power is interrupted.

The `scan_health` function in `ipod-rs/src/detect.rs` was refactored from
O(n²) to O(n) — it builds a `HashSet<u32>` of master-playlist correlation IDs
in a single DB pass, then checks each track against the set.

### Adding a new audio format

1. If the format has a pure-Rust decoder available on crates.io, add it to
   `symphonia` features in `Cargo.toml` — `player/mod.rs` will pick it up
   automatically via the `Decoder` path.
2. If it requires a C library (like libopenmpt), model it after `tracker/`:
   - Add an optional feature in `Cargo.toml`
   - Gate the dep and the implementation behind `#[cfg(feature = "...")]`
   - Add a `/DELAYLOAD` entry in `build.rs` for Windows
   - Add a `check_<lib>_dll()` probe in `main.rs`
   - Add a pre-flight task in `.vscode/tasks.json`

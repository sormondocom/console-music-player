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
- **Magic-byte format verification** — extension mismatches rejected at import time
- **MOD tracker support**: MOD, XM, IT, S3M, MO3 and 20+ legacy formats (optional — see below)
- Real-time **waveform oscilloscope** (`[V]`)
- **Sort & group presets** (`[Z]`): by artist, album, year, month, file extension, or user tags
- **Duplicate finder** (`[F]`): exact-content and metadata-match detection with per-track keep/delete UI
- **User tag library** (`[G]`): tag any track with custom keywords; filter and group by tag
- In-place tag editor (`[E]`): title, artist, album, year, genre
- Playlist badges and tag badges shown inline in the track list
- iPod Classic / Nano / Mini / Shuffle upload via USB Mass Storage
- iTunesDB & iTunesSD read/write — no iTunes required
- iPod health scan and database repair
- Playlists saved as JSON; single-track repeat

---

## Platform support

| Platform | Standard audio | Tracker (MOD/XM/IT…) | iPod transfer |
|----------|:---:|:---:|:---:|
| **Windows 10/11** (x86-64) | ✅ | ✅ (DLLs bundled) | ✅ |
| **Linux** (x86-64, aarch64) | ✅ | ✅ (`apt`/`dnf`/`pacman`) | ✅ |
| **macOS** (x86-64, Apple Silicon) | ✅ | ✅ (`brew`) | ✅ |
| **Android — Termux** (aarch64) | ✅ | ✅ (`pkg install libopenmpt`) | ⚠️ (USB limited) |

> iPod transfer on Android depends on the device exposing the iPod as USB Mass
> Storage. Many modern Android phones require a USB-OTG adapter and a file
> manager that mounts the device at a known path.

---

## Dependencies

### Required — always (no system libraries needed)

- **Rust 1.75+** — [rustup.rs](https://rustup.rs)
- All audio decoding (MP3, FLAC, AAC, OGG, WAV, AIFF) is handled by
  [symphonia](https://github.com/pdeljanov/Symphonia) via Cargo — no system
  library required.

### Optional — MOD tracker playback (`--features tracker`)

Tracker playback links against **libopenmpt**, a C++ library.  Install it for
your platform before passing `--features tracker`.

#### Windows

Pre-built DLLs are bundled in `deps/` — no package manager needed.  See
[Setting up deps/ on Windows](#setting-up-deps-on-a-new-machine-windows) in the
Developer Notes section.

#### Linux

```bash
# Debian / Ubuntu / Raspberry Pi OS
sudo apt install libopenmpt-dev

# Fedora / RHEL / CentOS Stream
sudo dnf install libopenmpt-devel

# Arch Linux / Manjaro
sudo pacman -S libopenmpt
```

#### macOS

```bash
brew install libopenmpt
```

#### Android — Termux

```bash
pkg install libopenmpt
```

libopenmpt links against Android's shared C++ runtime (`libc++_shared.so`).
The project's `.cargo/config.toml` adds the correct linker flag automatically —
no extra steps needed after the `pkg install`.

---

## Building

```bash
# Full build — tracker included by default (requires libopenmpt — see above)
cargo build --release

# Without tracker — pure Rust, no C++ dep, works anywhere without libopenmpt
cargo build --release --no-default-features

# Run directly from source with a music library path
cargo run -- --library /path/to/Music
```

> **Windows users:** the VS Code default tasks (Ctrl+Shift+B / Ctrl+Shift+P →
> Run Task) build with `--features tracker` and copy the bundled DLLs
> automatically.

> **Termux users:** install libopenmpt first (`pkg install libopenmpt`), then
> `cargo build` gives the full player including tracker support. If you prefer
> not to install libopenmpt, use `cargo build --no-default-features`.

---

## Controls

### Library screen

| Key | Action |
|-----|--------|
| `↑` / `↓` or `k` / `j` | Navigate tracks |
| `Page Up` / `Page Down` | Jump 10 tracks |
| `Enter` | Play focused track |
| `Space` | Toggle track selection (for transfer / playlist) |
| `P` | Pause / resume |
| `[` / `]` | Volume down / up |
| `O` | Toggle single-track repeat |
| `V` | Toggle waveform oscilloscope |
| `Z` | Cycle sort / group-by preset |
| `Tab` | Switch focus Library ↔ Devices |
| `E` | Edit tags (title, artist, album, year, genre) |
| `G` | Edit user tags / keywords for focused track |
| `F` | Find duplicates |
| `S` | Manage source directories |
| `L` | Browse / load playlists |
| `W` | Save current selection as playlist |
| `R` | Rescan library (or clear active playlist filter) |
| `D` | Rescan connected devices |
| `T` | Transfer selected tracks to iPod |
| `I` | Browse iPod library |
| `X` | Scan iPod health |
| `N` | Initialise fresh iTunesDB on iPod |
| `U` | Dump iTunesDB contents to log |
| `Q` | Quit |

### Sort / Group-by presets (`Z`)

Pressing `Z` cycles through these presets in order:

| Preset | Behaviour |
|--------|-----------|
| Original Order | Restore initial scan order |
| Artist / Album | Artist → Album → Title (default) |
| Title | Alphabetical by title |
| Album | Album → Title |
| Duration ↓ | Longest tracks first |
| Date Added | Newest file first (by mtime) |
| Group by Extension | Sections: FLAC, IT, MOD, MP3, XM … |
| Group by Artist | Sections per artist name |
| Group by Year | Sections per release year (unknown last) |
| Group by Month | Sections per mtime year · month |
| Group by Tag | Sections per user tag (untagged last) |

### Tag editor overlay (`G`)

| Key | Action |
|-----|--------|
| Any character | Append to tag input |
| `Backspace` | Delete last character |
| `Enter` | Save tags to disk |
| `Esc` | Cancel, discard changes |

Tags are comma-separated keywords (e.g. `rock, 80s, favourite`).  They are
normalised to lowercase and deduplicated on save.  Tags appear as `#tag` badges
inline in the track list and can be grouped with `Z → Group by Tag`.

### Tag editor overlay (`E`)

| Key | Action |
|-----|--------|
| `Tab` / `↓` / `j` | Next field |
| `↑` / `k` | Previous field |
| `Enter` | Save tags to file |
| `Esc` | Cancel, discard changes |

### Duplicate finder (`F`)

| Key | Action |
|-----|--------|
| `↑` / `↓` | Navigate duplicate groups (left panel) |
| `Tab` | Switch focus: group list ↔ candidates |
| `Space` | Cycle action for focused candidate (Keep / Delete / ?) |
| `A` | Auto-suggest best action for all groups |
| `Enter` | Execute all Delete actions (moves to Trash) |
| `Esc` | Cancel, return to library |

### Waveform oscilloscope (`V`)

Replaces the library pane with a real-time oscilloscope trace of the playing
audio.  Uses Unicode half-block characters (▀ ▄ █) for double vertical
resolution.  Press `V` or `Esc` to return to the library.

---

## Track list badges

Each track row can display two types of inline badges:

- **`‹PlaylistName›`** (blue) — the playlists this track belongs to (up to 2 shown; `+N` for overflow)
- **`#tag`** (magenta) — user-defined keywords (up to 3 shown; `+N` for overflow)

Badges are rebuilt automatically whenever playlists or tags change.

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
installed so that the Apple USB driver is available.  The iPod must be mounted
as a drive letter — disk mode is enabled automatically on Classic/Nano/Mini
models when connected.

---

## License

GPL-3.0 — see [LICENSE](LICENSE).

---

## Developer notes

> This section is for contributors and maintainers.  End users only need the
> **Platform support**, **Dependencies**, and **Building** sections above.

### Repository layout

```
console-music-player/
├── src/
│   ├── main.rs             # Entry point, event loop, DLL probe
│   ├── app.rs              # App state machine
│   ├── ui/mod.rs           # Ratatui rendering (all screens + overlays)
│   ├── player/mod.rs       # rodio audio backend + waveform sample capture
│   ├── visualizer.rs       # SampleCapture source wrapper + oscilloscope renderer
│   ├── tracker/mod.rs      # libopenmpt wrapper + pure-Rust metadata parsers
│   ├── library/
│   │   ├── mod.rs          # Library state, sort/group-by presets, Track struct
│   │   ├── scanner.rs      # Filesystem scan, lofty tag reader/writer, magic-byte gate
│   │   ├── dedup.rs        # Duplicate detection (exact-content + metadata match)
│   │   └── magic.rs        # Magic-byte format verification
│   ├── tags.rs             # User keyword tag store (tags.json)
│   └── ...
├── ipod-rs/                # Workspace crate: iTunesDB / iTunesSD / detect
│   └── src/
│       ├── itunesdb.rs     # Binary DB read/write (atomic via .tmp rename)
│       ├── itunessd.rs     # Shuffle SD file read/write
│       └── detect.rs       # iPod health scan (O(n) via HashSet)
├── deps/                   # Vendored Windows DLLs (not committed to git)
│   ├── openmpt.lib         # MSVC import library
│   ├── libopenmpt.dll      # Main runtime DLL
│   └── openmpt-*.dll       # Companion DLLs (mpg123, ogg, vorbis, zlib)
├── .cargo/config.toml      # Target-specific linker flags (Android c++_shared)
├── build.rs                # Copies DLLs to output dir; adds /DELAYLOAD on MSVC
├── Cargo.toml              # `tracker` feature gates the openmpt dep (opt-in)
└── .vscode/
    ├── tasks.json          # Build + run tasks (tracker variants are default)
    └── launch.json         # Attach-only configs (launch via tasks, not F5)
```

### Cargo features

| Feature | Default | Effect |
|---------|:-------:|--------|
| `tracker` | **yes** | Enables MOD/XM/IT/S3M playback via libopenmpt |

`tracker` is on by default for the best out-of-the-box experience on desktop
platforms.  Platform-specific build commands:

| Platform | Command |
|----------|---------|
| Windows / Linux / macOS | `cargo build` (tracker included — install libopenmpt first) |
| Android / Termux with libopenmpt | `pkg install libopenmpt` then `cargo build` |
| Android / Termux without libopenmpt | `cargo build --no-default-features` |

### Android / Termux — C++ runtime details

libopenmpt is a C++ library.  On Android the only available C++ runtime is the
*shared* one (`libc++_shared.so`); the static variant (`libc++_static`) does
not exist in the Termux NDK environment.  The `openmpt` crate's build script
may emit a link directive for `c++_static`, causing a linker error.

`.cargo/config.toml` adds `-lc++_shared` for all `android` targets, which
satisfies the C++ runtime requirement through the shared library.  No manual
environment variable configuration is needed.

```
Error: unable to find library -lopenmpt     → pkg install libopenmpt
Error: unable to find library lc++_static   → fixed automatically by .cargo/config.toml
```

### Windows DLL handling

`libopenmpt.dll` is a load-time dependency when the `tracker` feature is
enabled.  To make this developer-friendly, three mechanisms work together:

**1. `/DELAYLOAD` linker flag** (`build.rs`)

On MSVC targets, `build.rs` adds `/DELAYLOAD:openmpt.dll` and links
`delayimp.lib`.  This defers DLL resolution to the first openmpt call rather
than process start, so `main()` runs even when the DLL is missing.

**2. Runtime DLL probe** (`src/main.rs: check_openmpt_dll`)

`main()` calls `LoadLibraryW("libopenmpt.dll")` before any tracker code
executes.  If the DLL is absent the app prints the exe path, a download URL,
and the `--no-default-features` fallback, then exits with code 1.  No cryptic
OS crash dialog.

**3. Companion DLLs** (`deps/`)

`libopenmpt.dll` itself depends on four companion DLLs that must be present in
the same directory:

| DLL | Purpose |
|-----|---------|
| `openmpt-mpg123.dll` | MP3 decoding inside tracker files |
| `openmpt-ogg.dll` | Ogg container |
| `openmpt-vorbis.dll` | Vorbis audio codec |
| `openmpt-zlib.dll` | Compression |

`build.rs` copies all five DLLs next to `cmp.exe` on every build.

#### Setting up `deps/` on a new machine (Windows)

```powershell
# Download the Windows dev package from lib.openmpt.org/libopenmpt/download/
# Use the file named libopenmpt-*-dev.zip (not the plain bin zip).
#
# Extract these files into deps/:
#
#   From lib/amd64/  →  openmpt.lib
#   From bin/amd64/  →  libopenmpt.dll
#                        openmpt-mpg123.dll
#                        openmpt-ogg.dll
#                        openmpt-vorbis.dll
#                        openmpt-zlib.dll
#
# All six files must be present. libopenmpt.dll will silently fail to load if
# the companion DLLs are missing.
```

`deps/` is in `.gitignore` — do not commit DLLs or the import library.

### VS Code workflow

The project deliberately avoids `cppvsdbg` launch configurations for running
the app.  `cppvsdbg` injects a debugger into every process it spawns, which
breaks raw-mode TUI applications (crossterm / ratatui) on Windows regardless
of the `console` setting.

**`CARGO_TARGET_DIR` override**

The system-level `CARGO_TARGET_DIR` environment variable may redirect cargo
output outside the workspace (e.g. `D:\rust\cargo`).  All tasks override it to
`${workspaceFolder}/target` so that `launch.json` and the DLL pre-flight check
can use a fixed, predictable path.

**Running** — use tasks, not F5:

| Task | Default? | What it does |
|------|:--------:|-------------|
| `build (tracker)` | ✅ | `cargo build --features tracker` |
| `run (tracker)` | ✅ | Build with tracker → launch in new console window |
| `run (release, tracker)` | | Release build → launch |
| `build` | | `cargo build` — no tracker, works without DLLs |
| `run` | | Build without tracker → launch |

Trigger via **Terminal → Run Task** or bind `Ctrl+Shift+P → Tasks: Run Test Task`.

**Debugging** — `launch.json` only contains attach configs:

```
F5 → Attach to cmp (debug) → pick the running cmp.exe process
```

Start the app first via a run task, then attach.

### Data files

| File | Location | Contents |
|------|----------|----------|
| `config.json` | `%APPDATA%\console-music-player\` (Win) / `~/.config/console-music-player/` | Source directories |
| `tags.json` | same directory | User keyword tags per track path |
| `*.json` (playlists) | same directory | Track path lists per named playlist |

### iTunesDB / iTunesSD internals

All DB writes go through `ipod_rs::atomic_write`, which writes to a `.tmp`
sibling file and renames atomically.  This prevents partial writes from
corrupting the iPod database if the process or power is interrupted.

The `scan_health` function in `ipod-rs/src/detect.rs` was refactored from
O(n²) to O(n) — it builds a `HashSet<u32>` of master-playlist correlation IDs
in a single DB pass, then checks each track against the set.

### Adding a new audio format

1. If the format has a pure-Rust decoder on crates.io, add it to the
   `symphonia` features in `Cargo.toml` — `player/mod.rs` picks it up
   automatically via the `Decoder` path.
2. Add the file extension to `LOFTY_EXTENSIONS` in `library/scanner.rs`.
3. Add magic-byte detection for it in `library/magic.rs` (`detect_format`).
4. If it requires a C library, model it after `tracker/`:
   - Add an optional feature in `Cargo.toml`
   - Gate the dep and implementation behind `#[cfg(feature = "...")]`
   - Add a `/DELAYLOAD` entry in `build.rs` for Windows
   - Add a `check_<lib>_dll()` probe in `main.rs`
   - Update `.cargo/config.toml` if the library needs special Android linkage

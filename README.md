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

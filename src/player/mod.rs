//! Audio playback engine wrapping `rodio`, with an `mpv` subprocess fallback
//! for platforms where rodio/cpal cannot access the audio system (e.g. Termux).

use std::fs::File;
use std::io::BufReader;
use std::time::{Duration, Instant};

use rodio::{Decoder, OutputStreamHandle, Sink, Source};
use tracing::info;
#[cfg(unix)]
use tracing::warn;

use crate::library::Track;
use crate::media::MediaItem;
use crate::tracker;
use crate::visualizer::{self, DecoderPanicFlag, SampleCapture, WaveBuffer};

// ---------------------------------------------------------------------------
// External player (mpv subprocess via IPC socket)
// ---------------------------------------------------------------------------

/// Returns the name of an external audio player binary found in PATH, or None.
/// Only `mpv` is supported (it provides a Unix IPC socket for control).
pub fn probe_external_player() -> Option<String> {
    #[cfg(unix)]
    {
        for candidate in &["mpv"] {
            if std::process::Command::new("which")
                .arg(candidate)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

/// A running `mpv` process controlled over its IPC socket.
#[cfg(unix)]
struct ExternalHandle {
    process: std::process::Child,
    /// Path to the Unix socket mpv is listening on.
    sock_path: std::path::PathBuf,
    started_at: Option<Instant>,
    elapsed_before_pause: Duration,
    paused: bool,
}

#[cfg(unix)]
impl ExternalHandle {
    fn spawn(file: &std::path::Path, volume_pct: u32) -> Result<Self, String> {
        // Place the socket in the OS temp dir so it's always writable.
        let sock_path = std::env::temp_dir()
            .join(format!("cmp-mpv-{}.sock", std::process::id()));

        let process = std::process::Command::new("mpv")
            .args([
                "--no-terminal",
                "--no-video",
                "--really-quiet",
                &format!("--input-ipc-server={}", sock_path.display()),
                &format!("--volume={volume_pct}"),
                file.to_str().unwrap_or(""),
            ])
            .spawn()
            .map_err(|e| format!("Could not spawn mpv: {e}"))?;

        Ok(Self {
            process,
            sock_path,
            started_at: Some(Instant::now()),
            elapsed_before_pause: Duration::ZERO,
            paused: false,
        })
    }

    /// Send a JSON command to the mpv IPC socket.
    /// Retries briefly to allow mpv time to create the socket after startup.
    fn send(&self, json: &str) {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        for attempt in 0..20 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(50));
            }
            if let Ok(mut s) = UnixStream::connect(&self.sock_path) {
                let _ = write!(s, "{json}\n");
                return;
            }
        }
        warn!("mpv IPC: could not connect to {:?}", self.sock_path);
    }

    fn pause(&mut self) {
        self.send(r#"{"command":["set_property","pause",true]}"#);
        if !self.paused {
            if let Some(t) = self.started_at.take() {
                self.elapsed_before_pause += t.elapsed();
            }
            self.paused = true;
        }
    }

    fn resume(&mut self) {
        self.send(r#"{"command":["set_property","pause",false]}"#);
        if self.paused {
            self.started_at = Some(Instant::now());
            self.paused = false;
        }
    }

    fn stop(&mut self) {
        self.send(r#"{"command":["quit"]}"#);
        let _ = self.process.wait();
        let _ = std::fs::remove_file(&self.sock_path);
    }

    fn is_finished(&mut self) -> bool {
        self.process.try_wait().map(|r| r.is_some()).unwrap_or(false)
    }

    fn elapsed(&self) -> Duration {
        if self.paused {
            self.elapsed_before_pause
        } else {
            self.elapsed_before_pause
                + self.started_at.map(|t| t.elapsed()).unwrap_or_default()
        }
    }

    fn set_volume(&self, volume_pct: u32) {
        self.send(&format!(r#"{{"command":["set_property","volume",{volume_pct}]}}"#));
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
}

impl PlaybackState {
    pub fn icon(&self) -> &'static str {
        match self {
            PlaybackState::Playing => "▶",
            PlaybackState::Paused  => "⏸",
            PlaybackState::Stopped => "■",
        }
    }
}

/// Single-track repeat mode.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RepeatMode {
    #[default]
    Off,
    /// Replay the current track automatically when it ends.
    One,
}

impl RepeatMode {
    pub fn toggle(&self) -> Self {
        match self {
            Self::Off => Self::One,
            Self::One => Self::Off,
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Off => "  ",
            Self::One => "🔂",
        }
    }
}

// ---------------------------------------------------------------------------
// Player
// ---------------------------------------------------------------------------

pub struct Player {
    handle: Option<OutputStreamHandle>,
    sink: Option<Sink>,

    pub current_track: Option<Track>,
    pub state: PlaybackState,
    pub repeat: RepeatMode,
    /// Volume 0.0..=1.0
    pub volume: f32,

    /// Shared sample ring-buffer fed by the audio thread, read by the UI.
    pub wave_buffer: WaveBuffer,

    /// Set by `SampleCapture` when the decoder panics mid-stream.
    /// Polled each tick; cleared when the player stops or a new track starts.
    pub decoder_panic_flag: DecoderPanicFlag,

    /// Wall-clock moment the current play/resume started (rodio path).
    started_at: Option<Instant>,
    /// Accumulated playback time before the last pause (rodio path).
    elapsed_before_pause: Duration,

    /// External mpv process, used when rodio is unavailable (e.g. Termux).
    #[cfg(unix)]
    ext: Option<ExternalHandle>,
    /// Name of the detected external player binary, e.g. "mpv".
    pub ext_player: Option<String>,
}

impl Player {
    pub fn new(handle: Option<OutputStreamHandle>) -> Self {
        let ext_player = if handle.is_none() {
            probe_external_player()
        } else {
            None
        };
        Self {
            handle,
            sink: None,
            current_track: None,
            state: PlaybackState::Stopped,
            repeat: RepeatMode::Off,
            volume: 0.8,
            wave_buffer: visualizer::new_wave_buffer(),
            decoder_panic_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            started_at: None,
            elapsed_before_pause: Duration::ZERO,
            #[cfg(unix)]
            ext: None,
            ext_player,
        }
    }

    // --- transport ---

    /// Start playing `track`.
    ///
    /// Returns `Err(message)` with a user-facing description if playback
    /// cannot start (missing audio device, unreadable file, unsupported
    /// format, missing tracker feature, etc.).  On error the player is left
    /// in the `Stopped` state.
    pub fn play(&mut self, track: &Track) -> Result<(), String> {
        self.decoder_panic_flag.store(false, std::sync::atomic::Ordering::Relaxed);
        self.stop_internal();

        // ── rodio path ────────────────────────────────────────────────────
        if let Some(handle) = &self.handle {
            let sink = Sink::try_new(handle)
                .map_err(|e| format!("Cannot create audio sink: {e}"))?;

            let ext = track
                .path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .unwrap_or_default();

            if tracker::is_tracker_ext(&ext) {
                self.play_tracker(track, &sink)?;
            } else {
                self.play_standard(track, &sink)?;
            }

            self.sink = Some(sink);
            self.current_track = Some(track.clone());
            self.state = PlaybackState::Playing;
            self.started_at = Some(Instant::now());
            self.elapsed_before_pause = Duration::ZERO;
            info!("Playing (rodio): {} — {}", track.display_artist(), track.display_title());
            return Ok(());
        }

        // ── external player path (e.g. mpv on Termux) ────────────────────
        #[cfg(unix)]
        {
            if self.ext_player.is_some() {
                let vol_pct = (self.volume * 100.0).round() as u32;
                let handle = ExternalHandle::spawn(&track.path, vol_pct)
                    .map_err(|e| e.to_string())?;
                self.ext = Some(handle);
                self.current_track = Some(track.clone());
                self.state = PlaybackState::Playing;
                info!("Playing (mpv): {} — {}", track.display_artist(), track.display_title());
                return Ok(());
            }
        }

        Err("No audio output device and no external player found.\n\
             On Termux, run:  pkg install mpv".into())
    }

    /// Decode and append a standard audio file (MP3, FLAC, etc.) via rodio/symphonia.
    fn play_standard(&self, track: &Track, sink: &Sink) -> Result<(), String> {
        let file = File::open(&track.path)
            .map_err(|e| format!("Cannot open '{}': {e}", track.path.display()))?;
        let source = Decoder::new(BufReader::new(file))
            .map_err(|e| format!("Cannot decode '{}': {e}", track.path.display()))?
            .convert_samples::<f32>();
        sink.set_volume(self.volume);
        sink.append(SampleCapture::new(
            source,
            self.wave_buffer.clone(),
            self.decoder_panic_flag.clone(),
        ));
        Ok(())
    }

    /// Decode and append a tracker module via libopenmpt.
    fn play_tracker(&self, track: &Track, sink: &Sink) -> Result<(), String> {
        #[cfg(feature = "tracker")]
        {
            let data = std::fs::read(&track.path)
                .map_err(|e| format!("Cannot read '{}': {e}", track.path.display()))?;
            match crate::tracker::TrackerSource::from_bytes(&data) {
                Some(source) => {
                    sink.set_volume(self.volume);
                    sink.append(SampleCapture::new(
                        source.convert_samples::<f32>(),
                        self.wave_buffer.clone(),
                        self.decoder_panic_flag.clone(),
                    ));
                    Ok(())
                }
                None => Err(format!(
                    "libopenmpt could not parse '{}'",
                    track.path.display()
                )),
            }
        }
        #[cfg(not(feature = "tracker"))]
        {
            let _ = (track, sink);
            Err("Tracker playback is not compiled in — \
                 build with --features tracker and install libopenmpt."
                .into())
        }
    }

    pub fn toggle_pause(&mut self) {
        match self.state {
            PlaybackState::Playing => self.pause(),
            PlaybackState::Paused  => self.resume(),
            PlaybackState::Stopped => {}
        }
    }

    pub fn stop(&mut self) {
        self.stop_internal();
        self.current_track = None;
    }

    // --- volume ---

    pub fn volume_up(&mut self) {
        self.set_volume(self.volume + 0.05);
    }

    pub fn volume_down(&mut self) {
        self.set_volume((self.volume - 0.05).max(0.0));
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        if let Some(sink) = &self.sink {
            sink.set_volume(self.volume);
        }
        #[cfg(unix)]
        if let Some(ext) = &self.ext {
            ext.set_volume((self.volume * 100.0).round() as u32);
        }
    }

    // --- queries ---

    /// Elapsed playback time for the current track.
    pub fn elapsed(&self) -> Duration {
        #[cfg(unix)]
        if let Some(ext) = &self.ext {
            return ext.elapsed();
        }
        match self.state {
            PlaybackState::Stopped => Duration::ZERO,
            PlaybackState::Paused  => self.elapsed_before_pause,
            PlaybackState::Playing => {
                let since_resume = self.started_at.map(|t| t.elapsed()).unwrap_or_default();
                self.elapsed_before_pause + since_resume
            }
        }
    }

    /// Progress 0.0..=1.0 based on track duration metadata.
    pub fn progress(&self) -> f64 {
        let duration_secs = self
            .current_track
            .as_ref()
            .and_then(|t| t.duration_secs)
            .unwrap_or(0) as f64;

        if duration_secs == 0.0 {
            return 0.0;
        }
        (self.elapsed().as_secs_f64() / duration_secs).min(1.0)
    }

    /// Volume bars string for display (e.g. "████████░░").
    pub fn volume_bar(&self) -> String {
        let filled = (self.volume * 10.0).round() as usize;
        let empty = 10usize.saturating_sub(filled);
        format!("{}{}", "█".repeat(filled), "░".repeat(empty))
    }

    /// Toggle single-track repeat on/off.
    pub fn toggle_repeat(&mut self) {
        self.repeat = self.repeat.toggle();
    }

    /// Called every tick — detects natural end-of-track and handles repeat.
    pub fn tick(&mut self) {
        if self.state != PlaybackState::Playing {
            return;
        }

        let finished = self.sink.as_ref().map(|s| s.empty()).unwrap_or(false)
            || {
                #[cfg(unix)]
                { self.ext.as_mut().map(|e| e.is_finished()).unwrap_or(false) }
                #[cfg(not(unix))]
                { false }
            };

        if finished {
            if self.repeat == RepeatMode::One {
                if let Some(track) = self.current_track.clone() {
                    let _ = self.play(&track);
                    return;
                }
            }
            self.state = PlaybackState::Stopped;
            self.started_at = None;
            #[cfg(unix)]
            { self.ext = None; }
        }
    }

    /// Returns `Some(track)` if the decoder panicked since the last call, then
    /// clears the flag.  Returns `None` if no panic has occurred.
    pub fn take_decoder_panic(&mut self) -> Option<Track> {
        if self.decoder_panic_flag.swap(false, std::sync::atomic::Ordering::Relaxed) {
            self.state = PlaybackState::Stopped;
            self.started_at = None;
            self.current_track.take()
        } else {
            None
        }
    }

    // --- private ---

    fn pause(&mut self) {
        if let Some(sink) = &self.sink {
            sink.pause();
            if let Some(t) = self.started_at.take() {
                self.elapsed_before_pause += t.elapsed();
            }
            self.state = PlaybackState::Paused;
        }
        #[cfg(unix)]
        if let Some(ext) = &mut self.ext {
            ext.pause();
            self.state = PlaybackState::Paused;
        }
    }

    fn resume(&mut self) {
        if let Some(sink) = &self.sink {
            sink.play();
            self.started_at = Some(Instant::now());
            self.state = PlaybackState::Playing;
        }
        #[cfg(unix)]
        if let Some(ext) = &mut self.ext {
            ext.resume();
            self.state = PlaybackState::Playing;
        }
    }

    fn stop_internal(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
        #[cfg(unix)]
        if let Some(mut ext) = self.ext.take() {
            ext.stop();
        }
        self.state = PlaybackState::Stopped;
        self.started_at = None;
        self.elapsed_before_pause = Duration::ZERO;
    }
}

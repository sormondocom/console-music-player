//! Audio playback engine wrapping `rodio`.

use std::fs::File;
use std::io::BufReader;
use std::time::{Duration, Instant};

use rodio::{Decoder, OutputStreamHandle, Sink};
use tracing::{info, warn};

use crate::library::Track;
use crate::media::MediaItem;
use crate::tracker;

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

// ---------------------------------------------------------------------------
// Player
// ---------------------------------------------------------------------------

pub struct Player {
    handle: Option<OutputStreamHandle>,
    sink: Option<Sink>,

    pub current_track: Option<Track>,
    pub state: PlaybackState,
    /// Volume 0.0..=1.0
    pub volume: f32,

    /// Wall-clock moment the current play/resume started.
    started_at: Option<Instant>,
    /// Accumulated playback time before the last pause.
    elapsed_before_pause: Duration,
}

impl Player {
    pub fn new(handle: Option<OutputStreamHandle>) -> Self {
        Self {
            handle,
            sink: None,
            current_track: None,
            state: PlaybackState::Stopped,
            volume: 0.8,
            started_at: None,
            elapsed_before_pause: Duration::ZERO,
        }
    }

    // --- transport ---

    pub fn play(&mut self, track: &Track) {
        self.stop_internal();

        let Some(handle) = &self.handle else {
            warn!("No audio output — playback unavailable.");
            return;
        };

        let sink = match Sink::try_new(handle) {
            Ok(s) => s,
            Err(e) => {
                warn!("Cannot create audio sink: {e}");
                return;
            }
        };

        let ext = track
            .path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        if tracker::is_tracker_ext(&ext) {
            if !self.play_tracker(track, &sink) {
                return;
            }
        } else if !self.play_standard(track, &sink) {
            return;
        }

        self.sink = Some(sink);
        self.current_track = Some(track.clone());
        self.state = PlaybackState::Playing;
        self.started_at = Some(Instant::now());
        self.elapsed_before_pause = Duration::ZERO;

        info!("Playing: {} — {}", track.display_artist(), track.display_title());
    }

    /// Decode and append a standard audio file (MP3, FLAC, etc.) via rodio/symphonia.
    fn play_standard(&self, track: &Track, sink: &Sink) -> bool {
        let file = match File::open(&track.path) {
            Ok(f) => f,
            Err(e) => {
                warn!("Cannot open {}: {e}", track.path.display());
                return false;
            }
        };
        let source = match Decoder::new(BufReader::new(file)) {
            Ok(s) => s,
            Err(e) => {
                warn!("Cannot decode {}: {e}", track.path.display());
                return false;
            }
        };
        sink.set_volume(self.volume);
        sink.append(source);
        true
    }

    /// Decode and append a tracker module via libopenmpt.
    fn play_tracker(&self, track: &Track, sink: &Sink) -> bool {
        #[cfg(feature = "tracker")]
        {
            let data = match std::fs::read(&track.path) {
                Ok(d) => d,
                Err(e) => {
                    warn!("Cannot read tracker file {}: {e}", track.path.display());
                    return false;
                }
            };
            match crate::tracker::TrackerSource::from_bytes(&data) {
                Some(source) => {
                    sink.set_volume(self.volume);
                    sink.append(source);
                    true
                }
                None => {
                    warn!(
                        "libopenmpt could not parse tracker file: {}",
                        track.path.display()
                    );
                    false
                }
            }
        }
        #[cfg(not(feature = "tracker"))]
        {
            warn!(
                "Tracker playback is not compiled in. \
                 Build with --features tracker and install libopenmpt."
            );
            let _ = track;
            let _ = sink;
            false
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
    }

    // --- queries ---

    /// Elapsed playback time for the current track.
    pub fn elapsed(&self) -> Duration {
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

    /// Called every tick — detects natural end-of-track.
    pub fn tick(&mut self) {
        if self.state == PlaybackState::Playing {
            if self.sink.as_ref().map(|s| s.empty()).unwrap_or(false) {
                self.state = PlaybackState::Stopped;
                self.started_at = None;
            }
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
    }

    fn resume(&mut self) {
        if let Some(sink) = &self.sink {
            sink.play();
            self.started_at = Some(Instant::now());
            self.state = PlaybackState::Playing;
        }
    }

    fn stop_internal(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
        self.state = PlaybackState::Stopped;
        self.started_at = None;
        self.elapsed_before_pause = Duration::ZERO;
    }
}

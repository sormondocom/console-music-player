//! Real-time waveform capture and terminal rendering.
//!
//! [`SampleCapture`] is a thin `rodio::Source` wrapper that tees every `f32`
//! sample into a shared ring buffer without blocking the audio thread.  The
//! buffer is read each UI tick by the active visualizer, which uses Unicode
//! half-block characters (`▀` `▄` `█` ` `) to achieve double the terminal's
//! native vertical resolution.
//!
//! # Visualizer modes
//! - [`VizMode::Waveform`]    — oscilloscope bar trace
//! - [`VizMode::Spirograph`]  — audio-reactive hypotrochoid (spirograph toy curve)

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::Source;

/// Shared flag: set by `SampleCapture` when the inner decoder panics.
/// The player polls this each tick so it can surface the error to the UI.
pub type DecoderPanicFlag = Arc<AtomicBool>;

// ---------------------------------------------------------------------------
// Shared sample buffer
// ---------------------------------------------------------------------------

/// How many `f32` samples to hold in the ring buffer.
/// At 48 kHz stereo this is ~85 ms — enough for a smooth trace.
pub const BUFFER_SIZE: usize = 8_192;

/// Thread-safe ring buffer shared between the audio thread and the UI thread.
pub type WaveBuffer = Arc<Mutex<VecDeque<f32>>>;

pub fn new_wave_buffer() -> WaveBuffer {
    Arc::new(Mutex::new(VecDeque::with_capacity(BUFFER_SIZE)))
}

// ---------------------------------------------------------------------------
// Sample-capture source wrapper
// ---------------------------------------------------------------------------

/// Wraps any `rodio::Source<Item = f32>`, recording every sample into a
/// shared [`WaveBuffer`] without any allocations on the audio thread's hot
/// path.
///
/// Also catches any panics from the inner decoder (e.g. symphonia internal
/// `unreachable!()` on corrupt or edge-case files): sets `panic_flag` and
/// returns `None` (end of stream) so the audio thread keeps running.
pub struct SampleCapture<S: Source<Item = f32>> {
    inner:      S,
    buffer:     WaveBuffer,
    panic_flag: DecoderPanicFlag,
    /// Set locally once we've signalled — avoids repeated atomic writes.
    panicked:   bool,
}

impl<S: Source<Item = f32>> SampleCapture<S> {
    pub fn new(inner: S, buffer: WaveBuffer, panic_flag: DecoderPanicFlag) -> Self {
        Self { inner, buffer, panic_flag, panicked: false }
    }
}

impl<S: Source<Item = f32>> Iterator for SampleCapture<S> {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        if self.panicked {
            return None;
        }
        // Wrap the inner decoder call so a symphonia `unreachable!()`/panic
        // is caught here rather than unwinding rodio's audio thread.
        let result = std::panic::catch_unwind(
            std::panic::AssertUnwindSafe(|| self.inner.next())
        );
        match result {
            Ok(sample) => {
                let s = sample?;
                // `try_lock` — never blocks the audio thread if the UI is mid-read.
                if let Ok(mut buf) = self.buffer.try_lock() {
                    if buf.len() >= BUFFER_SIZE {
                        buf.pop_front();
                    }
                    buf.push_back(s);
                }
                Some(s)
            }
            Err(_) => {
                // Decoder panicked — signal the main thread and stop.
                self.panicked = true;
                self.panic_flag.store(true, Ordering::Relaxed);
                None
            }
        }
    }
}

impl<S: Source<Item = f32>> Source for SampleCapture<S> {
    fn current_frame_len(&self) -> Option<usize> { self.inner.current_frame_len() }
    fn channels(&self)                -> u16      { self.inner.channels() }
    fn sample_rate(&self)             -> u32      { self.inner.sample_rate() }
    fn total_duration(&self)          -> Option<Duration> { self.inner.total_duration() }
}

// ---------------------------------------------------------------------------
// Waveform renderer
// ---------------------------------------------------------------------------

/// Render the contents of `buffer` into a `width` × `height` character grid.
///
/// Returns one `String` per row (top → bottom).  Uses Unicode half-block
/// characters (`▀` `▄` `█` ` `) so each character cell carries *two* pixel
/// rows, effectively doubling the vertical resolution.
///
/// The waveform is an oscilloscope trace — signed amplitude, centred on a
/// zero line that is always visible.  A flat centre line is drawn when the
/// buffer is empty (nothing playing / silence).
pub fn render_waveform(buffer: &WaveBuffer, width: usize, height: usize) -> Vec<String> {
    if width == 0 || height == 0 {
        return Vec::new();
    }

    let samples: Vec<f32> = match buffer.try_lock() {
        Ok(g)  => g.iter().copied().collect(),
        Err(_) => return vec![" ".repeat(width); height],
    };

    if samples.is_empty() {
        return flat_line(width, height);
    }

    // ------------------------------------------------------------------
    // Build per-column amplitude peaks.
    //
    // The buffer may contain interleaved stereo (L, R, L, R, …).  We mix
    // to mono by averaging adjacent pairs before taking the peak, so the
    // oscilloscope trace represents the combined signal.
    // ------------------------------------------------------------------
    let mono: Vec<f32> = samples
        .chunks(2)
        .map(|c| if c.len() == 2 { (c[0] + c[1]) * 0.5 } else { c[0] })
        .collect();

    let mut peaks: Vec<f32> = Vec::with_capacity(width);
    for col in 0..width {
        let start = col * mono.len() / width;
        let end   = ((col + 1) * mono.len() / width).max(start + 1).min(mono.len());
        let window = &mono[start..end];
        // Signed peak: whichever extreme (+ or −) has the larger magnitude.
        let peak = window.iter().copied().fold(0.0f32, |acc, s| {
            if s.abs() > acc.abs() { s } else { acc }
        });
        peaks.push(peak.clamp(-1.0, 1.0));
    }

    // ------------------------------------------------------------------
    // Build a boolean pixel grid (px_height × width).
    // Row 0 = top.  Centre pixel row = height (using `height * 2` rows).
    // ------------------------------------------------------------------
    let px_height = height * 2;
    let centre    = height; // zero-line pixel row

    let mut pixels = vec![vec![false; width]; px_height];

    // Always draw the zero line so the oscilloscope reference is visible.
    for col in 0..width {
        pixels[centre.min(px_height - 1)][col] = true;
    }

    // Fill from zero-line to the peak, creating solid bars around centre.
    for (col, &v) in peaks.iter().enumerate() {
        let target = (centre as f32 - v * height as f32).round() as isize;
        let target = target.clamp(0, px_height as isize - 1) as usize;
        let (lo, hi) = if target <= centre {
            (target, centre)
        } else {
            (centre, target)
        };
        for row in lo..=hi {
            pixels[row][col] = true;
        }
    }

    // ------------------------------------------------------------------
    // Combine pixel pairs into half-block characters.
    // ------------------------------------------------------------------
    (0..height)
        .map(|row| {
            let top_px = row * 2;
            let bot_px = row * 2 + 1;
            (0..width)
                .map(|col| match (pixels[top_px][col], pixels[bot_px][col]) {
                    (true,  true)  => '█',
                    (true,  false) => '▀',
                    (false, true)  => '▄',
                    (false, false) => ' ',
                })
                .collect()
        })
        .collect()
}

/// A flat centre line rendered when nothing is playing.
fn flat_line(width: usize, height: usize) -> Vec<String> {
    let mid = height / 2;
    (0..height)
        .map(|r| {
            if r == mid { "─".repeat(width) } else { " ".repeat(width) }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Visualizer mode selector
// ---------------------------------------------------------------------------

/// Which visualizer is currently displayed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VizMode {
    Waveform,
    Spirograph,
    Fireworks,
}

impl VizMode {
    pub fn label(self) -> &'static str {
        match self {
            VizMode::Waveform   => "Waveform",
            VizMode::Spirograph => "Spirograph",
            VizMode::Fireworks  => "Fireworks",
        }
    }

    pub fn next(self) -> Self {
        match self {
            VizMode::Waveform   => VizMode::Spirograph,
            VizMode::Spirograph => VizMode::Fireworks,
            VizMode::Fireworks  => VizMode::Waveform,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            VizMode::Waveform   => VizMode::Fireworks,
            VizMode::Spirograph => VizMode::Waveform,
            VizMode::Fireworks  => VizMode::Spirograph,
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Compute RMS amplitude from the wave buffer (0.0 – 1.0).
pub fn rms_amplitude(buffer: &WaveBuffer) -> f32 {
    if let Ok(guard) = buffer.try_lock() {
        if guard.is_empty() {
            return 0.0;
        }
        let sum_sq: f32 = guard.iter().map(|s| s * s).sum();
        (sum_sq / guard.len() as f32).sqrt().clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Combine a boolean pixel grid into half-block character rows.
/// `pixels[row][col]` uses `px_height = height * 2` rows.
fn pixels_to_lines(pixels: &[Vec<bool>], width: usize, height: usize) -> Vec<String> {
    (0..height)
        .map(|row| {
            let top = row * 2;
            let bot = row * 2 + 1;
            (0..width)
                .map(|col| match (pixels[top][col], pixels[bot][col]) {
                    (true,  true)  => '█',
                    (true,  false) => '▀',
                    (false, true)  => '▄',
                    (false, false) => ' ',
                })
                .collect()
        })
        .collect()
}

/// Bresenham line into the pixel grid (bounds-checked).
fn draw_line(
    pixels: &mut [Vec<bool>],
    x0: isize, y0: isize,
    x1: isize, y1: isize,
    w: usize, h: usize,
) {
    let dx =  (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1isize } else { -1 };
    let sy = if y0 < y1 { 1isize } else { -1 };
    let mut err = dx + dy;
    let (mut x, mut y) = (x0, y0);
    loop {
        if x >= 0 && (x as usize) < w && y >= 0 && (y as usize) < h {
            pixels[y as usize][x as usize] = true;
        }
        if x == x1 && y == y1 { break; }
        let e2 = 2 * err;
        if e2 >= dy { err += dy; x += sx; }
        if e2 <= dx { err += dx; y += sy; }
    }
}

// ---------------------------------------------------------------------------
// Spirograph renderer
// ---------------------------------------------------------------------------

/// Render a hypotrochoid (spirograph) into a `width × height` character grid.
///
/// The curve is:
///   x(t) = (R−r)·cos(t)  +  d·cos((R−r)/r · t)
///   y(t) = (R−r)·sin(t)  −  d·sin((R−r)/r · t)
///
/// `phase` advances each UI tick to slowly rotate the pattern.
/// The RMS amplitude of the current audio modulates `d`, making the
/// spirograph "bloom" louder passages.
pub fn render_spirograph(
    buffer:  &WaveBuffer,
    width:   usize,
    height:  usize,
    phase:   f64,
) -> Vec<String> {
    if width == 0 || height == 0 {
        return Vec::new();
    }

    let amplitude = rms_amplitude(buffer) as f64;

    let px_h = height * 2;
    let cx   = width  as f64 / 2.0;
    let cy   = px_h   as f64 / 2.0;

    // Fit the curve inside the pixel grid with a small margin.
    // Half-block pixels are approximately square, so no aspect correction needed.
    let max_radius = cx.min(cy) * 0.88;

    // Hypotrochoid parameters (R=5, r=2 → 5-petal closed curve, period = 5×2π).
    const BIG_R: f64 = 5.0;
    const LIL_R: f64 = 2.0;
    let base_d: f64  = 3.5;
    let d: f64       = base_d + amplitude * 1.8; // louder → petals extend further

    // Normalise so max extent (= (R-r) + d) fits within max_radius.
    let max_extent = (BIG_R - LIL_R) + d;
    let scale      = max_radius / max_extent;

    // Number of steps: enough that consecutive points are < 1 px apart.
    let circumference = 2.0 * std::f64::consts::PI * (BIG_R - LIL_R + d) * scale;
    let steps = ((circumference * 6.0) as usize).max(4_000);

    let t_max = BIG_R * 2.0 * std::f64::consts::PI; // full closed curve period

    let mut pixels = vec![vec![false; width]; px_h];

    // Compute curve points and connect with Bresenham lines.
    let mut prev_px: Option<(isize, isize)> = None;
    for i in 0..=steps {
        let t = i as f64 / steps as f64 * t_max;
        let ratio = (BIG_R - LIL_R) / LIL_R;

        let x = ((BIG_R - LIL_R) * (t + phase).cos()
                + d * (ratio * t + phase * (ratio + 1.0)).cos()) * scale;
        let y = ((BIG_R - LIL_R) * (t + phase).sin()
                - d * (ratio * t + phase * (ratio + 1.0)).sin()) * scale;

        let px = (cx + x).round() as isize;
        let py = (cy + y).round() as isize;

        if let Some((ppx, ppy)) = prev_px {
            draw_line(&mut pixels, ppx, ppy, px, py, width, px_h);
        }
        prev_px = Some((px, py));
    }

    pixels_to_lines(&pixels, width, height)
}

// ---------------------------------------------------------------------------
// Firework visualizer
// ---------------------------------------------------------------------------
// Firework visualizer
// ---------------------------------------------------------------------------

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

const GRAVITY:     f32   = 0.0012;   // normalised units per tick²
const MAX_PARTS:   usize = 700;
const MAX_ROCKETS: usize = 8;
const SILENCE_FLOOR: f32 = 0.004;   // RMS below this → no launches

// Colour palettes for burst variety
const WARM: &[Color] = &[
    Color::Red, Color::LightRed, Color::Yellow, Color::LightYellow,
];
const COOL: &[Color] = &[
    Color::Blue, Color::LightBlue, Color::Cyan, Color::LightCyan,
];
const ALL_COLORS: &[Color] = &[
    Color::Red,    Color::Yellow,      Color::Green,
    Color::Cyan,   Color::Blue,        Color::Magenta,
    Color::LightRed, Color::LightYellow, Color::LightGreen,
    Color::LightCyan, Color::LightBlue, Color::LightMagenta,
];
// Complementary pairs — (primary, accent) for two-tone bursts
const COMPLEMENTARY: &[(Color, Color)] = &[
    (Color::Red,         Color::Cyan),
    (Color::Yellow,      Color::Blue),
    (Color::Green,       Color::Magenta),
    (Color::LightRed,    Color::LightCyan),
    (Color::LightYellow, Color::LightBlue),
    (Color::LightGreen,  Color::LightMagenta),
];

struct Particle {
    x: f32, y: f32,
    vx: f32, vy: f32,
    life: f32,
    gravity_mult: f32,   // per-particle gravity scale for floaty variety
    color: Color,
}

struct Rocket {
    x: f32, y: f32,
    vy: f32,
    apex: f32,
    color: Color,
    color2: Option<Color>,  // second colour for two-tone bursts
    strength: f32,          // hit strength at launch time
}

/// Self-contained firework particle simulation driven by bass onset detection.
///
/// Call [`FireworkState::update`] with the live wave buffer each tick, then
/// [`FireworkState::render`] to get coloured ratatui [`Line`]s.
///
/// Rockets only launch on bass hits; the screen goes dark in silence.
pub struct FireworkState {
    particles:    Vec<Particle>,
    rockets:      Vec<Rocket>,
    rng:          u64,
    // Audio analysis state (persists between ticks)
    lpf:          f32,   // IIR low-pass filter accumulator (~420 Hz cutoff @ 44.1 kHz)
    bass_fast:    f32,   // short-term bass EMA  (~2-tick window)
    bass_slow:    f32,   // long-term bass EMA   (~33-tick window, used as baseline)
    rms_long:     f32,   // overall loudness EMA for silence detection
    hit_cooldown: u32,   // ticks to suppress re-triggering after a launch
    was_paused:   bool,  // true while paused; triggers analysis reset on first resume tick
}

impl FireworkState {
    pub fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x517cc1b727220a95);
        Self {
            particles:    Vec::new(),
            rockets:      Vec::new(),
            rng:          seed ^ 0xa5a5a5a5a5a5a5a5,
            lpf:          0.0,
            bass_fast:    0.0,
            bass_slow:    0.001,  // small non-zero avoids div-by-zero on first tick
            rms_long:     0.0,
            hit_cooldown: 0,
            was_paused:   false,
        }
    }

    fn rand(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13; x ^= x >> 7; x ^= x << 17;
        self.rng = x; x
    }
    fn randf(&mut self) -> f32 { (self.rand() & 0xFFFF) as f32 / 65536.0 }

    // ── Public API ───────────────────────────────────────────────────────────

    /// Advance the simulation one tick.
    ///
    /// Pass `is_playing = false` when the player is paused or stopped: physics
    /// still advances (particles coast to a stop) but audio analysis is skipped
    /// so stale buffer data does not pre-converge the onset filters.  On the
    /// first tick after resuming the filters reset, guaranteeing a rocket burst
    /// as soon as the bass comes back.
    pub fn update(&mut self, buffer: &WaveBuffer, is_playing: bool) {
        // Physics runs every tick regardless of playback state.
        self.tick_physics();

        if !is_playing {
            self.was_paused = true;
            return;
        }

        let samples: Vec<f32> = match buffer.try_lock() {
            Ok(g)  => g.iter().copied().collect(),
            Err(_) => return,
        };

        // After a pause the EMA filters have stale values; reset so the onset
        // detector re-calibrates on the very first fresh frame.
        if self.was_paused {
            self.lpf       = 0.0;
            self.bass_fast = 0.0;
            self.bass_slow = 0.001;
            self.was_paused = false;
        }

        if samples.is_empty() {
            self.rms_long *= 0.97;
            return;
        }

        // ── Overall loudness EMA — silence gate ───────────────────────────
        let overall_sq: f32 = samples.iter().map(|s| s * s).sum::<f32>()
            / samples.len() as f32;
        let rms = overall_sq.sqrt();
        self.rms_long = self.rms_long * 0.97 + rms * 0.03;

        if self.rms_long < SILENCE_FLOOR {
            return; // silence — no launches
        }

        // ── IIR low-pass (α ≈ 0.06 → ~420 Hz cutoff @ 44.1 kHz) ─────────
        let alpha = 0.06_f32;
        let mut bass_sq = 0.0f32;
        for &s in &samples {
            self.lpf = self.lpf * (1.0 - alpha) + s * alpha;
            bass_sq += self.lpf * self.lpf;
        }
        let bass_rms = (bass_sq / samples.len() as f32).sqrt();

        // α=0.60 → ~2-tick response window; at 100 ms/tick this catches a
        // single kick drum in the first tick rather than needing 4+ ticks.
        self.bass_fast = self.bass_fast * 0.40 + bass_rms * 0.60; // ~2 ticks
        self.bass_slow = self.bass_slow * 0.97 + bass_rms * 0.03; // ~33 ticks

        // ── Beat/onset detection ──────────────────────────────────────────
        self.hit_cooldown = self.hit_cooldown.saturating_sub(1);

        // Ratio of short-term to long-term bass energy.
        // Threshold 1.20 (was 1.35) lets moderate kicks trigger rockets within
        // a single 100 ms tick instead of requiring 4+ ticks of sustained bass.
        let ratio = self.bass_fast / self.bass_slow.max(0.0001);

        if ratio > 1.20 && self.hit_cooldown == 0 && self.rockets.len() < MAX_ROCKETS {
            // Normalise strength: 0.0 = barely above threshold, 1.0 = very hard hit
            let strength = ((ratio - 1.20) / 0.80).clamp(0.0, 1.0);
            self.launch_on_hit(strength);
            self.hit_cooldown = (7.0 + strength * 8.0) as u32;
        }
    }

    /// Render the current frame into coloured ratatui [`Line`]s.
    pub fn render(&self, width: usize, height: usize) -> Vec<Line<'static>> {
        if width == 0 || height == 0 { return Vec::new(); }

        let px_h = height * 2;
        let mut grid: Vec<Vec<Option<Color>>> = vec![vec![None; width]; px_h];

        let mut plot = |nx: f32, ny: f32, color: Color| {
            let px = (nx * width as f32).round() as isize;
            let py = (ny * px_h  as f32).round() as isize;
            if px >= 0 && (px as usize) < width && py >= 0 && (py as usize) < px_h {
                grid[py as usize][px as usize] = Some(color);
            }
        };

        // Rockets: bright white tip, colour-tinted trail (previews the burst colour)
        for r in &self.rockets {
            plot(r.x, r.y,          Color::White);
            plot(r.x, r.y + 0.010,  r.color);
            plot(r.x, r.y + 0.020,  Color::DarkGray);
        }

        // Particles: bright (light variant) when fresh, base colour as they age
        for p in &self.particles {
            if p.life < 0.13 { continue; }
            let color = if p.life > 0.55 { lighten(p.color) } else { p.color };
            plot(p.x, p.y, color);
        }

        // Combine pixel pairs into styled spans (run-length encoded for efficiency)
        (0..height).map(|row| {
            let top = row * 2;
            let bot = row * 2 + 1;
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut run_color: Option<Color>  = None;
            let mut run = String::new();

            for col in 0..width {
                let tc = grid[top][col];
                let bc = grid[bot][col];
                let (ch, color) = match (tc, bc) {
                    (Some(c), Some(_)) => ('█', Some(c)),
                    (Some(c), None)    => ('▀', Some(c)),
                    (None,    Some(c)) => ('▄', Some(c)),
                    (None,    None)    => (' ', None),
                };
                if color == run_color {
                    run.push(ch);
                } else {
                    if !run.is_empty() {
                        spans.push(color_span(run.clone(), run_color));
                    }
                    run.clear();
                    run.push(ch);
                    run_color = color;
                }
            }
            if !run.is_empty() {
                spans.push(color_span(run, run_color));
            }
            Line::from(spans)
        }).collect()
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    /// Decide how many rockets to launch and with what variety, then push them.
    fn launch_on_hit(&mut self, strength: f32) {
        // Hard hits fire 2–3 simultaneous rockets; soft hits just one.
        let count = match strength {
            s if s > 0.75 => 3,
            s if s > 0.40 => 2,
            _              => 1,
        };

        for i in 0..count {
            // Spread rockets horizontally; single-rocket hits are fully random.
            let x = if count == 1 {
                self.randf()
            } else {
                (i as f32 + 0.15 + self.randf() * 0.7) / count as f32
            };

            // Apex height varies with strength: hard hits reach higher.
            let apex_min  = 0.05 + (1.0 - strength) * 0.25;
            let apex_max  = apex_min + 0.35;
            let apex      = apex_min + self.randf() * (apex_max - apex_min);
            let dist      = 1.0 - apex;
            let vy        = -(2.0 * GRAVITY * dist).sqrt();

            let (color, color2) = self.pick_color_scheme(strength);
            self.rockets.push(Rocket { x, y: 1.05, vy, apex, color, color2, strength });
        }
    }

    /// Choose a burst colour scheme based on hit strength and RNG.
    fn pick_color_scheme(&mut self, strength: f32) -> (Color, Option<Color>) {
        match self.rand() % 5 {
            0 => {  // warm single
                let c = WARM[(self.rand() as usize) % WARM.len()];
                (c, None)
            }
            1 => {  // cool single
                let c = COOL[(self.rand() as usize) % COOL.len()];
                (c, None)
            }
            2 if strength > 0.3 => {  // complementary two-tone
                let p = &COMPLEMENTARY[(self.rand() as usize) % COMPLEMENTARY.len()];
                (p.0, Some(p.1))
            }
            3 if strength > 0.6 => {  // warm + cool mix (strong hits only)
                let w = WARM[(self.rand() as usize) % WARM.len()];
                let c = COOL[(self.rand() as usize) % COOL.len()];
                (w, Some(c))
            }
            _ => {  // any colour
                let c = ALL_COLORS[(self.rand() as usize) % ALL_COLORS.len()];
                (c, None)
            }
        }
    }

    /// Advance all rockets and particles by one physics tick.
    fn tick_physics(&mut self) {
        // ── Rockets ───────────────────────────────────────────────────────
        let mut bursts: Vec<(f32, f32, Color, Option<Color>, f32)> = Vec::new();
        let mut live: Vec<Rocket> = Vec::new();
        for mut r in self.rockets.drain(..) {
            r.y  += r.vy;
            r.vy += GRAVITY;
            if r.y <= r.apex || r.vy >= 0.0 {
                bursts.push((r.x, r.y.clamp(0.0, 1.0), r.color, r.color2, r.strength));
            } else if r.y < 1.1 {
                live.push(r);
            }
        }
        self.rockets = live;

        for (bx, by, color, color2, strength) in bursts {
            self.spawn_burst(bx, by, color, color2, strength);
        }

        // ── Particles ─────────────────────────────────────────────────────
        let mut live: Vec<Particle> = Vec::new();
        for mut p in self.particles.drain(..) {
            p.vy += GRAVITY * p.gravity_mult;
            p.x  += p.vx;
            p.y  += p.vy;
            p.life -= 0.016;
            if p.life > 0.0 && p.x >= 0.0 && p.x <= 1.0
                             && p.y >= 0.0 && p.y <= 1.08 {
                live.push(p);
            }
        }
        self.particles = live;
    }

    /// Spawn a burst of particles with randomised shape, speed, and gravity.
    fn spawn_burst(
        &mut self,
        bx: f32, by: f32,
        color: Color, color2: Option<Color>,
        strength: f32,
    ) {
        // Particle count: 25 (soft) → 100 (hard hit)
        let count = (25.0 + strength * 75.0) as usize;

        // Make room in the pool
        let need = count + 25;
        if self.particles.len() + need > MAX_PARTS {
            let excess = self.particles.len() + need - MAX_PARTS;
            self.particles.drain(..excess.min(self.particles.len()));
        }

        let base_speed  = 0.008 + strength * 0.015;
        // Per-burst gravity multiplier creates floaty vs. heavy explosions
        let g_burst     = 0.5 + self.randf() * 1.0;

        // Four burst shapes, chosen randomly each explosion
        let shape: u8 = (self.rand() % 4) as u8;

        use std::f32::consts::{FRAC_PI_2, PI, TAU};

        for i in 0..count {
            let frac = i as f32 / count as f32;

            let (vx, vy) = match shape {
                // ── Radial: uniform sphere with random speed variance ──────
                0 => {
                    let angle = frac * TAU + self.randf() * 0.5;
                    let speed = base_speed * (0.4 + self.randf() * 0.6);
                    (angle.cos() * speed, angle.sin() * speed * 0.5)
                }
                // ── Ring: narrow speed band → hollow halo effect ──────────
                1 => {
                    let angle = frac * TAU + self.randf() * 0.08;
                    let speed = base_speed * (0.92 + self.randf() * 0.16);
                    (angle.cos() * speed, angle.sin() * speed * 0.5)
                }
                // ── Fountain: biased strongly upward, fans out ────────────
                2 => {
                    let angle = -FRAC_PI_2 + (self.randf() - 0.5) * PI * 0.8;
                    let speed = base_speed * (0.5 + self.randf() * 0.9);
                    (angle.cos() * speed, angle.sin() * speed * 0.5)
                }
                // ── Starburst: 5 or 7 arms with inter-arm scatter ─────────
                _ => {
                    let arms      = if self.rand() & 1 == 0 { 5usize } else { 7 };
                    let arm_angle = (i % arms) as f32 / arms as f32 * TAU;
                    let jitter    = (self.randf() - 0.5) * 0.3;
                    let angle     = arm_angle + jitter;
                    // Most particles cluster on the arm tips; a few fill the gaps
                    let on_arm    = i % 3 != 2;
                    let speed     = if on_arm { base_speed * (0.85 + self.randf() * 0.3) }
                                   else       { base_speed * self.randf() * 0.45 };
                    (angle.cos() * speed, angle.sin() * speed * 0.5)
                }
            };

            // Alternate colours for two-tone bursts
            let c = match color2 {
                Some(c2) if i % 2 == 1 => c2,
                _                       => color,
            };

            // Per-particle gravity variation (±30% around burst baseline)
            let g_p = g_burst * (0.7 + self.randf() * 0.6);

            let life = 0.82 + self.randf() * 0.18;
            self.particles.push(Particle {
                x: bx, y: by, vx, vy,
                life,
                gravity_mult: g_p,
                color: c,
            });
        }

        // ── Glitter sparks on medium–hard hits ────────────────────────────
        // Short-lived, slow-moving, white — they shimmer in the explosion cloud.
        if strength > 0.35 {
            let sparks = (strength * 22.0) as usize;
            use std::f32::consts::TAU;
            for _ in 0..sparks {
                let angle = self.randf() * TAU;
                let speed = base_speed * 0.22;
                let life = 0.20 + self.randf() * 0.25;
                self.particles.push(Particle {
                    x: bx, y: by,
                    vx: angle.cos() * speed,
                    vy: angle.sin() * speed * 0.25 - 0.002,
                    life,
                    gravity_mult: 0.20,
                    color:        Color::White,
                });
            }
        }
    }
}

impl Default for FireworkState {
    fn default() -> Self { Self::new() }
}

fn lighten(c: Color) -> Color {
    match c {
        Color::Red     => Color::LightRed,
        Color::Green   => Color::LightGreen,
        Color::Yellow  => Color::LightYellow,
        Color::Blue    => Color::LightBlue,
        Color::Magenta => Color::LightMagenta,
        Color::Cyan    => Color::LightCyan,
        other          => other,
    }
}

fn color_span(text: String, color: Option<Color>) -> Span<'static> {
    match color {
        Some(c) => Span::styled(text, Style::default().fg(c)),
        None    => Span::raw(text),
    }
}

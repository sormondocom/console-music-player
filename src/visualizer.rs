//! Real-time waveform capture and terminal rendering.
//!
//! [`SampleCapture`] is a thin `rodio::Source` wrapper that tees every `f32`
//! sample into a shared ring buffer without blocking the audio thread.  The
//! buffer is read each UI tick by [`render_waveform`], which produces a
//! character-art oscilloscope display using Unicode half-block characters
//! (`▀` `▄` `█` ` `) to achieve double the terminal's native vertical
//! resolution.

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

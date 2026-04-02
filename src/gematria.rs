//! Gematria-based track selection.
//!
//! Derived from the cosmic-knowledge project's numerology module.
//! The user types a word or phrase; the value is computed across multiple
//! traditional systems and used as an index into the current library to
//! select a track to play.
//!
//! Systems included:
//!   Hebrew Gematria (Mispar Hechrachi)
//!   Pythagorean (Western cyclical)
//!   Chaldean (Babylonian vibrational)
//!   Simple Ordinal (A=1 … Z=26)

// ---------------------------------------------------------------------------
// Letter tables
// ---------------------------------------------------------------------------

fn hebrew(c: char) -> Option<u32> {
    match c.to_ascii_uppercase() {
        'A' => Some(1),   'B' => Some(2),   'C' => Some(3),
        'D' => Some(4),   'E' => Some(5),   'F' => Some(6),
        'G' => Some(7),   'H' => Some(8),   'I' => Some(9),
        'J' => Some(10),  'K' => Some(20),  'L' => Some(30),
        'M' => Some(40),  'N' => Some(50),  'O' => Some(60),
        'P' => Some(70),  'Q' => Some(80),  'R' => Some(100),
        'S' => Some(200), 'T' => Some(300), 'U' => Some(400),
        'V' => Some(500), 'W' => Some(600), 'X' => Some(700),
        'Y' => Some(800), 'Z' => Some(900),
        _ => None,
    }
}

fn pythagorean(c: char) -> Option<u32> {
    match c.to_ascii_uppercase() {
        'A' | 'J' | 'S' => Some(1),
        'B' | 'K' | 'T' => Some(2),
        'C' | 'L' | 'U' => Some(3),
        'D' | 'M' | 'V' => Some(4),
        'E' | 'N' | 'W' => Some(5),
        'F' | 'O' | 'X' => Some(6),
        'G' | 'P' | 'Y' => Some(7),
        'H' | 'Q' | 'Z' => Some(8),
        'I' | 'R'       => Some(9),
        _ => None,
    }
}

fn chaldean(c: char) -> Option<u32> {
    match c.to_ascii_uppercase() {
        'A' | 'I' | 'J' | 'Q' | 'Y' => Some(1),
        'B' | 'K' | 'R'             => Some(2),
        'C' | 'G' | 'L' | 'S'      => Some(3),
        'D' | 'M' | 'T'             => Some(4),
        'E' | 'H' | 'N' | 'X'      => Some(5),
        'U' | 'V' | 'W'             => Some(6),
        'O' | 'Z'                   => Some(7),
        'F' | 'P'                   => Some(8),
        _ => None,
    }
}

fn simple_ordinal(c: char) -> Option<u32> {
    let u = c.to_ascii_uppercase();
    if u.is_ascii_uppercase() {
        Some((u as u32) - ('A' as u32) + 1)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Core computation
// ---------------------------------------------------------------------------

/// Reduce `n` to a single digit (1–9) by summing digits repeatedly.
/// Preserves master numbers 11, 22, and 33.
pub fn digital_root(mut n: u32) -> u32 {
    loop {
        if n <= 9 || n == 11 || n == 22 || n == 33 {
            return n;
        }
        n = n.to_string().chars().filter_map(|c| c.to_digit(10)).sum();
    }
}

/// A single system's result.
#[derive(Clone, Debug)]
pub struct SystemResult {
    pub name: &'static str,
    pub total: u32,
    pub root: u32,
}

/// Compute all four systems for `phrase` (case-insensitive, spaces/punctuation ignored).
pub fn compute(phrase: &str) -> Vec<SystemResult> {
    let systems: &[(&'static str, fn(char) -> Option<u32>)] = &[
        ("Hebrew Gematria",  hebrew),
        ("Pythagorean",      pythagorean),
        ("Chaldean",         chaldean),
        ("Simple Ordinal",   simple_ordinal),
    ];

    systems
        .iter()
        .map(|(name, f)| {
            let total: u32 = phrase.chars().filter_map(|c| f(c)).sum();
            SystemResult { name, total, root: digital_root(total) }
        })
        .collect()
}

/// Select a 0-based track index from `track_count` using the given system's total.
///
/// Uses `total % track_count` so the full (pre-reduced) value spreads across
/// the whole library — digital root alone would cluster everything in 1–9.
pub fn select_index(total: u32, track_count: usize) -> usize {
    if track_count == 0 {
        return 0;
    }
    (total as usize) % track_count
}

// ---------------------------------------------------------------------------
// Interpretive text (from cosmic-knowledge numerology module)
// ---------------------------------------------------------------------------

pub fn meaning_of(root: u32) -> &'static str {
    match root {
        1  => "New beginnings, leadership, independence, manifestation",
        2  => "Balance, cooperation, divine partnerships, faith",
        3  => "Creativity, self-expression, joy",
        4  => "Stability, hard work, building solid foundations",
        5  => "Freedom, positive change, transformation",
        6  => "Love, family, nurturing, responsibility",
        7  => "Spiritual awakening, inner wisdom, mystical knowledge",
        8  => "Material abundance, karma, personal power",
        9  => "Universal love, spiritual completion, humanitarian service",
        11 => "MASTER 11 — Spiritual messenger, intuitive insight",
        22 => "MASTER 22 — Master builder, turning dreams into reality",
        33 => "MASTER 33 — Master teacher, divine love in action",
        _  => "Beyond the veil of ordinary understanding",
    }
}

//! CDP (Chrome DevTools Protocol) automation for Amazon Music download trigger.
//!
//! Only the Win32 installer version of Amazon Music supports launching with
//! `--remote-debugging-port`. The UWP / Store version cannot accept CLI args
//! through normal means, so this module gates itself to Win32 installs.
//!
//! Flow:
//!   1. Check whether Amazon Music is already running without the debug flag.
//!   2. Try to connect to localhost:9222 first (user may have pre-launched).
//!   3. Launch the exe with --remote-debugging-port=9222 if not reachable.
//!   4. Poll until the Amazon Music page appears in /json/list.
//!   5. Connect via WebSocket CDP, run a DOM probe, report what was found.
//!   6. Batch download loop: click 5 per-track Download buttons, sleep, scroll, repeat.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};


// ---------------------------------------------------------------------------
// Tuning constants
// ---------------------------------------------------------------------------

/// How many consecutive scrolls with nothing to click before we give up.
const EMPTY_SCROLL_LIMIT: u32 = 6;

/// How long to poll for the track list to appear after navigation (ms).
const PAGE_WAIT_MS: u64 = 5_000;

// ---------------------------------------------------------------------------
// Public message type
// ---------------------------------------------------------------------------

pub enum CdpMsg {
    /// Informational log line for the TUI.
    Log(String),
    /// Automation completed (success or non-fatal).
    Done,
    /// Fatal error — automation aborted.
    Error(String),
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Spawn the download automation as a background tokio task.
pub fn spawn_download_automation(exe: PathBuf) -> mpsc::Receiver<CdpMsg> {
    let (tx, rx) = mpsc::channel(256);
    tokio::spawn(run(exe, tx));
    rx
}

// ---------------------------------------------------------------------------
// Main async runner
// ---------------------------------------------------------------------------

async fn run(exe: PathBuf, tx: mpsc::Sender<CdpMsg>) {
    macro_rules! log {
        ($($arg:tt)*) => {{ let _ = tx.send(CdpMsg::Log(format!($($arg)*))).await; }};
    }
    macro_rules! bail {
        ($($arg:tt)*) => {{
            let _ = tx.send(CdpMsg::Error(format!($($arg)*))).await;
            return;
        }};
    }

    // ── Step 1: reuse existing session or launch fresh ─────────────────────
    log!("Checking for existing debug endpoint on localhost:9222...");
    let ws_url = if let Some(url) = probe_debug_endpoint().await {
        log!("Found existing debug session — reusing.");
        url
    } else {
        if is_amazon_music_running() {
            log!("⚠  Amazon Music is already running (no debug port).");
            log!("   Close it, then press [D] again to re-launch with debugging.");
            let _ = tx.send(CdpMsg::Done).await;
            return;
        }

        log!("Launching Amazon Music with --remote-debugging-port=9222...");
        {
            use std::os::windows::process::CommandExt;
            if let Err(e) = std::process::Command::new(&exe)
                .arg("--remote-debugging-port=9222")
                .arg("--remote-allow-origins=*")
                .creation_flags(0x00000008)
                .spawn()
            {
                bail!("Failed to launch Amazon Music: {e}");
            }
        }

        log!("Waiting for app to start...");
        match wait_for_page(&tx).await {
            Some(url) => url,
            None => bail!(
                "Timed out waiting for debug endpoint. The app may not support \
                 --remote-debugging-port, or it reused an existing window without the flag."
            ),
        }
    };

    // ── Step 2: WebSocket connect ───────────────────────────────────────────
    log!("Connecting to debug interface...");
    let (ws_stream, _) = match connect_async(&ws_url).await {
        Ok(x) => x,
        Err(e) => bail!("WebSocket connect failed: {e}"),
    };
    let (mut write, mut read) = ws_stream.split();

    let mut next_id: u64 = 1;

    // Enable Runtime domain
    cdp_send(&mut write, next_id, "Runtime.enable", json!({})).await;
    drain_until_id(&mut read, next_id).await;

    // Give the app a moment to fully load before touching the UI
    tokio::time::sleep(Duration::from_secs(5)).await;
    log!("Connected. Probing Amazon Music UI...");

    // ── Step 3: DOM probe — understand what we're looking at ───────────────
    next_id += 1;
    let probe_id = next_id;
    cdp_send(&mut write, probe_id, "Runtime.evaluate", json!({
        "expression": PROBE_JS,
        "returnByValue": true,
    })).await;

    if let Some(val) = read_response(&mut read, probe_id).await {
        for line in extract_string_result(&val).lines() {
            log!("{line}");
        }
    }
    log!("────────────────────────────────────────");

    // ── Step 4: navigate Library → Music → Songs → Reset → Purchased ─────────
    macro_rules! nav_step {
        ($label:expr, $js:expr) => {{
            log!("Clicking {}...", $label);
            next_id += 1;
            let step_id = next_id;
            cdp_send(&mut write, step_id, "Runtime.evaluate", json!({
                "expression": $js,
                "returnByValue": true,
            })).await;
            if let Some(val) = read_response(&mut read, step_id).await {
                log!("{}", extract_string_result(&val));
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }};
    }

    nav_step!("Library",   NAV_LIBRARY_JS);
    nav_step!("Music",     NAV_MUSIC_JS);
    nav_step!("Songs",     NAV_SONGS_JS);
    nav_step!("Reset",     NAV_RESET_JS);
    nav_step!("Purchased", NAV_PURCHASED_JS);

    // Poll until track rows appear (or timeout)
    log!("Waiting for Purchased list to load...");
    next_id += 1;
    let wait_id = next_id;
    let wait_js = format!("({})({})", WAIT_FOR_TRACKS_FN, PAGE_WAIT_MS);
    cdp_send(&mut write, wait_id, "Runtime.evaluate", json!({
        "expression": wait_js,
        "returnByValue": true,
        "awaitPromise": true,
    })).await;
    if let Some(val) = read_response(&mut read, wait_id).await {
        log!("{}", extract_string_result(&val));
    }
    log!("────────────────────────────────────────");

    // ── Step 5: per-track download loop ────────────────────────────────────
    // Session tracking — just an in-memory list for this run, not persisted.
    let mut session: Vec<String> = Vec::new();
    let mut consecutive_empty: u32 = 0;

    log!("Starting downloads...");
    log!("────────────────────────────────────────");

    loop {
        next_id += 1;
        let cid = next_id;
        cdp_send(&mut write, cid, "Runtime.evaluate", json!({
            "expression": CLICK_ONE_JS,
            "returnByValue": true,
            "awaitPromise": true,
        })).await;

        let result = match read_response_timeout(&mut read, cid, 20).await {
            Some(v) => v,
            None => {
                log!("⚠  No response from page — connection may have dropped.");
                break;
            }
        };

        let result_str = extract_string_result(&result);
        let stats: Value = match serde_json::from_str(&result_str) {
            Ok(v) => v,
            Err(_) => {
                log!("JS raw: {result_str}");
                json!({"clicked": false, "track": "", "at_end": false, "error": ""})
            }
        };

        let clicked  = stats["clicked"].as_bool().unwrap_or(false);
        let track    = stats["track"].as_str().unwrap_or("").to_string();
        let at_end   = stats["at_end"].as_bool().unwrap_or(false);
        let js_error = stats["error"].as_str().unwrap_or("").to_string();

        if !js_error.is_empty() {
            log!("  dbg: {js_error}");
        }

        if clicked {
            let n = session.len() + 1;
            let entry = format!("[{n}] {track}");
            log!("{entry}");
            session.push(entry);
            consecutive_empty = 0;
        } else if at_end {
            log!("────────────────────────────────────────");
            log!("End of list reached. {} downloaded this session.", session.len());
            log!("When downloads finish, press [S] to add the folder as a library source.");
            break;
        } else {
            consecutive_empty += 1;
            log!("scroll ({consecutive_empty}/{EMPTY_SCROLL_LIMIT})");
            if consecutive_empty >= EMPTY_SCROLL_LIMIT {
                log!("────────────────────────────────────────");
                log!("{} downloaded this session. Nothing left to find.", session.len());
                break;
            }
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let _ = tx.send(CdpMsg::Done).await;
}

// ---------------------------------------------------------------------------
// JS: DOM probe (runs once, returns a multi-line diagnostic string)
// ---------------------------------------------------------------------------

const PROBE_JS: &str = r#"(function() {
    var lines = [];
    lines.push('URL: ' + location.href);
    lines.push('Title: ' + document.title);

    var globals = ['amznMusic', 'DMusicApp', 'catalog', 'store', 'Redux']
        .filter(function(k) { return typeof window[k] !== 'undefined'; });
    if (globals.length) lines.push('Globals: ' + globals.join(', '));

    // Inventory all download-related buttons
    var all = Array.from(document.querySelectorAll(
        'button, [role="button"], [data-testid]'
    ));
    var dlEls = all.filter(function(el) {
        var t = (el.textContent || '').trim().toLowerCase();
        var a = (el.getAttribute('aria-label') || '').toLowerCase();
        var d = (el.getAttribute('data-testid') || '').toLowerCase();
        return t.indexOf('download') !== -1
            || a.indexOf('download') !== -1
            || d.indexOf('download') !== -1;
    });
    lines.push('Download-related elements: ' + dlEls.length);
    dlEls.slice(0, 10).forEach(function(el, i) {
        var text = (el.textContent || el.getAttribute('aria-label') || '')
            .trim().replace(/\s+/g, ' ').slice(0, 60);
        var tid = el.getAttribute('data-testid') || '';
        var cls = (el.className || '').slice(0, 40);
        lines.push('  [' + i + '] <' + el.tagName.toLowerCase() + '>'
            + (tid  ? ' tid="' + tid + '"' : '')
            + (cls  ? ' cls="' + cls + '"' : '')
            + ' "' + text + '"');
    });

    // Also count list rows to estimate how many tracks are rendered
    var rows = document.querySelectorAll('[role="row"], [role="listitem"], li');
    lines.push('Rendered rows/items: ' + rows.length);

    return lines.join('\n');
})()"#;

// ---------------------------------------------------------------------------
// JS: wait for track rows to appear in the DOM (returns a status string).
// Called as an async IIFE: (WAIT_FOR_TRACKS_FN)(timeoutMs)
// ---------------------------------------------------------------------------

const WAIT_FOR_TRACKS_FN: &str = r#"async function(timeoutMs) {
    var deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
        var rows = document.querySelectorAll('[role="row"], [role="listitem"], li');
        // Look for rows that contain any text (i.e. actual track rows, not headers)
        var trackRows = Array.from(rows).filter(function(r) {
            return r.textContent.trim().length > 10;
        });
        if (trackRows.length > 0) {
            return 'Track list ready (' + trackRows.length + ' rows visible).';
        }
        await new Promise(function(r) { setTimeout(r, 200); });
    }
    return 'Timed out waiting for track rows — proceeding anyway.';
}"#;

// ---------------------------------------------------------------------------
// JS navigation helpers — each returns a short status string for the log.
// All use exact-text matching (after normalisation) so we never accidentally
// click the wrong element.
// ---------------------------------------------------------------------------

// Step 1: Library
const NAV_LIBRARY_JS: &str = r#"(function() {
    function cmpNorm(s) { return (s || '').trim().toLowerCase().replace(/\s+/g, ' '); }
    function cmpFind(label) {
        return Array.from(document.querySelectorAll(
            'a, button, [role="link"], [role="button"], [role="tab"], [role="menuitem"], [role="option"]'
        )).find(function(el) {
            return cmpNorm(el.textContent) === label || cmpNorm(el.getAttribute('aria-label') || '') === label;
        });
    }
    var el = cmpFind('library');
    if (el) { el.click(); return 'Clicked: Library'; }
    return 'Library not found. Visible: [' + Array.from(document.querySelectorAll(
        'a, button, [role="link"], [role="button"], [role="tab"], [role="menuitem"]'
    )).map(function(e) { return cmpNorm(e.textContent || e.getAttribute('aria-label') || ''); })
      .filter(function(s, i, a) { return s.length > 1 && s.length < 30 && a.indexOf(s) === i; })
      .slice(0, 20).join(', ') + ']';
})()"#;

// Step 2: Music (sub-section under Library)
const NAV_MUSIC_JS: &str = r#"(function() {
    function cmpNorm(s) { return (s || '').trim().toLowerCase().replace(/\s+/g, ' '); }
    function cmpFind(label) {
        return Array.from(document.querySelectorAll(
            'a, button, [role="link"], [role="button"], [role="tab"], [role="menuitem"], [role="option"]'
        )).find(function(el) {
            return cmpNorm(el.textContent) === label || cmpNorm(el.getAttribute('aria-label') || '') === label;
        });
    }
    var el = cmpFind('music');
    if (el) { el.click(); return 'Clicked: Music'; }
    return 'Music not found. Visible: [' + Array.from(document.querySelectorAll(
        'a, button, [role="link"], [role="button"], [role="tab"], [role="menuitem"]'
    )).map(function(e) { return cmpNorm(e.textContent || e.getAttribute('aria-label') || ''); })
      .filter(function(s, i, a) { return s.length > 1 && s.length < 30 && a.indexOf(s) === i; })
      .slice(0, 20).join(', ') + ']';
})()"#;

// Step 3: Songs
const NAV_SONGS_JS: &str = r#"(function() {
    function cmpNorm(s) { return (s || '').trim().toLowerCase().replace(/\s+/g, ' '); }
    function cmpFind(label) {
        return Array.from(document.querySelectorAll(
            'a, button, [role="link"], [role="button"], [role="tab"], [role="menuitem"], [role="option"]'
        )).find(function(el) {
            return cmpNorm(el.textContent) === label || cmpNorm(el.getAttribute('aria-label') || '') === label;
        });
    }
    var el = cmpFind('songs');
    if (el) { el.click(); return 'Clicked: Songs'; }
    return 'Songs not found. Visible: [' + Array.from(document.querySelectorAll(
        'a, button, [role="link"], [role="button"], [role="tab"], [role="menuitem"]'
    )).map(function(e) { return cmpNorm(e.textContent || e.getAttribute('aria-label') || ''); })
      .filter(function(s, i, a) { return s.length > 1 && s.length < 30 && a.indexOf(s) === i; })
      .slice(0, 20).join(', ') + ']';
})()"#;

// Step 4: Reset (clear active filter)
const NAV_RESET_JS: &str = r#"(function() {
    function cmpNorm(s) { return (s || '').trim().toLowerCase().replace(/\s+/g, ' '); }
    function cmpFind(label) {
        return Array.from(document.querySelectorAll(
            'a, button, [role="link"], [role="button"], [role="tab"], [role="menuitem"], [role="option"]'
        )).find(function(el) {
            return cmpNorm(el.textContent) === label || cmpNorm(el.getAttribute('aria-label') || '') === label;
        });
    }
    var el = cmpFind('reset');
    if (el) { el.click(); return 'Clicked: Reset'; }
    return 'Reset not found (filter may already be clear)';
})()"#;

// Step 5: Purchased filter chip
const NAV_PURCHASED_JS: &str = r#"(function() {
    function cmpNorm(s) { return (s || '').trim().toLowerCase().replace(/\s+/g, ' '); }
    function cmpFind(label) {
        return Array.from(document.querySelectorAll(
            'a, button, [role="link"], [role="button"], [role="tab"], [role="menuitem"], [role="option"]'
        )).find(function(el) {
            return cmpNorm(el.textContent) === label || cmpNorm(el.getAttribute('aria-label') || '') === label;
        });
    }
    var el = cmpFind('purchased');
    if (el) { el.click(); return 'Clicked: Purchased'; }
    return 'Purchased not found. Visible: [' + Array.from(document.querySelectorAll(
        'a, button, [role="link"], [role="button"], [role="tab"], [role="menuitem"]'
    )).map(function(e) { return cmpNorm(e.textContent || e.getAttribute('aria-label') || ''); })
      .filter(function(s, i, a) { return s.length > 1 && s.length < 30 && a.indexOf(s) === i; })
      .slice(0, 20).join(', ') + ']';
})()"#;

// ---------------------------------------------------------------------------
// JS: click exactly one undownloaded track, report its name, scroll when
// the current viewport is exhausted.
//
// Returns JSON: { clicked: bool, track: string, already_done: number,
//                 at_end: bool, error: string }
//
// "clicked" is true when a button was found and clicked this call.
// "at_end"  is true when scrolling can go no further and nothing was clicked.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// JS: inspect the first track row — dumps every element without clicking.
// ---------------------------------------------------------------------------

const INSPECT_ROW_JS: &str = r#"(async function() {
    var lines = [];
    lines.push('URL: ' + location.href);

    // ── Find the Neko Case row ────────────────────────────────────────────
    var target = null;
    var allEls = Array.from(document.querySelectorAll('*'));
    for (var i = 0; i < allEls.length; i++) {
        var el = allEls[i];
        if (el.children.length > 3) continue;
        var t = el.textContent.trim();
        if (t.indexOf('1,000') !== -1 || t.toLowerCase().indexOf('neko') !== -1) {
            var node = el;
            for (var d = 0; d < 15; d++) {
                if (!node.parentElement) break;
                node = node.parentElement;
                var role = (node.getAttribute('role') || '').toLowerCase();
                var tag  = node.tagName.toLowerCase();
                if (role === 'row' || role === 'listitem' || role === 'presentation'
                        || tag === 'li' || tag === 'tr') {
                    target = node;
                    break;
                }
            }
            if (target) break;
        }
    }

    if (!target) {
        lines.push('Could not find the Neko Case row — is Purchased filter active?');
        var rows = Array.from(document.querySelectorAll('[role="row"], [role="listitem"], li, tr'))
            .filter(function(r) { return r.textContent.trim().length > 5; });
        lines.push('Candidate rows visible: ' + rows.length);
        rows.slice(0, 6).forEach(function(r, i) {
            lines.push('  [' + i + '] ' + r.textContent.trim().replace(/\s+/g, ' ').slice(0, 80));
        });
        return lines.join('\n');
    }

    lines.push('Found row: "' + target.textContent.trim().replace(/\s+/g, ' ').slice(0, 80) + '"');

    // ── Find and log every button in the row ─────────────────────────────
    var btns = Array.from(target.querySelectorAll('button, [role="button"]'));
    lines.push('Buttons in row (' + btns.length + '):');
    btns.forEach(function(b, i) {
        lines.push('  [' + i + '] text="' + b.textContent.trim().replace(/\s+/g, ' ').slice(0, 40)
            + '" aria="' + (b.getAttribute('aria-label') || '')
            + '" cls="' + (b.className || '').toString().slice(0, 60) + '"');
    });

    // ── Click the last button (hamburger is typically rightmost) ─────────
    var hamburger = btns[btns.length - 1];
    if (!hamburger) {
        lines.push('No buttons found in row!');
        return lines.join('\n');
    }
    lines.push('Clicking last button: text="' + hamburger.textContent.trim()
        + '" aria="' + (hamburger.getAttribute('aria-label') || '') + '"');
    hamburger.click();

    // ── Wait up to 2s for a menu to appear ───────────────────────────────
    var menu = null;
    for (var w = 0; w < 20; w++) {
        await new Promise(function(r) { setTimeout(r, 100); });
        // Look for any newly-appeared overlay/menu element
        var candidates = Array.from(document.querySelectorAll(
            '[role="menu"], [role="listbox"], [role="dialog"]'
        ));
        // Also try any element that appeared with "Download" text
        var dlEl = Array.from(document.querySelectorAll('*')).find(function(el) {
            return el.children.length === 0
                && el.textContent.trim().toLowerCase() === 'download';
        });
        if (candidates.length > 0) { menu = candidates[0]; break; }
        if (dlEl) { menu = dlEl.parentElement; break; }
    }

    if (!menu) {
        lines.push('No menu appeared after clicking. Dumping body additions...');
        // Find any element containing "Download" text as a fallback diagnostic
        var anyDl = Array.from(document.querySelectorAll('*')).filter(function(el) {
            return el.children.length === 0
                && el.textContent.trim().toLowerCase().indexOf('download') !== -1;
        });
        lines.push('Elements with "download" text: ' + anyDl.length);
        anyDl.slice(0, 6).forEach(function(el) {
            lines.push('  <' + el.tagName.toLowerCase()
                + '> role="' + (el.getAttribute('role') || '')
                + '" cls="' + (el.className || '').toString().slice(0, 60)
                + '" text="' + el.textContent.trim() + '"');
        });
        return lines.join('\n');
    }

    // ── Menu found — log all items then click Download ────────────────────
    lines.push('Menu found: <' + menu.tagName.toLowerCase()
        + '> role="' + (menu.getAttribute('role') || '')
        + '" cls="' + (menu.className || '').toString().slice(0, 60) + '"');

    var items = Array.from(menu.querySelectorAll('*')).filter(function(el) {
        return el.children.length === 0 && el.textContent.trim().length > 0;
    });
    lines.push('Menu items (' + items.length + '):');
    var downloadEl = null;
    items.forEach(function(el, i) {
        var t = el.textContent.trim();
        lines.push('  [' + i + '] <' + el.tagName.toLowerCase()
            + '> role="' + (el.getAttribute('role') || '')
            + '" cls="' + (el.className || '').toString().slice(0, 60)
            + '" text="' + t + '"');
        if (!downloadEl && t.toLowerCase() === 'download') downloadEl = el;
    });

    if (downloadEl) {
        downloadEl.click();
        lines.push('SUCCESS: Clicked Download!');
    } else {
        lines.push('Download item not found in menu.');
    }

    return lines.join('\n');
})()"#;

// Per-track download.
//
// Strategy: Amazon Music's song list shows a row number (1, 2, 3…) as a leaf
// text node inside each track row.  These integers are unique to the track list
// — they never appear in nav links, filter chips, or headers — so they are the
// most reliable anchor we have for finding real song rows.
//
// Algorithm:
//   1. Collect every leaf node whose text is a plain integer ≥ 1.
//   2. Sort ascending so we always process in list order.
//   3. Walk up from the number node to the smallest ancestor that contains at
//      least one button (that ancestor is the track row).
//   4. Skip rows already tagged data-cmpProcessed.
//   5. On the first untagged row: mark it, click the last button (hamburger ⋮),
//      wait for the Download leaf, click it.
//   6. If all visible rows are processed: scroll the track list down and check
//      whether we've reached the bottom.
//
// Returns JSON: { clicked: bool, track: string, at_end: bool, error: string }
const CLICK_ONE_JS: &str = r#"(async function() {
    try {
        function sleep(ms) { return new Promise(function(r) { setTimeout(r, ms); }); }

        // ── 1. Locate numbered leaf nodes ─────────────────────────────────────
        var numEls = Array.from(document.querySelectorAll('*')).filter(function(el) {
            if (el.children.length > 0) return false;
            var t = el.textContent.trim();
            return /^\d+$/.test(t) && parseInt(t, 10) >= 1;
        });

        // Sort by numeric value so we walk the list in order
        numEls.sort(function(a, b) {
            return parseInt(a.textContent.trim(), 10) - parseInt(b.textContent.trim(), 10);
        });

        // ── 2. Walk up to find the row container (must contain a button) ──────
        function findRow(el) {
            var node = el.parentElement;
            for (var d = 0; d < 15 && node && node !== document.body; d++) {
                if (node.querySelectorAll('button, [role="button"]').length >= 1) {
                    return node;
                }
                node = node.parentElement;
            }
            return null;
        }

        // ── 3. Find first unprocessed row ─────────────────────────────────────
        var row = null;
        for (var i = 0; i < numEls.length; i++) {
            var candidate = findRow(numEls[i]);
            if (candidate && !candidate.dataset.cmpProcessed) {
                row = candidate;
                break;
            }
        }

        // ── 4. If nothing left, scroll and report ─────────────────────────────
        if (!row) {
            // Identify the scroller by walking up from the first number node
            var scroller = null;
            if (numEls.length > 0) {
                var sn = numEls[0].parentElement;
                for (var d = 0; d < 15 && sn && sn !== document.body; d++) {
                    var ov = window.getComputedStyle(sn).overflowY;
                    if ((ov === 'auto' || ov === 'scroll') && sn.scrollHeight > sn.clientHeight) {
                        scroller = sn; break;
                    }
                    sn = sn.parentElement;
                }
            }

            var atEnd = false;
            if (scroller) {
                var before = scroller.scrollTop;
                scroller.scrollBy({ top: Math.floor(scroller.clientHeight * 0.8), behavior: 'instant' });
                await sleep(800);
                atEnd = scroller.scrollTop === before
                    || scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 4;
            } else {
                var beforeY = window.scrollY;
                window.scrollBy({ top: Math.floor(window.innerHeight * 0.8), behavior: 'instant' });
                await sleep(800);
                atEnd = window.scrollY === beforeY
                    || window.scrollY + window.innerHeight >= document.body.scrollHeight - 4;
            }

            var errMsg = numEls.length === 0
                ? 'no numbered rows found — is the Purchased filter active?'
                : '';
            return JSON.stringify({ clicked: false, track: '', at_end: atEnd, error: errMsg });
        }

        // ── 5. Mark row and extract display text ──────────────────────────────
        row.dataset.cmpProcessed = '1';
        var trackText = row.textContent.trim().replace(/\s+/g, ' ').slice(0, 120);

        // ── 6. Click the hamburger (last button in row) ───────────────────────
        var btns = Array.from(row.querySelectorAll('button, [role="button"]'));
        if (btns.length === 0) {
            return JSON.stringify({ clicked: false, track: trackText, at_end: false,
                error: 'no buttons found in row' });
        }
        btns[btns.length - 1].click();

        // Let the menu animate open before we start looking for items
        await sleep(1500);

        // ── 7. Wait up to 6 s for a [role="menu"] container to appear ────────
        // We always want the container, not the leaf — that way we can search
        // inside it and call .click() on the leaf, which is the approach that
        // worked in INSPECT_ROW_JS.
        var menu = null;
        for (var w = 0; w < 40; w++) {
            await sleep(150);
            var menuEl = document.querySelector('[role="menu"], [role="listbox"], [role="dialog"]');
            if (menuEl) { menu = menuEl; break; }
            // Fallback: if Amazon doesn't set role="menu", find a leaf with
            // text "download" and use its parent as the container.
            var dlEl = Array.from(document.querySelectorAll('*')).find(function(el) {
                return el.children.length === 0
                    && el.textContent.trim().toLowerCase() === 'download';
            });
            if (dlEl) { menu = dlEl.parentElement; break; }
        }

        if (!menu) {
            return JSON.stringify({ clicked: false, track: trackText, at_end: false,
                error: 'context menu did not appear after hamburger click' });
        }

        // ── 8. Search inside the menu for the Download leaf, click it ─────────
        // Replicate exactly what worked in INSPECT_ROW_JS: iterate all leaf
        // descendants, find the one whose text === "download", call .click().
        var items = Array.from(menu.querySelectorAll('*')).filter(function(el) {
            return el.children.length === 0 && el.textContent.trim().length > 0;
        });

        var downloadEl = null;
        for (var m = 0; m < items.length; m++) {
            if (items[m].textContent.trim().toLowerCase() === 'download') {
                downloadEl = items[m];
                break;
            }
        }

        if (!downloadEl) {
            var opts = items.map(function(el) { return el.textContent.trim(); }).join(', ');
            document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
            return JSON.stringify({ clicked: false, track: trackText, at_end: false,
                error: 'no Download in menu: [' + opts + ']' });
        }

        // Click exactly the leaf — same as INSPECT_ROW_JS which confirmed this works.
        downloadEl.click();
        await sleep(500);
        return JSON.stringify({ clicked: true, track: trackText, at_end: false, error: '' });

    } catch(e) {
        return JSON.stringify({ clicked: false, track: '', at_end: false, error: e.toString() });
    }
})()"#;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn probe_debug_endpoint() -> Option<String> {
    let client = reqwest::Client::new();
    let resp = client
        .get("http://localhost:9222/json/list")
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .ok()?;
    let pages: Vec<Value> = resp.json().await.ok()?;
    find_amazon_page(&pages)
}

async fn wait_for_page(tx: &mpsc::Sender<CdpMsg>) -> Option<String> {
    let client = reqwest::Client::new();
    for i in 0..40 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Ok(r) = client
            .get("http://localhost:9222/json/list")
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            if let Ok(pages) = r.json::<Vec<Value>>().await {
                if let Some(url) = find_amazon_page(&pages) {
                    return Some(url);
                }
            }
        }
        if i % 5 == 4 {
            let _ = tx.send(CdpMsg::Log(format!("Still waiting... {}s", i + 1))).await;
        }
    }
    None
}

fn find_amazon_page(pages: &[Value]) -> Option<String> {
    pages.iter().find_map(|p| {
        let typ = p["type"].as_str()?;
        let url = p["url"].as_str()?;
        let ws  = p["webSocketDebuggerUrl"].as_str()?;
        if typ == "page" && (url.contains("amazon.com") || url.contains("amazonmusic")) {
            Some(ws.to_string())
        } else {
            None
        }
    })
}

fn is_amazon_music_running() -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq Amazon Music.exe", "/FO", "CSV", "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("Amazon Music.exe"))
        .unwrap_or(false)
}

async fn cdp_send<S>(sink: &mut S, id: u64, method: &str, params: Value)
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let msg = json!({"id": id, "method": method, "params": params});
    let _ = sink.send(Message::Text(msg.to_string().into())).await;
}

async fn read_response<S>(stream: &mut S, target_id: u64) -> Option<Value>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    read_response_timeout(stream, target_id, 15).await
}

async fn read_response_timeout<S>(stream: &mut S, target_id: u64, secs: u64) -> Option<Value>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    tokio::time::timeout(Duration::from_secs(secs), async {
        while let Some(msg) = stream.next().await {
            if let Ok(Message::Text(text)) = msg {
                if let Ok(val) = serde_json::from_str::<Value>(&text) {
                    if val["id"].as_u64() == Some(target_id) {
                        return Some(val);
                    }
                }
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

async fn drain_until_id<S>(stream: &mut S, target_id: u64)
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let _ = read_response(stream, target_id).await;
}

fn extract_string_result(val: &Value) -> String {
    val["result"]["result"]["value"]
        .as_str()
        .unwrap_or("(no result)")
        .to_string()
}

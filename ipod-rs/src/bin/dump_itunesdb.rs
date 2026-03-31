/// dump-itunesdb — print every field of every record in an iTunesDB file.
///
/// Usage:
///   cargo run --bin dump-itunesdb -- <path/to/iTunesDB>
///
/// Designed to compare what iTunes writes against what ipod-rs writes so we
/// can find any structural differences byte-by-byte.

use std::env;
use std::fs;
use std::process;

fn main() {
    let path = env::args().nth(1).unwrap_or_else(|| {
        eprintln!("Usage: dump-itunesdb <iTunesDB>");
        process::exit(1);
    });

    let data = fs::read(&path).unwrap_or_else(|e| {
        eprintln!("Cannot read {path}: {e}");
        process::exit(1);
    });

    println!("File: {path}  ({} bytes)", data.len());
    println!();

    if data.len() < 4 || &data[0..4] != b"mhbd" {
        eprintln!("Not an iTunesDB file (missing mhbd magic)");
        process::exit(1);
    }

    dump_mhbd(&data);
}

// ---------------------------------------------------------------------------
// mhbd
// ---------------------------------------------------------------------------

fn dump_mhbd(data: &[u8]) {
    let hdr_len   = r32(data, 0x04);
    let total_len = r32(data, 0x08);
    let always1   = r32(data, 0x0C);
    let db_ver    = r32(data, 0x10);
    let children  = r32(data, 0x14);
    let id_lo     = r32(data, 0x18);
    let id_hi     = r32(data, 0x1C);

    println!("══ mhbd ═════════════════════════════════════════════");
    println!("  0x04  header_len   = {hdr_len:#010x}  ({hdr_len})");
    println!("  0x08  total_len    = {total_len:#010x}  ({total_len})");
    println!("  0x0C  always_1     = {always1:#010x}  ({always1})");
    println!("  0x10  db_version   = {db_ver:#010x}  ({db_ver})");
    println!("  0x14  child_count  = {children:#010x}  ({children})");
    println!("  0x18  unique_id_lo = {id_lo:#010x}");
    println!("  0x1C  unique_id_hi = {id_hi:#010x}");
    println!();

    // Dump any non-zero bytes in the rest of the header
    dump_nonzero_header(data, 0x20, hdr_len as usize, "mhbd");

    // Walk children starting at hdr_len
    let mut pos = hdr_len as usize;
    while pos + 8 <= data.len() {
        match &data[pos..pos + 4] {
            b"mhlt" => dump_mhlt(data, pos),
            b"mhlp" => dump_mhlp(data, pos),
            _ => {
                println!("  [unknown record {:?} at {pos:#x}]", std::str::from_utf8(&data[pos..pos+4]));
            }
        }
        let total = r32(data, pos + 8) as usize;
        if total == 0 { break; }
        pos += total;
    }
}

// ---------------------------------------------------------------------------
// mhlt + mhit
// ---------------------------------------------------------------------------

fn dump_mhlt(data: &[u8], off: usize) {
    let hdr_len     = r32(data, off + 0x04);
    let total_len   = r32(data, off + 0x08);
    let track_count = r32(data, off + 0x0C);

    println!("  ── mhlt ────────────────────────────────────────────");
    println!("    0x04  header_len   = {hdr_len:#010x}  ({hdr_len})");
    println!("    0x08  total_len    = {total_len:#010x}  ({total_len})");
    println!("    0x0C  track_count  = {track_count:#010x}  ({track_count})");
    dump_nonzero_header(data, off + 0x10, off + hdr_len as usize, "mhlt");
    println!();

    let end = off + total_len as usize;
    let mut pos = off + hdr_len as usize;
    let mut idx = 0u32;

    while pos + 8 <= end && pos + 8 <= data.len() {
        if &data[pos..pos + 4] != b"mhit" { break; }
        dump_mhit(data, pos, idx);
        idx += 1;
        let t = r32(data, pos + 8) as usize;
        if t == 0 { break; }
        pos += t;
    }
}

fn dump_mhit(data: &[u8], off: usize, idx: u32) {
    let hdr_len    = r32(data, off + 0x04);
    let total_len  = r32(data, off + 0x08);
    let num_mhod   = r32(data, off + 0x0C);
    let track_id   = r32(data, off + 0x10);

    println!("    ┌─ mhit #{idx} ──────────────────────────────────────");
    println!("    │  offset in file: {off:#x}");
    println!("    │  0x04  header_len   = {hdr_len:#010x}  ({hdr_len})");
    println!("    │  0x08  total_len    = {total_len:#010x}  ({total_len})");
    println!("    │  0x0C  num_mhod     = {num_mhod:#010x}  ({num_mhod})");
    println!("    │  0x10  track_id     = {track_id:#010x}  ({track_id})");

    // Print every 4-byte word in the mhit header that is non-zero
    let end = off + hdr_len as usize;
    let mut o = off + 0x14;
    while o + 4 <= end && o + 4 <= data.len() {
        let v = r32(data, o);
        let rel = o - off;
        println!("    │  {rel:#06x}  = {v:#010x}  ({v})");
        o += 4;
    }

    // Dump mhod children
    let mhod_end = off + total_len as usize;
    let mut mpos = off + hdr_len as usize;
    while mpos + 12 <= mhod_end && mpos + 12 <= data.len() {
        if &data[mpos..mpos + 4] != b"mhod" { break; }
        dump_mhod(data, mpos, "    │  ");
        let t = r32(data, mpos + 8) as usize;
        if t == 0 { break; }
        mpos += t;
    }
    println!("    └────────────────────────────────────────────────");
    println!();
}

// ---------------------------------------------------------------------------
// mhlp + mhyp + mhip
// ---------------------------------------------------------------------------

fn dump_mhlp(data: &[u8], off: usize) {
    let hdr_len    = r32(data, off + 0x04);
    let total_len  = r32(data, off + 0x08);
    let pl_count   = r32(data, off + 0x0C);

    println!("  ── mhlp ────────────────────────────────────────────");
    println!("    0x04  header_len     = {hdr_len:#010x}  ({hdr_len})");
    println!("    0x08  total_len      = {total_len:#010x}  ({total_len})");
    println!("    0x0C  playlist_count = {pl_count:#010x}  ({pl_count})");
    dump_nonzero_header(data, off + 0x10, off + hdr_len as usize, "mhlp");
    println!();

    let end = off + total_len as usize;
    let mut pos = off + hdr_len as usize;
    let mut idx = 0u32;

    while pos + 8 <= end && pos + 8 <= data.len() {
        if &data[pos..pos + 4] != b"mhyp" { break; }
        dump_mhyp(data, pos, idx);
        idx += 1;
        let t = r32(data, pos + 8) as usize;
        if t == 0 { break; }
        pos += t;
    }
}

fn dump_mhyp(data: &[u8], off: usize, idx: u32) {
    let hdr_len    = r32(data, off + 0x04);
    let total_len  = r32(data, off + 0x08);
    let child_cnt  = r32(data, off + 0x0C);
    let mhip_cnt   = r32(data, off + 0x10);
    let is_master  = r32(data, off + 0x14);

    println!("    ┌─ mhyp #{idx} ──────────────────────────────────────");
    println!("    │  0x04  header_len   = {hdr_len:#010x}  ({hdr_len})");
    println!("    │  0x08  total_len    = {total_len:#010x}  ({total_len})");
    println!("    │  0x0C  child_count  = {child_cnt:#010x}  ({child_cnt})");
    println!("    │  0x10  mhip_count   = {mhip_cnt:#010x}  ({mhip_cnt})");
    println!("    │  0x14  is_master    = {is_master:#010x}  ({is_master})");
    dump_nonzero_header(data, off + 0x18, off + hdr_len as usize, "mhyp");

    let end = off + total_len as usize;
    let mut pos = off + hdr_len as usize;

    while pos + 8 <= end && pos + 8 <= data.len() {
        match &data[pos..pos + 4] {
            b"mhod" => dump_mhod(data, pos, "    │  "),
            b"mhip" => dump_mhip(data, pos),
            _ => break,
        }
        let t = r32(data, pos + 8) as usize;
        if t == 0 { break; }
        pos += t;
    }
    println!("    └────────────────────────────────────────────────");
    println!();
}

fn dump_mhip(data: &[u8], off: usize) {
    let hdr_len   = r32(data, off + 0x04);
    let total_len = r32(data, off + 0x08);
    let corr_id   = r32(data, off + 0x18);
    let order     = r32(data, off + 0x20);

    println!("    │  mhip  hdr={hdr_len} total={total_len}  \
              corr_id={corr_id}  order={order}");

    // Print any other non-zero words
    let end = off + hdr_len as usize;
    let known = [0x04, 0x08, 0x18, 0x20];
    let mut o = off + 0x0C;
    while o + 4 <= end && o + 4 <= data.len() {
        let rel = o - off;
        if !known.contains(&rel) {
            let v = r32(data, o);
            if v != 0 {
                println!("    │    {rel:#06x}  = {v:#010x}  ({v})");
            }
        }
        o += 4;
    }
}

// ---------------------------------------------------------------------------
// mhod
// ---------------------------------------------------------------------------

fn dump_mhod(data: &[u8], off: usize, prefix: &str) {
    let hdr_len   = r32(data, off + 0x04);
    let total_len = r32(data, off + 0x08);
    let str_type  = r32(data, off + 0x0C);
    let encoding  = r32(data, off + 0x18);
    let str_len   = r32(data, off + 0x1C) as usize;
    let pad       = r32(data, off + 0x20);

    let type_name = match str_type {
        1  => "title",
        2  => "file_path",
        3  => "album",
        4  => "artist",
        5  => "genre",
        6  => "filetype",
        12 => "comment",
        13 => "category",
        14 => "composer",
        _  => "unknown",
    };

    let text = if encoding == 1 && off + 36 + str_len <= data.len() {
        let utf16: Vec<u16> = data[off + 36..off + 36 + str_len]
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        String::from_utf16(&utf16).unwrap_or_else(|_| "<invalid utf16>".into())
    } else {
        format!("<encoding={encoding} len={str_len}>")
    };

    println!("{prefix}mhod  type={str_type} ({type_name})  hdr={hdr_len}  \
              total={total_len}  enc={encoding}  str_bytes={str_len}  \
              pad={pad}");
    println!("{prefix}       value: {text:?}");

    // Print any non-zero bytes between 0x0C and string data we haven't covered
    let end_of_known = 0x24; // string data starts at 0x24
    let mut o = off + 0x10;
    while o + 4 <= off + end_of_known && o + 4 <= data.len() {
        let rel = o - off;
        if !matches!(rel, 0x18 | 0x1C | 0x20) {
            let v = r32(data, o);
            if v != 0 {
                println!("{prefix}       extra {rel:#06x} = {v:#010x}  ({v})");
            }
        }
        o += 4;
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Print every non-zero 4-byte word between `from` and `to` (byte offsets into data).
fn dump_nonzero_header(data: &[u8], from: usize, to: usize, label: &str) {
    let mut any = false;
    let mut o = from;
    while o + 4 <= to && o + 4 <= data.len() {
        let v = r32(data, o);
        if v != 0 {
            if !any {
                println!("  [{label} extra non-zero header fields]");
                any = true;
            }
            println!("    {o:#06x}  = {v:#010x}  ({v})");
        }
        o += 4;
    }
}

#[inline]
fn r32(data: &[u8], off: usize) -> u32 {
    if off + 4 > data.len() { return 0; }
    u32::from_le_bytes(data[off..off + 4].try_into().unwrap())
}

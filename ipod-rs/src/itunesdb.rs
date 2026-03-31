//! iTunesDB binary format reader/writer — targeted at 4th-generation iPod.
//!
//! ## Database layout (version 9 / 4th gen click-wheel)
//!
//! ```text
//! mhbd  root record
//!   mhlt  track list
//!     mhit  one per track (header 0xF4 bytes, followed by mhod children)
//!       mhod type=2   iPod-relative file path  ← REQUIRED for playback
//!       mhod type=1   title
//!       mhod type=4   artist
//!       mhod type=3   album
//!   mhlp  playlist container
//!     mhyp  master "Songs" playlist  (identified by is_master=1 at 0x14,
//!                                     or simply the first mhyp)
//!       mhod type=1   playlist name "Music"
//!       mhip          one per track  (correlation_id at 0x18 → mhit track_id)
//! ```
//!
//! ## Key field offsets
//!
//! | Record | Offset | Field                  |
//! |--------|--------|------------------------|
//! | mhbd   | 0x04   | header_len = 0x68      |
//! | mhbd   | 0x08   | total_len              |
//! | mhbd   | 0x0C   | always 1               |
//! | mhbd   | 0x10   | db_version = 9         |
//! | mhbd   | 0x14   | child_count = 2        |
//! | mhlt   | 0x04   | header_len = 0x5C      |
//! | mhlt   | 0x08   | total_len              |
//! | mhlt   | 0x0C   | track_count            |
//! | mhit   | 0x04   | header_len = 0xF4      |
//! | mhit   | 0x08   | total_len              |
//! | mhit   | 0x0C   | num_mhod               |
//! | mhit   | 0x10   | track_id (unique)      |
//! | mhit   | 0x14   | visible = 1            |
//! | mhit   | 0x20   | modification_date      |
//! | mhit   | 0x24   | file_size              |
//! | mhit   | 0x28   | duration_ms            |
//! | mhit   | 0x34   | year                   |
//! | mhit   | 0x38   | bitrate_kbps           |
//! | mhit   | 0x3C   | sample_rate (upper 16b)|
//! | mhit   | 0x68   | date_added             |
//! | mhit   | 0x74   | transferred = 1        |
//! | mhip   | 0x04   | header_len = 0x4C      |
//! | mhip   | 0x08   | total_len = 0x4C       |
//! | mhip   | 0x18   | correlation_id (mhit)  |
//! | mhip   | 0x20   | track_position (1-base)|
//! | mhyp   | 0x04   | header_len = 0x6C      |
//! | mhyp   | 0x08   | total_len              |
//! | mhyp   | 0x0C   | child_count            |
//! | mhyp   | 0x10   | mhip_count             |
//! | mhyp   | 0x14   | is_master (1 = Songs)  |
//! | mhod   | 0x04   | header_len = 0x18      |
//! | mhod   | 0x08   | total_len              |
//! | mhod   | 0x0C   | string_type            |
//! | mhod   | 0x18   | encoding (1=UTF-16LE)  |
//! | mhod   | 0x1C   | string_len_bytes       |
//! | mhod   | 0x20   | padding (1)            |
//! | mhod   | 0x24   | string data            |
//!
//! ## References
//! - <http://www.ipodlinux.org/ITunesDB>
//! - libgpod source (itunesdb.c)

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::{info, warn};

use crate::{IpodError, Result};

// Seconds between Mac epoch (1904-01-01) and Unix epoch (1970-01-01).
const MAC_EPOCH_OFFSET: u32 = 2_082_844_800;

// mhit fixed-header size for 4th gen (DB version 9).
const MHIT_HEADER: usize = 0xF4; // 244 bytes

// mhip fixed-header size — same for all DB versions we support.
const MHIP_HEADER: usize = 0x4C; // 76 bytes

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Metadata needed to create one mhit entry in iTunesDB.
pub struct DbTrack {
    /// iPod-relative path using colon separators (HFS-style), as required by
    /// the iPod firmware.  Example: `:iPod_Control:Music:F00:AAAA.mp3`
    pub ipod_rel_path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u32,
    pub file_size: u32,
    pub bitrate_kbps: u32,
    pub sample_rate_hz: u32,
    pub year: u32,
}

/// A summary of one track as recorded in the database.
#[derive(Debug, Clone)]
pub struct DbTrackEntry {
    pub track_id: u32,
    pub ipod_rel_path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u32,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Append a single track record to an existing `iTunesDB` file.
///
/// Returns a human-readable step log describing every action taken.
/// Steps:
/// 1. Append `mhit` (+ mhod children) to `mhlt`.
/// 2. Append `mhip` to the master `mhyp` so the track appears under Songs.
/// 3. Fix up all ancestor `total_len` / count fields.
/// 4. Write the result back and log a verification summary.
pub fn append_track(db_path: &Path, track: &DbTrack) -> Result<Vec<String>> {
    let mut log: Vec<String> = Vec::new();

    let mut data = fs::read(db_path)?;
    check_magic(&data, 0, b"mhbd")?;

    let db_version = read_u32(&data, 0x10);
    log.push(format!("  DB version={db_version}  file={} bytes", data.len()));
    info!("iTunesDB version: {db_version}");

    // Use the header size from the first existing mhit so we stay consistent
    // with whatever format is already on disk.  Fresh databases have no mhit
    // yet, so we fall back to 0xF4 (4th-gen standard).
    let mhit_hdr_len = first_mhit_header_len(&data).unwrap_or(MHIT_HEADER);
    log.push(format!("  mhit header_len = {mhit_hdr_len:#x} ({mhit_hdr_len} bytes)"));
    info!("Using mhit header_len = 0x{mhit_hdr_len:X}");

    // ── Step 1: insert mhit into mhlt ────────────────────────────────────────

    let mhlt_off = find_record(&data, 0, b"mhlt")
        .ok_or_else(|| IpodError::Database("mhlt not found".into()))?;
    log.push(format!("  mhlt found at offset {mhlt_off:#x}"));

    let new_id = find_max_track_id(&data, mhlt_off) + 1;
    log.push(format!("  assigning track_id={new_id}"));
    info!("Assigning track_id = {new_id}");

    log.push(format!("  building mhit: title={:?}  artist={:?}  album={:?}",
        track.title, track.artist, track.album));
    log.push(format!("    path={}", track.ipod_rel_path));
    log.push(format!("    duration={}ms  size={}B  bitrate={}kbps  rate={}Hz",
        track.duration_ms, track.file_size, track.bitrate_kbps, track.sample_rate_hz));
    log.push(format!("    visible=1  transferred=1"));

    let mhit_bytes = build_mhit(track, new_id, mhit_hdr_len);
    let mhit_len = mhit_bytes.len() as u32;
    log.push(format!("  mhit size={mhit_len} bytes  inserting into mhlt..."));

    let mhlt_total_before = read_u32(&data, mhlt_off + 8) as usize;
    let mhit_insert = mhlt_off + mhlt_total_before;
    data.splice(mhit_insert..mhit_insert, mhit_bytes);

    let v = read_u32(&data, mhlt_off + 8) + mhit_len;
    write_u32(&mut data, mhlt_off + 8, v);
    let mhlt_count = read_u32(&data, mhlt_off + 0x0C) + 1;
    write_u32(&mut data, mhlt_off + 0x0C, mhlt_count);
    log.push(format!("  ✓ mhlt updated: total_len={v}  track_count={mhlt_count}"));

    // ── Step 2: insert mhip into master mhyp ─────────────────────────────────

    let mhlp_off = find_record(&data, 0, b"mhlp")
        .ok_or_else(|| IpodError::Database("mhlp not found".into()))?;
    log.push(format!("  mhlp found at offset {mhlp_off:#x}"));

    let mhlp_hdr_len = read_u32(&data, mhlp_off + 4) as usize;
    let mhyp_off = find_master_mhyp(&data, mhlp_off + mhlp_hdr_len)
        .ok_or_else(|| IpodError::Database("master mhyp not found in mhlp".into()))?;
    log.push(format!("  master mhyp found at offset {mhyp_off:#x}"));

    let existing_mhip_count = read_u32(&data, mhyp_off + 0x10);
    let order = existing_mhip_count + 1;
    log.push(format!("  building mhip: corr_id={new_id}  playlist_order={order}"));

    let mhip_bytes = build_mhip(new_id, order);
    let mhip_len = mhip_bytes.len() as u32;
    log.push(format!("  mhip size={mhip_len} bytes  inserting into mhyp..."));

    let mhyp_total_before = read_u32(&data, mhyp_off + 8) as usize;
    let mhip_insert = mhyp_off + mhyp_total_before;
    data.splice(mhip_insert..mhip_insert, mhip_bytes);

    // Fix mhyp total_len, child_count, mhip_count
    let v = read_u32(&data, mhyp_off + 8) + mhip_len;
    write_u32(&mut data, mhyp_off + 8, v);
    let v = read_u32(&data, mhyp_off + 0x0C) + 1;
    write_u32(&mut data, mhyp_off + 0x0C, v);
    let v = read_u32(&data, mhyp_off + 0x10) + 1;
    write_u32(&mut data, mhyp_off + 0x10, v);
    log.push(format!("  ✓ mhyp updated: mhip_count={v}"));

    // Fix mhlp total_len
    let v = read_u32(&data, mhlp_off + 8) + mhip_len;
    write_u32(&mut data, mhlp_off + 8, v);
    log.push(format!("  ✓ mhlp total_len updated: {v}"));

    // ── Step 3: fix mhbd total_len ────────────────────────────────────────────
    let v = read_u32(&data, 8) + mhit_len + mhip_len;
    write_u32(&mut data, 8, v);
    log.push(format!("  ✓ mhbd total_len updated: {v}"));

    crate::atomic_write(db_path, &data)?;
    log.push(format!("  ✓ iTunesDB written: {} bytes", data.len()));

    // ── Step 4: verify ────────────────────────────────────────────────────────
    let mhlt_track_count = read_u32(&data, mhlt_off + 0x0C);
    let mhyp_mhip_count  = read_u32(&data, mhyp_off + 0x10);
    log.push(format!(
        "  ✓ verify: {mhlt_track_count} track(s) in mhlt, \
         {mhyp_mhip_count} in Songs playlist"
    ));
    info!(
        "iTunesDB written ({} bytes): {} track(s) in mhlt, {} in master playlist, \
         id={new_id}, playlist_order={order}, path={}",
        data.len(), mhlt_track_count, mhyp_mhip_count, track.ipod_rel_path,
    );

    Ok(log)
}

/// Create a minimal, valid iTunesDB at `db_path` for a 4th-gen iPod.
///
/// Writes DB version 9 with an empty track list and a single master "Songs"
/// playlist, ready to receive tracks via [`append_track`].
pub fn create_fresh_itunesdb(db_path: &Path) -> Result<()> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // ── mhod type=1: playlist name "Music" ───────────────────────────────────
    let name_mhod = build_mhod(1, "Music");
    let name_mhod_len = name_mhod.len() as u32;

    // ── mhyp: master (Songs) playlist ────────────────────────────────────────
    const MHYP_HDR: u32 = 0x6C;
    let mhyp_total = MHYP_HDR + name_mhod_len;
    let mut mhyp = vec![0u8; MHYP_HDR as usize];
    mhyp[0..4].copy_from_slice(b"mhyp");
    write_at(&mut mhyp, 0x04, MHYP_HDR);   // header_len
    write_at(&mut mhyp, 0x08, mhyp_total); // total_len
    write_at(&mut mhyp, 0x0C, 1u32);       // child_count  (1 mhod name)
    write_at(&mut mhyp, 0x10, 0u32);       // mhip_count   (0 tracks yet)
    write_at(&mut mhyp, 0x14, 1u32);       // is_master = 1
    mhyp.extend_from_slice(&name_mhod);

    // ── mhlp: playlist container ──────────────────────────────────────────────
    const MHLP_HDR: u32 = 0x5C;
    let mhlp_total = MHLP_HDR + mhyp_total;
    let mut mhlp = vec![0u8; MHLP_HDR as usize];
    mhlp[0..4].copy_from_slice(b"mhlp");
    write_at(&mut mhlp, 0x04, MHLP_HDR);   // header_len
    write_at(&mut mhlp, 0x08, mhlp_total); // total_len
    write_at(&mut mhlp, 0x0C, 1u32);       // playlist_count
    mhlp.extend_from_slice(&mhyp);

    // ── mhlt: empty track list ────────────────────────────────────────────────
    const MHLT_HDR: u32 = 0x5C;
    let mut mhlt = vec![0u8; MHLT_HDR as usize];
    mhlt[0..4].copy_from_slice(b"mhlt");
    write_at(&mut mhlt, 0x04, MHLT_HDR); // header_len = total_len (no tracks)
    write_at(&mut mhlt, 0x08, MHLT_HDR);
    write_at(&mut mhlt, 0x0C, 0u32);     // track_count = 0

    // ── mhbd: database root ───────────────────────────────────────────────────
    const MHBD_HDR: u32 = 0x68;
    let db_total = MHBD_HDR + MHLT_HDR + mhlp_total;
    let now_mac = unix_now_u32().wrapping_add(MAC_EPOCH_OFFSET);
    let mut mhbd = vec![0u8; MHBD_HDR as usize];
    mhbd[0..4].copy_from_slice(b"mhbd");
    write_at(&mut mhbd, 0x04, MHBD_HDR); // header_len
    write_at(&mut mhbd, 0x08, db_total); // total_len
    write_at(&mut mhbd, 0x0C, 1u32);     // required field, always 1
    write_at(&mut mhbd, 0x10, 9u32);     // DB version 9 (4th gen)
    write_at(&mut mhbd, 0x14, 2u32);     // child_count: mhlt + mhlp
    // 8-byte unique ID seeded from current time
    write_at(&mut mhbd, 0x18, now_mac);
    write_at(&mut mhbd, 0x1C, now_mac ^ 0xDEAD_BEEF);

    let mut db = mhbd;
    db.extend_from_slice(&mhlt);
    db.extend_from_slice(&mhlp);

    crate::atomic_write(db_path, &db)?;
    info!(
        "Created fresh iTunesDB ({} bytes, v9) at {}",
        db.len(),
        db_path.display()
    );
    Ok(())
}

/// Read all track entries from an existing iTunesDB.
pub fn read_tracks(db_path: &Path) -> Result<Vec<DbTrackEntry>> {
    let data = fs::read(db_path)?;
    check_magic(&data, 0, b"mhbd")?;

    let mhlt_off = find_record(&data, 0, b"mhlt")
        .ok_or_else(|| IpodError::Database("mhlt not found".into()))?;

    let mut entries = Vec::new();
    let mhlt_hdr_len = read_u32(&data, mhlt_off + 4) as usize;
    let mhlt_total   = read_u32(&data, mhlt_off + 8) as usize;
    let end = mhlt_off + mhlt_total;
    let mut pos = mhlt_off + mhlt_hdr_len;

    while pos + 20 <= end && pos + 20 <= data.len() {
        if &data[pos..pos + 4] != b"mhit" {
            break;
        }
        let track_id    = read_u32(&data, pos + 0x10);
        let mhit_hdr    = read_u32(&data, pos + 0x04) as usize;
        let mhit_total  = read_u32(&data, pos + 0x08) as usize;
        let duration_ms = read_u32(&data, pos + 0x28);

        let mut ipod_rel_path = String::new();
        let mut title         = String::new();
        let mut artist        = String::new();
        let mut album         = String::new();

        let mhod_end = pos + mhit_total;
        let mut mpos = pos + mhit_hdr;
        while mpos + 12 <= mhod_end && mpos + 12 <= data.len() {
            if &data[mpos..mpos + 4] != b"mhod" { break; }
            let mhod_type  = read_u32(&data, mpos + 0x0C);
            let mhod_total = read_u32(&data, mpos + 0x08) as usize;
            if matches!(mhod_type, 1 | 2 | 3 | 4) && mpos + 36 <= data.len() {
                let str_len = read_u32(&data, mpos + 0x1C) as usize;
                if mpos + 36 + str_len <= data.len() {
                    let utf16: Vec<u16> = data[mpos + 36..mpos + 36 + str_len]
                        .chunks_exact(2)
                        .map(|b| u16::from_le_bytes([b[0], b[1]]))
                        .collect();
                    if let Ok(s) = String::from_utf16(&utf16) {
                        match mhod_type {
                            1 => title         = s,
                            2 => ipod_rel_path = s,
                            3 => album         = s,
                            4 => artist        = s,
                            _ => {}
                        }
                    }
                }
            }
            if mhod_total == 0 { break; }
            mpos += mhod_total;
        }

        entries.push(DbTrackEntry { track_id, ipod_rel_path, title, artist, album, duration_ms });
        if mhit_total == 0 { break; }
        pos += mhit_total;
    }

    Ok(entries)
}

/// Check whether a track is in the master mhyp playlist.
pub fn is_in_master_playlist(db_path: &Path, track_id: u32) -> Result<bool> {
    let data = fs::read(db_path)?;
    check_magic(&data, 0, b"mhbd")?;

    let mhlp_off = match find_record(&data, 0, b"mhlp") {
        Some(off) => off,
        None => return Ok(false),
    };
    let mhlp_hdr_len = read_u32(&data, mhlp_off + 4) as usize;
    let mhyp_off = match find_master_mhyp(&data, mhlp_off + mhlp_hdr_len) {
        Some(off) => off,
        None => return Ok(false),
    };

    let mhyp_hdr_len = read_u32(&data, mhyp_off + 4) as usize;
    let mhyp_total   = read_u32(&data, mhyp_off + 8) as usize;
    let end = mhyp_off + mhyp_total;
    let mut pos = mhyp_off + mhyp_hdr_len;

    while pos + 12 <= end && pos + 12 <= data.len() {
        let total = read_u32(&data, pos + 8) as usize;
        if &data[pos..pos + 4] == b"mhip" {
            // Correlation ID is at mhip + 0x18
            let corr_id = read_u32(&data, pos + 0x18);
            if corr_id == track_id {
                return Ok(true);
            }
        }
        if total == 0 { break; }
        pos += total;
    }
    Ok(false)
}

/// Read every `correlation_id` stored in the master mhyp playlist in one
/// pass, returning a `HashSet` of track IDs.
///
/// Prefer this over repeated calls to [`is_in_master_playlist`] when checking
/// multiple tracks: it reads the database file once regardless of track count,
/// reducing an O(n²) pattern to O(n).
pub fn read_master_playlist_ids(db_path: &Path) -> Result<HashSet<u32>> {
    let data = fs::read(db_path)?;
    check_magic(&data, 0, b"mhbd")?;

    let mhlp_off = match find_record(&data, 0, b"mhlp") {
        Some(off) => off,
        None => return Ok(HashSet::new()),
    };
    let mhlp_hdr_len = read_u32(&data, mhlp_off + 4) as usize;
    let mhyp_off = match find_master_mhyp(&data, mhlp_off + mhlp_hdr_len) {
        Some(off) => off,
        None => return Ok(HashSet::new()),
    };

    let mhyp_hdr_len = read_u32(&data, mhyp_off + 4) as usize;
    let mhyp_total   = read_u32(&data, mhyp_off + 8) as usize;
    let end = mhyp_off + mhyp_total;
    let mut pos = mhyp_off + mhyp_hdr_len;
    let mut ids = HashSet::new();

    while pos + 12 <= end && pos + 12 <= data.len() {
        let total = read_u32(&data, pos + 8) as usize;
        if &data[pos..pos + 4] == b"mhip" {
            ids.insert(read_u32(&data, pos + 0x18)); // correlation_id → mhit
        }
        if total == 0 { break; }
        pos += total;
    }

    Ok(ids)
}

/// Add an `mhip` for a track that has an `mhit` but is absent from the
/// master playlist (repair function).
pub fn repair_add_to_master_playlist(db_path: &Path, track_id: u32) -> Result<()> {
    let mut data = fs::read(db_path)?;
    check_magic(&data, 0, b"mhbd")?;

    let mhlp_off = find_record(&data, 0, b"mhlp")
        .ok_or_else(|| IpodError::Database("mhlp not found".into()))?;
    let mhlp_hdr_len = read_u32(&data, mhlp_off + 4) as usize;
    let mhyp_off = find_master_mhyp(&data, mhlp_off + mhlp_hdr_len)
        .ok_or_else(|| IpodError::Database("master mhyp not found".into()))?;

    let existing_mhip_count = read_u32(&data, mhyp_off + 0x10);
    let order = existing_mhip_count + 1;

    let mhip_bytes = build_mhip(track_id, order);
    let mhip_len   = mhip_bytes.len() as u32;

    let mhyp_total_before = read_u32(&data, mhyp_off + 8) as usize;
    let mhip_insert = mhyp_off + mhyp_total_before;
    data.splice(mhip_insert..mhip_insert, mhip_bytes);

    let v = read_u32(&data, mhyp_off + 8)    + mhip_len; write_u32(&mut data, mhyp_off + 8, v);
    let v = read_u32(&data, mhyp_off + 0x0C) + 1;        write_u32(&mut data, mhyp_off + 0x0C, v);
    let v = read_u32(&data, mhyp_off + 0x10) + 1;        write_u32(&mut data, mhyp_off + 0x10, v);
    let v = read_u32(&data, mhlp_off + 8)    + mhip_len; write_u32(&mut data, mhlp_off + 8, v);
    let v = read_u32(&data, 8)               + mhip_len; write_u32(&mut data, 8, v);

    crate::atomic_write(db_path, &data)?;
    info!("Repaired: added mhip for track_id={track_id} to master playlist");
    Ok(())
}

// ---------------------------------------------------------------------------
// Record builders
// ---------------------------------------------------------------------------

fn build_mhit(track: &DbTrack, track_id: u32, hdr_len: usize) -> Vec<u8> {
    let mut mhods: Vec<Vec<u8>> = vec![
        build_mhod(2, &track.ipod_rel_path), // filename — REQUIRED
        build_mhod(1, &track.title),          // title
    ];
    if !track.artist.is_empty() { mhods.push(build_mhod(4, &track.artist)); }
    if !track.album.is_empty()  { mhods.push(build_mhod(3, &track.album));  }

    let mhods_len: usize = mhods.iter().map(|m| m.len()).sum();
    let total_len  = (hdr_len + mhods_len) as u32;
    let num_mhods  = mhods.len() as u32;
    let now_mac    = unix_now_u32().wrapping_add(MAC_EPOCH_OFFSET);

    let mut h = vec![0u8; hdr_len];
    h[0..4].copy_from_slice(b"mhit");
    write_at(&mut h, 0x04, hdr_len as u32); // header_len
    write_at(&mut h, 0x08, total_len);       // total_len
    write_at(&mut h, 0x0C, num_mhods);       // mhod child count
    write_at(&mut h, 0x10, track_id);        // unique track id
    write_at(&mut h, 0x14, 1u32);            // visible flag — 1 = shown in menus
    write_at(&mut h, 0x20, now_mac);                                          // modification date
    write_at(&mut h, 0x24, track.file_size);                                  // file size (bytes)
    write_at(&mut h, 0x28, track.duration_ms);                                // duration (ms)
    write_at(&mut h, 0x34, track.year);                                       // year
    write_at(&mut h, 0x38, track.bitrate_kbps);                               // bitrate (kbps)
    write_at(&mut h, 0x3C, track.sample_rate_hz.wrapping_shl(16));            // sample rate (16.16 fixed)
    if hdr_len >= 0x6C {
        write_at(&mut h, 0x68, now_mac); // date added (present in v9 0xF4 header)
    }
    if hdr_len >= 0x78 {
        // transferred = 1: file is on the device, not a computer-side reference.
        // Without this the firmware hides the track from all menus (Songs / Artists / Albums).
        write_at(&mut h, 0x74, 1u32);
    }

    let mut out = h;
    for m in mhods { out.extend_from_slice(&m); }
    out
}

/// Build a 76-byte `mhip` (playlist item) record.
///
/// Layout confirmed against libgpod and iPodLinux wiki:
/// - 0x18: correlation_id — unique track ID from the matching mhit
/// - 0x20: track_position — 1-based index in the playlist
fn build_mhip(track_id: u32, order: u32) -> Vec<u8> {
    let mut rec = vec![0u8; MHIP_HEADER];
    rec[0..4].copy_from_slice(b"mhip");
    write_at(&mut rec, 0x04, MHIP_HEADER as u32); // header_len = total_len
    write_at(&mut rec, 0x08, MHIP_HEADER as u32);
    // 0x0C: num_child_mhod = 0
    // 0x10: podcast_grouping_flag = 0
    // 0x14: group_id = 0
    write_at(&mut rec, 0x18, track_id);            // correlation_id → mhit
    // 0x1C: album_id = 0
    write_at(&mut rec, 0x20, order);               // track_position (1-based)
    rec
}

/// Build an mhod string record (UTF-16LE encoded).
fn build_mhod(string_type: u32, text: &str) -> Vec<u8> {
    let utf16: Vec<u8> = text.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    let str_byte_len = utf16.len() as u32;
    let total_len    = 36 + str_byte_len; // 36 = 0x24 (fixed overhead before string data)

    let mut rec = vec![0u8; total_len as usize];
    rec[0..4].copy_from_slice(b"mhod");
    write_at(&mut rec, 0x04, 0x18u32);      // header_len = 24
    write_at(&mut rec, 0x08, total_len);     // total_len
    write_at(&mut rec, 0x0C, string_type);   // string type
    write_at(&mut rec, 0x18, 1u32);          // encoding: 1 = UTF-16LE
    write_at(&mut rec, 0x1C, str_byte_len);  // string length in bytes
    write_at(&mut rec, 0x20, 1u32);          // pad/unk — 1 per libgpod
    rec[36..36 + utf16.len()].copy_from_slice(&utf16);
    rec
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return the header_len of the first `mhit` found in the DB, or `None` if
/// the track list is empty.  Used to stay consistent with the existing DB
/// format when appending to a database we did not create ourselves.
fn first_mhit_header_len(data: &[u8]) -> Option<usize> {
    let mhlt_off     = find_record(data, 0, b"mhlt")?;
    let mhlt_hdr_len = read_u32(data, mhlt_off + 4) as usize;
    let mhlt_total   = read_u32(data, mhlt_off + 8) as usize;
    let pos = mhlt_off + mhlt_hdr_len;
    if pos + 8 <= mhlt_off + mhlt_total
        && pos + 8 <= data.len()
        && &data[pos..pos + 4] == b"mhit"
    {
        Some(read_u32(data, pos + 4) as usize)
    } else {
        None // empty track list
    }
}

/// Walk sibling records starting at `from` to find one with `magic`.
///
/// When `from == 0`, starts at the mhbd child offset (= mhbd header_len).
fn find_record(data: &[u8], from: usize, magic: &[u8; 4]) -> Option<usize> {
    let start = if from == 0 {
        read_u32(data, 4) as usize // mhbd header_len → first child
    } else {
        from
    };
    let mut pos = start;
    while pos + 12 <= data.len() {
        if data[pos..pos + 4] == *magic {
            return Some(pos);
        }
        let total = read_u32(data, pos + 8) as usize;
        if total == 0 { break; }
        pos += total;
    }
    None
}

/// Find the master `mhyp` (is_master == 1 at 0x14) starting at `start`.
/// Falls back to the first `mhyp` if none carries the flag.
fn find_master_mhyp(data: &[u8], start: usize) -> Option<usize> {
    let mut pos = start;
    let mut first: Option<usize> = None;

    while pos + 24 <= data.len() {
        if &data[pos..pos + 4] != b"mhyp" { break; }
        if first.is_none() { first = Some(pos); }
        if read_u32(data, pos + 0x14) == 1 { return Some(pos); }
        let total = read_u32(data, pos + 8) as usize;
        if total == 0 { break; }
        pos += total;
    }

    if first.is_none() {
        warn!("No mhyp found in mhlp — cannot add track to playlist");
    }
    first
}

fn find_max_track_id(data: &[u8], mhlt_off: usize) -> u32 {
    let hdr_len   = read_u32(data, mhlt_off + 4) as usize;
    let total_len = read_u32(data, mhlt_off + 8) as usize;
    let end = mhlt_off + total_len;
    let mut pos = mhlt_off + hdr_len;
    let mut max = 0u32;

    while pos + 20 <= end && pos + 20 <= data.len() {
        if &data[pos..pos + 4] != b"mhit" { break; }
        max = max.max(read_u32(data, pos + 0x10));
        let t = read_u32(data, pos + 8) as usize;
        if t == 0 { break; }
        pos += t;
    }
    max
}

fn check_magic(data: &[u8], offset: usize, expected: &[u8; 4]) -> Result<()> {
    if data.len() < offset + 4 || &data[offset..offset + 4] != expected {
        Err(IpodError::Database(format!(
            "Expected magic {:?} at offset {offset}",
            std::str::from_utf8(expected).unwrap_or("????")
        )))
    } else {
        Ok(())
    }
}

#[inline]
fn read_u32(data: &[u8], offset: usize) -> u32 {
    if offset + 4 > data.len() { return 0; }
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

#[inline]
fn write_u32(data: &mut Vec<u8>, offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[inline]
fn write_at(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn unix_now_u32() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32
}

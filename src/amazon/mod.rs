//! Amazon Music easter egg — download owned DRM-free MP3s.
//!
//! Activated by pressing A → C → E within 2 seconds from the library screen.
//! The user supplies a browser cookie string (copied from DevTools → Application
//! → Cookies → music.amazon.com).  No DRM circumvention — only "Purchased" /
//! "Uploaded" tracks that Amazon lets you download outright are listed.
//!
//! API: Amazon Music's internal "cirrus" REST endpoint.
//!   POST https://music.amazon.com/cirrus/
//!   Content-Type: application/x-www-form-urlencoded
//!   Cookie: <user-supplied>

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use tracing::info;

// ---------------------------------------------------------------------------
// Public message type  (async task → UI inbox)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum AmazonMsg {
    /// Catalog listing fetched (or updated).
    Tracks(Vec<AmazonTrack>),
    /// Download byte-progress update.
    Progress { asin: String, bytes: u64, total: Option<u64> },
    /// A track finished downloading.
    Downloaded { asin: String, path: PathBuf },
    /// Non-fatal error (shown as status text).
    Error(String),
}

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AmazonTrack {
    pub asin: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Track duration in seconds.
    pub duration_secs: u32,
    /// Pre-resolved download URL, if already fetched from metadata endpoint.
    pub download_url: Option<String>,
}

impl AmazonTrack {
    /// One-line display string.
    pub fn display_line(&self) -> String {
        let mins = self.duration_secs / 60;
        let secs = self.duration_secs % 60;
        format!("{} — {}  [{mins}:{secs:02}]", self.artist, self.title)
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

const CIRRUS_URL: &str = "https://music.amazon.com/cirrus/";
const PAGE_SIZE: usize = 50;

pub struct AmazonClient {
    cookie: String,
    http: reqwest::Client,
}

impl AmazonClient {
    pub fn new(cookie: String) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
                 AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/124.0.0.0 Safari/537.36",
            )
            .build()
            .expect("reqwest client");
        Self { cookie, http }
    }

    // -----------------------------------------------------------------------
    // Catalog fetch
    // -----------------------------------------------------------------------

    /// Fetch the user's entire purchased/uploaded library.
    /// Results arrive via `inbox` as a single `AmazonMsg::Tracks` when complete,
    /// or `AmazonMsg::Error` on failure.
    pub async fn fetch_catalog(&self, inbox: Arc<Mutex<Vec<AmazonMsg>>>) {
        let mut all: Vec<AmazonTrack> = Vec::new();
        let mut offset = 0usize;

        loop {
            match self.fetch_page(offset).await {
                Ok((page, total)) => {
                    let got = page.len();
                    all.extend(page);
                    if all.len() >= total || got == 0 {
                        break;
                    }
                    offset += got;
                }
                Err(e) => {
                    push(&inbox, AmazonMsg::Error(format!("Catalog fetch failed: {e}")));
                    return;
                }
            }
        }

        info!("Amazon: fetched {} tracks", all.len());
        push(&inbox, AmazonMsg::Tracks(all));
    }

    async fn fetch_page(&self, offset: usize) -> anyhow::Result<(Vec<AmazonTrack>, usize)> {
        let body = format!(
            "Operation=searchLibrary\
             &ContentType=JSON\
             &customerInfo.marketplaceId=ATVPDKIKX0DER\
             &searchCriteria.member.1.attributeName=primaryArtist\
             &searchCriteria.member.1.comparisonType=LIKE\
             &searchCriteria.member.1.attributeValue=\
             &selectCriteria.member.1=albumArtistName\
             &selectCriteria.member.2=albumName\
             &selectCriteria.member.3=asin\
             &selectCriteria.member.4=duration\
             &selectCriteria.member.5=title\
             &selectCriteria.member.6=primaryArtist\
             &sortCriteriaList.member.1.sortColumn=sortArtist\
             &sortCriteriaList.member.1.sortType=ASC\
             &maxResults={PAGE_SIZE}\
             &nextResultsToken={offset}"
        );

        let resp = self
            .http
            .post(CIRRUS_URL)
            .header("Cookie", &self.cookie)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("X-Requested-With", "XMLHttpRequest")
            .body(body)
            .send()
            .await
            .context("POST cirrus/searchLibrary")?;

        let status = resp.status();
        let text = resp.text().await.context("reading response")?;

        if !status.is_success() {
            anyhow::bail!(
                "Amazon API HTTP {status}: {}",
                &text[..text.len().min(300)]
            );
        }

        let v: serde_json::Value = serde_json::from_str(&text).context("parse JSON")?;

        let result = &v["searchLibraryResponse"]["searchLibraryResult"];
        let total = result["totalResultSetSize"]
            .as_u64()
            .unwrap_or(0) as usize;

        let empty = vec![];
        let items = result["libraryTrackList"]["libraryTrack"]
            .as_array()
            .unwrap_or(&empty);

        let tracks: Vec<AmazonTrack> = items
            .iter()
            .filter_map(|t| {
                let asin = t["asin"].as_str()?.to_owned();
                let title = t["title"].as_str().unwrap_or("Unknown").to_owned();
                let artist = t["primaryArtist"]["name"]
                    .as_str()
                    .or_else(|| t["albumArtistName"].as_str())
                    .unwrap_or("Unknown Artist")
                    .to_owned();
                let album = t["albumName"].as_str().unwrap_or("").to_owned();
                let duration_secs = t["duration"].as_u64().unwrap_or(0) as u32;
                Some(AmazonTrack { asin, title, artist, album, duration_secs, download_url: None })
            })
            .collect();

        Ok((tracks, total))
    }

    // -----------------------------------------------------------------------
    // Download
    // -----------------------------------------------------------------------

    /// Download `track` into `dir`, streaming progress back via `inbox`.
    pub async fn download_track(
        &self,
        track: AmazonTrack,
        dir: PathBuf,
        inbox: Arc<Mutex<Vec<AmazonMsg>>>,
    ) {
        // 1. Resolve download URL.
        let url = match track.download_url.clone() {
            Some(u) => u,
            None => match self.resolve_url(&track.asin).await {
                Ok(u) => u,
                Err(e) => {
                    push(
                        &inbox,
                        AmazonMsg::Error(format!("URL for {}: {e}", track.asin)),
                    );
                    return;
                }
            },
        };

        // 2. Stream to disk.
        let mut resp = match self
            .http
            .get(&url)
            .header("Cookie", &self.cookie)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                push(&inbox, AmazonMsg::Error(format!("Download {}: {e}", track.asin)));
                return;
            }
        };

        let total = resp.content_length();
        let filename = format!(
            "{} - {}.mp3",
            sanitize_name(&track.artist),
            sanitize_name(&track.title)
        );
        let dest = dir.join(&filename);

        let mut file = match std::fs::File::create(&dest) {
            Ok(f) => f,
            Err(e) => {
                push(
                    &inbox,
                    AmazonMsg::Error(format!("Cannot create {filename}: {e}")),
                );
                return;
            }
        };

        use std::io::Write;
        let mut downloaded: u64 = 0;
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    if let Err(e) = file.write_all(&chunk) {
                        push(
                            &inbox,
                            AmazonMsg::Error(format!("Write {filename}: {e}")),
                        );
                        return;
                    }
                    downloaded += chunk.len() as u64;
                    push(
                        &inbox,
                        AmazonMsg::Progress {
                            asin: track.asin.clone(),
                            bytes: downloaded,
                            total,
                        },
                    );
                }
                Ok(None) => break,
                Err(e) => {
                    push(
                        &inbox,
                        AmazonMsg::Error(format!("Stream {}: {e}", track.asin)),
                    );
                    return;
                }
            }
        }

        info!("Downloaded {} → {}", track.asin, dest.display());
        push(&inbox, AmazonMsg::Downloaded { asin: track.asin, path: dest });
    }

    async fn resolve_url(&self, asin: &str) -> anyhow::Result<String> {
        let body = format!(
            "Operation=getTrackMetadata\
             &ContentType=JSON\
             &asin={asin}\
             &features=Preorder\
             &trackMetadataResponse.trackResponseType=DOWNLOAD"
        );

        let resp = self
            .http
            .post(CIRRUS_URL)
            .header("Cookie", &self.cookie)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("X-Requested-With", "XMLHttpRequest")
            .body(body)
            .send()
            .await?;

        let text = resp.text().await?;
        let v: serde_json::Value = serde_json::from_str(&text)?;

        v["getTrackMetadataResponse"]["trackInfo"]["streamUrls"]["streamUrl"][0]["url"]
            .as_str()
            .context("no download URL in metadata response")
            .map(str::to_owned)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn push(inbox: &Arc<Mutex<Vec<AmazonMsg>>>, msg: AmazonMsg) {
    if let Ok(mut q) = inbox.lock() {
        q.push(msg);
    }
}

fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .take(80)
        .collect()
}

/// Build a `HashSet<String>` of local track base-names (stem) so the catalog
/// view can show which Amazon tracks are already present locally.
pub fn local_asin_set_from_filenames(paths: &[std::path::PathBuf]) -> HashSet<String> {
    // We don't have ASINs for local files, so this is a best-effort match by
    // "Artist - Title" stem (same format we use when downloading).
    paths
        .iter()
        .filter_map(|p| p.file_stem()?.to_str().map(str::to_owned))
        .collect()
}

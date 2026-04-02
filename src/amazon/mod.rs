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
    /// Non-fatal error (shown as status text in the UI).
    Error(String),
    /// Full diagnostic dump for errors that benefit from detailed inspection
    /// (bad HTTP status, unexpected content-type, parse failures, etc.).
    /// Displayed in a scrollable log pane so advanced users can diagnose issues.
    Diagnostic(ApiDiagnostic),
}

/// Complete record of a failed API exchange — no truncation.
#[derive(Debug, Clone)]
pub struct ApiDiagnostic {
    /// Which operation was attempted (e.g. "searchLibrary", "getTrackMetadata").
    pub operation: String,
    /// HTTP method + URL that was sent.
    pub request_line: String,
    /// Request headers that were sent, excluding the Cookie value (replaced
    /// with a length hint so the user knows it was present).
    pub request_headers: Vec<(String, String)>,
    /// HTTP status code returned.
    pub status: u16,
    /// All response headers received.
    pub response_headers: Vec<(String, String)>,
    /// Full response body — never truncated.
    pub body: String,
    /// Any additional context (e.g. which JSON path was missing).
    pub context: Option<String>,
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
    /// Purchase date string from Amazon (e.g. "2023-11-15").
    pub purchase_date: Option<String>,
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
            match self.fetch_page(offset, &inbox).await {
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

    /// Issue a POST to the cirrus endpoint, returning the raw response text and
    /// a pre-built `ApiDiagnostic` skeleton (headers filled in, body empty) so
    /// callers can attach the body and push `Diagnostic` on failure.
    async fn cirrus_post(
        &self,
        operation: &str,
        form_body: String,
    ) -> anyhow::Result<(u16, Vec<(String, String)>, String)> {
        let resp = self
            .http
            .post(CIRRUS_URL)
            .header("Cookie", &self.cookie)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Referer", "https://music.amazon.com/")
            .header("Origin", "https://music.amazon.com")
            .body(form_body)
            .send()
            .await
            .with_context(|| format!("POST cirrus/{operation}"))?;

        let status = resp.status().as_u16();
        let response_headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("<binary>").to_owned()))
            .collect();
        let body = resp.text().await.context("reading response body")?;

        Ok((status, response_headers, body))
    }

    fn make_diagnostic(
        &self,
        operation: &str,
        status: u16,
        response_headers: Vec<(String, String)>,
        body: String,
        context: Option<String>,
    ) -> ApiDiagnostic {
        let cookie_hint = format!("<cookie: {} bytes>", self.cookie.len());
        let request_headers = vec![
            ("Content-Type".into(), "application/x-www-form-urlencoded".into()),
            ("X-Requested-With".into(), "XMLHttpRequest".into()),
            ("Referer".into(), "https://music.amazon.com/".into()),
            ("Origin".into(), "https://music.amazon.com".into()),
            ("Cookie".into(), cookie_hint),
        ];
        ApiDiagnostic {
            operation: operation.to_owned(),
            request_line: format!("POST {CIRRUS_URL}"),
            request_headers,
            status,
            response_headers,
            body,
            context,
        }
    }

    async fn fetch_page(
        &self,
        offset: usize,
        inbox: &Arc<Mutex<Vec<AmazonMsg>>>,
    ) -> anyhow::Result<(Vec<AmazonTrack>, usize)> {
        let form_body = format!(
            "Operation=searchLibrary\
             &ContentType=JSON\
             &customerInfo.marketplaceId=ATVPDKIKX0DER\
             &searchCriteria.member.1.attributeName=assetType\
             &searchCriteria.member.1.comparisonType=EQUALS\
             &searchCriteria.member.1.attributeValue=PURCHASED\
             &selectCriteria.member.1=albumArtistName\
             &selectCriteria.member.2=albumName\
             &selectCriteria.member.3=asin\
             &selectCriteria.member.4=duration\
             &selectCriteria.member.5=title\
             &selectCriteria.member.6=primaryArtist\
             &selectCriteria.member.7=purchaseDate\
             &sortCriteriaList.member.1.sortColumn=purchaseDate\
             &sortCriteriaList.member.1.sortType=DESC\
             &maxResults={PAGE_SIZE}\
             &nextResultsToken={offset}"
        );

        let (status, resp_headers, body) =
            self.cirrus_post("searchLibrary", form_body).await?;

        // Any non-2xx response: emit full diagnostic + short error for the
        // status bar, then bail.
        if !(200..300).contains(&(status as u32)) {
            let content_type = resp_headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                .map(|(_, v)| v.as_str())
                .unwrap_or("unknown");
            let short = format!(
                "HTTP {status} ({content_type}) — see diagnostic log for full response"
            );
            push(inbox, AmazonMsg::Diagnostic(self.make_diagnostic(
                "searchLibrary",
                status,
                resp_headers,
                body,
                Some(format!("Expected HTTP 2xx, got {status}")),
            )));
            anyhow::bail!("{short}");
        }

        // Successful status but wrong content-type (e.g. HTML login redirect
        // that returns 200 with an HTML body) — also emit a diagnostic.
        let content_type = resp_headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();

        let v: serde_json::Value = serde_json::from_str(&body).map_err(|parse_err| {
            push(inbox, AmazonMsg::Diagnostic(self.make_diagnostic(
                "searchLibrary",
                status,
                resp_headers.clone(),
                body.clone(),
                Some(format!(
                    "JSON parse error: {parse_err}\n\
                     Content-Type: {content_type}\n\
                     (If the body looks like HTML, your cookie is likely expired or invalid.)"
                )),
            )));
            anyhow::anyhow!("JSON parse failed ({parse_err}) — Content-Type: {content_type}")
        })?;

        let result = &v["searchLibraryResponse"]["searchLibraryResult"];

        // Sanity-check: if the expected key is missing, the API shape changed.
        if result.is_null() {
            push(inbox, AmazonMsg::Diagnostic(self.make_diagnostic(
                "searchLibrary",
                status,
                resp_headers,
                body,
                Some("searchLibraryResponse.searchLibraryResult not found in JSON".into()),
            )));
            anyhow::bail!("unexpected API response shape — diagnostic emitted");
        }

        let total = result["totalResultSetSize"].as_u64().unwrap_or(0) as usize;

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
                let purchase_date = t["purchaseDate"]
                    .as_str()
                    .map(|s| s.chars().take(10).collect());
                Some(AmazonTrack {
                    asin, title, artist, album, duration_secs, purchase_date,
                    download_url: None,
                })
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
            None => match self.resolve_url(&track.asin, &inbox).await {
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

    async fn resolve_url(
        &self,
        asin: &str,
        inbox: &Arc<Mutex<Vec<AmazonMsg>>>,
    ) -> anyhow::Result<String> {
        let form_body = format!(
            "Operation=getTrackMetadata\
             &ContentType=JSON\
             &asin={asin}\
             &features=Preorder\
             &trackMetadataResponse.trackResponseType=DOWNLOAD"
        );

        let (status, resp_headers, body) =
            self.cirrus_post("getTrackMetadata", form_body).await?;

        if !(200..300).contains(&(status as u32)) {
            let content_type = resp_headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            push(inbox, AmazonMsg::Diagnostic(self.make_diagnostic(
                "getTrackMetadata",
                status,
                resp_headers,
                body,
                Some(format!("asin={asin}  Expected HTTP 2xx, got {status} ({content_type})")),
            )));
            anyhow::bail!("HTTP {status} resolving download URL for {asin}");
        }

        let content_type = resp_headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();

        let v: serde_json::Value = serde_json::from_str(&body).map_err(|parse_err| {
            push(inbox, AmazonMsg::Diagnostic(self.make_diagnostic(
                "getTrackMetadata",
                status,
                resp_headers.clone(),
                body.clone(),
                Some(format!(
                    "asin={asin}\nJSON parse error: {parse_err}\n\
                     Content-Type: {content_type}"
                )),
            )));
            anyhow::anyhow!("JSON parse failed for getTrackMetadata ({parse_err})")
        })?;

        v["getTrackMetadataResponse"]["trackInfo"]["streamUrls"]["streamUrl"][0]["url"]
            .as_str()
            .ok_or_else(|| {
                push(inbox, AmazonMsg::Diagnostic(self.make_diagnostic(
                    "getTrackMetadata",
                    status,
                    resp_headers,
                    body,
                    Some(format!(
                        "asin={asin}\n\
                         Path not found: getTrackMetadataResponse.trackInfo\
                         .streamUrls.streamUrl[0].url"
                    )),
                )));
                anyhow::anyhow!("no download URL in metadata response for {asin}")
            })
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

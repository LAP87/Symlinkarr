use std::path::{Component, Path};
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::api::http;
use crate::config::DecypharrConfig;

/// A torrent/content entry from Decypharr's browse API
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct DecypharrEntry {
    /// Entry name (torrent/folder name)
    pub name: String,
    /// Entry size in bytes
    #[serde(default)]
    pub size: i64,
    /// Whether entry is a directory
    #[serde(default)]
    pub is_dir: bool,
}

/// Repair schedule and last sweep as reported by `GET /api/repair/status`
/// (Decypharr >= 2.3). Every field defaults so newer builds can add keys freely.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RepairStatus {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub next_run_at: Option<String>,
    #[serde(default)]
    pub last_run: Option<RepairRun>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RepairRun {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub trigger: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub error: String,
}

/// Request body for `POST /api/repair/run` (Decypharr >= 2.3). The sweep is global
/// across every configured *Arr; `protocol` optionally limits it to "torrent" or "nzb".
#[derive(Debug, Serialize)]
struct RepairRunRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    protocol: Option<String>,
    verify_content: bool,
    auto_repair: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DecypharrArr {
    pub name: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub host: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImportRequest {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub error: String,
}

/// A queued or completed torrent entry from Decypharr's queue API.
#[derive(Debug, Clone, Deserialize)]
pub struct DecypharrTorrent {
    #[serde(alias = "hash")]
    pub info_hash: String,
    pub name: String,
    /// "torrent" or "nzb" (Decypharr >= 2.0); older builds omit it.
    #[allow(dead_code)]
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub progress: f64,
    #[serde(default)]
    pub is_complete: bool,
    #[serde(default)]
    pub bad: bool,
    #[serde(default)]
    pub category: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub mount_path: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub save_path: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub content_path: String,
    #[serde(default)]
    pub last_error: String,
    #[serde(default)]
    pub added_on: Option<DateTime<Utc>>,
    #[allow(dead_code)]
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
}

impl DecypharrTorrent {
    /// Usenet-backed entry (Decypharr >= 2.0 reports `protocol: "nzb"`).
    #[allow(dead_code)]
    pub fn is_nzb(&self) -> bool {
        self.protocol.eq_ignore_ascii_case("nzb")
    }

    pub fn is_failed(&self) -> bool {
        self.bad
            || self.state.eq_ignore_ascii_case("error")
            || self.status.eq_ignore_ascii_case("error")
            || (!self.last_error.trim().is_empty() && !self.is_complete)
    }

    pub fn failure_reason(&self) -> Option<&str> {
        if self.bad {
            Some("torrent marked bad")
        } else if self.state.eq_ignore_ascii_case("error") {
            Some("queue state=error")
        } else if self.status.eq_ignore_ascii_case("error") {
            Some("provider status=error")
        } else if !self.last_error.trim().is_empty() && !self.is_complete {
            Some(self.last_error.trim())
        } else {
            None
        }
    }
}

#[derive(Debug, Deserialize)]
struct TorrentListResponse {
    torrents: Vec<DecypharrTorrent>,
    #[serde(default)]
    total_pages: usize,
    #[serde(default)]
    has_next: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebDavProbeError {
    NotFound,
    Unreadable(String),
}

impl std::fmt::Display for WebDavProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "webdav probe path not found"),
            Self::Unreadable(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for WebDavProbeError {}

/// Client for Decypharr's web API (chi-based, typically port 8282)
pub struct DecypharrClient {
    client: Client,
    base_url: String,
    api_token: Option<String>,
    queue_page_size: usize,
}

impl DecypharrClient {
    #[allow(dead_code)]
    pub fn new(base_url: &str, api_token: Option<String>) -> Self {
        Self::with_queue_page_size(base_url, api_token, 100)
    }

    pub fn from_config(cfg: &DecypharrConfig) -> Self {
        Self::with_queue_page_size(&cfg.url, cfg.api_token.clone(), cfg.queue_page_size)
    }

    pub fn with_queue_page_size(
        base_url: &str,
        api_token: Option<String>,
        queue_page_size: usize,
    ) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            client: http::build_client(),
            base_url,
            api_token,
            queue_page_size,
        }
    }

    /// Build a request with optional auth header
    fn auth_header(&self) -> Option<(&str, String)> {
        self.api_token
            .as_ref()
            .map(|t| ("Authorization", format!("Bearer {}", t)))
    }

    fn build_webdav_url(&self, relative_path: &Path) -> Result<Url> {
        let mut url = Url::parse(&self.base_url)?;
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Decypharr base URL cannot be extended"))?;
        segments.pop_if_empty();
        segments.push("webdav");

        for component in relative_path.components() {
            match component {
                Component::Normal(segment) => {
                    let segment = segment.to_str().ok_or_else(|| {
                        anyhow::anyhow!("Decypharr WebDAV probe path contains non-UTF-8 segment")
                    })?;
                    segments.push(segment);
                }
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    anyhow::bail!(
                        "Decypharr WebDAV probe path must be relative and normalized: {}",
                        relative_path.display()
                    );
                }
            }
        }

        drop(segments);
        Ok(url)
    }

    /// How much longer the second attempt may take when the first one timed out. A cold
    /// debrid file needs a link generated and a CDN warm-up before its first byte (often
    /// several seconds); once served it answers in well under a second.
    const COLD_PROBE_RETRY_FACTOR: u32 = 4;

    pub async fn probe_webdav_path(
        &self,
        relative_path: &Path,
        timeout: Duration,
    ) -> std::result::Result<(), WebDavProbeError> {
        let url = self
            .build_webdav_url(relative_path)
            .map_err(|err| WebDavProbeError::Unreadable(err.to_string()))?;

        let send = |timeout: Duration| {
            let mut req = self
                .client
                .get(url.clone())
                .timeout(timeout)
                .header(reqwest::header::RANGE, "bytes=0-0");
            if let Some((key, val)) = self.auth_header() {
                req = req.header(key, val);
            }
            req.send()
        };

        let resp = match send(timeout).await {
            Ok(resp) => resp,
            Err(err) if err.is_timeout() => {
                let retry_timeout = timeout * Self::COLD_PROBE_RETRY_FACTOR;
                debug!(
                    "WebDAV probe of {} timed out after {:?}; retrying once with {:?} (cold file?)",
                    relative_path.display(),
                    timeout,
                    retry_timeout
                );
                send(retry_timeout).await.map_err(|err| {
                    WebDavProbeError::Unreadable(format!(
                        "webdav probe transport error after cold retry ({:?}): {}",
                        retry_timeout, err
                    ))
                })?
            }
            Err(err) => {
                return Err(WebDavProbeError::Unreadable(format!(
                    "webdav probe transport error: {}",
                    err
                )))
            }
        };

        match resp.status() {
            StatusCode::OK | StatusCode::PARTIAL_CONTENT => Ok(()),
            StatusCode::NOT_FOUND | StatusCode::GONE => Err(WebDavProbeError::NotFound),
            status => {
                let body = resp.text().await.unwrap_or_default();
                let detail = body.trim();
                Err(WebDavProbeError::Unreadable(if detail.is_empty() {
                    format!("webdav probe error {}", status)
                } else {
                    format!("webdav probe error {}: {}", status, detail)
                }))
            }
        }
    }

    /// Browse a content group on the Decypharr mount.
    /// Groups: "__all__", "__bad__", category names, etc.
    #[allow(dead_code)]
    pub async fn browse_group(&self, group: &str) -> Result<Vec<DecypharrEntry>> {
        let url = format!("{}/api/browse/{}", self.base_url, group);
        debug!("Decypharr: GET /api/browse/{}", group);

        let mut req = self.client.get(&url);
        if let Some((key, val)) = self.auth_header() {
            req = req.header(key, val);
        }

        let resp = http::send_with_retry(req).await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Decypharr browse error {}: {}", status, body);
        }

        let entries: Vec<DecypharrEntry> = resp.json().await?;
        debug!("Decypharr: {} entries in group '{}'", entries.len(), group);
        Ok(entries)
    }

    /// Start a repair sweep (`POST /api/repair/run`, Decypharr >= 2.3). The sweep is
    /// global across every configured *Arr; `protocol` optionally limits it to
    /// "torrent" or "nzb". A 409 means a sweep is already running and is reported as
    /// success, since the caller's intent is satisfied either way.
    pub async fn trigger_repair(
        &self,
        protocol: Option<&str>,
        verify_content: bool,
        auto_repair: bool,
    ) -> Result<String> {
        let url = format!("{}/api/repair/run", self.base_url);
        info!(
            "Decypharr: POST /api/repair/run (protocol={:?}, verify_content={}, auto_repair={})",
            protocol, verify_content, auto_repair
        );

        let body = RepairRunRequest {
            protocol: protocol.map(|s| s.to_string()),
            verify_content,
            auto_repair,
        };

        let mut req = self.client.post(&url).json(&body);
        if let Some((key, val)) = self.auth_header() {
            req = req.header(key, val);
        }

        let resp = http::send_with_retry(req).await?;
        let status = resp.status();
        if status == reqwest::StatusCode::CONFLICT {
            let message = "a repair sweep is already running".to_string();
            info!("Decypharr: {}", message);
            return Ok(message);
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!(
                "Decypharr returned 404 for /api/repair/run; the repair API needs Decypharr 2.3 or newer"
            );
        }
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Decypharr repair error {}: {}", status, body_text);
        }

        // The response shape is not pinned across builds; surface whatever identifies the run.
        let value: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        let message = value
            .get("message")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                value
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|id| format!("repair run {id} started"))
            })
            .unwrap_or_else(|| "repair run accepted".to_string());
        info!("Decypharr: {}", message);
        Ok(message)
    }

    /// Current repair schedule and last sweep (`GET /api/repair/status`, Decypharr >= 2.3).
    pub async fn get_repair_status(&self) -> Result<RepairStatus> {
        let url = format!("{}/api/repair/status", self.base_url);
        debug!("Decypharr: GET /api/repair/status");

        let mut req = self.client.get(&url);
        if let Some((key, val)) = self.auth_header() {
            req = req.header(key, val);
        }

        let resp = http::send_with_retry(req).await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Decypharr repair status error {}: {}", status, body);
        }

        Ok(resp.json().await?)
    }

    /// Add content (magnet links) via Decypharr's API.
    pub async fn add_content(
        &self,
        urls: &[String],
        arr_name: &str,
        action: &str,
    ) -> Result<Vec<ImportRequest>> {
        let url = format!("{}/api/add", self.base_url);
        info!(
            "Decypharr: POST /api/add ({} URLs, arr={})",
            urls.len(),
            arr_name
        );

        let form = reqwest::multipart::Form::new()
            .text("urls", urls.join("\n"))
            .text("arr", arr_name.to_string())
            .text("action", action.to_string());

        let mut req = self.client.post(&url).multipart(form);
        if let Some((key, val)) = self.auth_header() {
            req = req.header(key, val);
        }

        let resp = http::send_with_retry(req).await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Decypharr add error {}: {}", status, body);
        }

        let imports: Vec<ImportRequest> = resp.json().await?;
        if let Some(failed) = imports
            .iter()
            .find(|import| import.status.eq_ignore_ascii_case("error"))
        {
            anyhow::bail!(
                "Decypharr rejected content{}{}",
                if failed.id.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", failed.id)
                },
                if failed.error.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", failed.error.trim())
                }
            );
        }

        info!("Decypharr: Content added successfully");
        Ok(imports)
    }

    /// List torrents from Decypharr's queue API.
    pub async fn list_torrents(
        &self,
        category: Option<&str>,
        hash: Option<&str>,
    ) -> Result<Vec<DecypharrTorrent>> {
        let url = format!("{}/api/torrents", self.base_url);
        let mut page = 1usize;
        let mut all = Vec::new();

        loop {
            let mut req = self
                .client
                .get(&url)
                .query(&[("page", page), ("limit", self.queue_page_size)]);
            if let Some(category) = category {
                req = req.query(&[("category", category)]);
            }
            if let Some(hash) = hash {
                req = req.query(&[("search", hash)]);
            }
            if let Some((key, val)) = self.auth_header() {
                req = req.header(key, val);
            }

            let resp = http::send_with_retry(req).await?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("Decypharr torrents error {}: {}", status, body);
            }

            let page_data: TorrentListResponse = resp.json().await?;
            let has_next =
                page_data.has_next || (page_data.total_pages > 0 && page < page_data.total_pages);
            all.extend(page_data.torrents);
            if !has_next {
                break;
            }
            page += 1;
        }

        Ok(all)
    }

    /// List Arr instances known to Decypharr.
    pub async fn get_arrs(&self) -> Result<Vec<DecypharrArr>> {
        let url = format!("{}/api/arrs", self.base_url);
        let mut req = self.client.get(&url);
        if let Some((key, val)) = self.auth_header() {
            req = req.header(key, val);
        }

        let resp = http::send_with_retry(req).await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Decypharr arrs error {}: {}", status, body);
        }

        Ok(resp.json().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_helpers::spawn_sequence_http_server;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_parse_browse_entry() {
        let json = r#"[
            {"name": "Breaking.Bad.S01.1080p", "size": 5368709120, "is_dir": true},
            {"name": "The.Matrix.1999.2160p.mkv", "size": 21474836480, "is_dir": false}
        ]"#;

        let entries: Vec<DecypharrEntry> = serde_json::from_str(json).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "Breaking.Bad.S01.1080p");
        assert!(entries[0].is_dir);
        assert!(!entries[1].is_dir);
    }

    #[test]
    fn test_parse_repair_status() {
        let json = r#"{
            "enabled": true,
            "next_run_at": "2026-09-05T04:00:00+02:00",
            "last_run": {"id": "a760adda", "trigger": "scheduled", "status": "completed"}
        }"#;

        let status: RepairStatus = serde_json::from_str(json).unwrap();
        assert!(status.enabled);
        assert_eq!(
            status.next_run_at.as_deref(),
            Some("2026-09-05T04:00:00+02:00")
        );
        let run = status.last_run.unwrap();
        assert_eq!(run.id, "a760adda");
        assert_eq!(run.status, "completed");

        // Older or newer builds may omit keys entirely.
        let sparse: RepairStatus = serde_json::from_str("{}").unwrap();
        assert!(!sparse.enabled);
        assert!(sparse.last_run.is_none());
    }

    #[test]
    fn test_trigger_repair_posts_to_v2_run_endpoint() {
        let Some((base_url, requests)) = spawn_sequence_http_server(&[(
            "HTTP/1.1 200 OK",
            r#"{"id":"run-1","message":"Repair run started"}"#,
        )]) else {
            return;
        };
        let client = DecypharrClient::new(&base_url, None);
        let message = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(client.trigger_repair(Some("nzb"), false, true))
            .unwrap();
        assert_eq!(message, "Repair run started");

        let captured = requests.lock().unwrap();
        let request = captured.first().unwrap();
        assert!(request.starts_with("POST /api/repair/run HTTP/1.1"));
        assert!(request.contains(r#""protocol":"nzb""#));
        assert!(request.contains(r#""auto_repair":true"#));
    }

    #[test]
    fn test_trigger_repair_treats_409_as_already_running() {
        let Some((base_url, _requests)) =
            spawn_sequence_http_server(&[("HTTP/1.1 409 Conflict", r#"{"error":"running"}"#)])
        else {
            return;
        };
        let client = DecypharrClient::new(&base_url, None);
        let message = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(client.trigger_repair(None, false, true))
            .unwrap();
        assert!(message.contains("already running"));
    }

    #[test]
    fn test_torrent_protocol_defaults_and_is_nzb() {
        let nzb: DecypharrTorrent =
            serde_json::from_str(r#"{"info_hash":"x","name":"n","protocol":"nzb"}"#).unwrap();
        assert!(nzb.is_nzb());
        let legacy: DecypharrTorrent =
            serde_json::from_str(r#"{"info_hash":"x","name":"n"}"#).unwrap();
        assert!(!legacy.is_nzb());
        assert_eq!(legacy.protocol, "");
    }

    #[test]
    fn test_parse_torrent_page() {
        let json = r#"{
            "torrents": [{
                "info_hash": "ABC123",
                "name": "Breaking Bad S01E01",
                "state": "downloading",
                "status": "downloading",
                "progress": 42.0,
                "is_complete": false,
                "bad": false,
                "category": "sonarr",
                "last_error": ""
            }],
            "total_pages": 1,
            "has_next": false
        }"#;

        let page: TorrentListResponse = serde_json::from_str(json).unwrap();
        assert_eq!(page.torrents.len(), 1);
        assert_eq!(page.torrents[0].info_hash, "ABC123");
        assert_eq!(page.torrents[0].category, "sonarr");
        assert!(!page.torrents[0].is_failed());
    }

    #[test]
    fn test_failed_torrent_detection_uses_error_signals() {
        let torrent: DecypharrTorrent = serde_json::from_str(
            r#"{
                "info_hash": "DEF456",
                "name": "Broken Torrent",
                "state": "error",
                "status": "error",
                "is_complete": false,
                "bad": true,
                "last_error": "slot full"
            }"#,
        )
        .unwrap();

        assert!(torrent.is_failed());
        assert_eq!(torrent.failure_reason(), Some("torrent marked bad"));
    }

    #[tokio::test]
    async fn probe_webdav_path_encodes_segments_and_sends_range() {
        let (base_url, requests) =
            spawn_sequence_http_server(&[("HTTP/1.1 206 Partial Content", "x")]).unwrap();
        let client = DecypharrClient::new(&base_url, None);

        client
            .probe_webdav_path(
                Path::new(
                    "__all__/12.Monkeys.S02E13.720p.HDTV.x264-AVS[rarbg]/12.Monkeys.S02E13.720p.HDTV.x264-AVS.mkv",
                ),
                Duration::from_millis(500),
            )
            .await
            .unwrap();

        let captured = requests.lock().unwrap();
        let request = captured.first().unwrap();
        assert!(request.contains("GET /webdav/__all__/"));
        assert!(request.contains("12.Monkeys.S02E13.720p.HDTV.x264-AVS"));
        assert!(request.contains("12.Monkeys.S02E13.720p.HDTV.x264-AVS.mkv HTTP/1.1"));
        assert!(request.to_ascii_lowercase().contains("range: bytes=0-0"));
    }

    #[tokio::test]
    async fn probe_webdav_path_retries_once_with_a_longer_timeout_for_cold_files() {
        // First byte arrives after 120 ms: too slow for the 50 ms probe, fine for the
        // 200 ms cold retry.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for _ in 0..2 {
                if let Ok((mut stream, _)) = listener.accept() {
                    let mut req_buf = [0u8; 1024];
                    let _ = stream.read(&mut req_buf);
                    thread::sleep(Duration::from_millis(120));
                    let _ = stream.write_all(
                        b"HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx",
                    );
                }
            }
        });

        let client = DecypharrClient::new(&format!("http://{}", addr), None);
        client
            .probe_webdav_path(
                Path::new("__all__/cold/file.mkv"),
                Duration::from_millis(50),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn probe_webdav_path_times_out_when_server_stalls() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        thread::spawn(move || {
            // Stall both the probe and its cold retry.
            for _ in 0..2 {
                if let Ok((mut stream, _)) = listener.accept() {
                    let mut req_buf = [0u8; 1024];
                    let _ = stream.read(&mut req_buf);
                    thread::sleep(Duration::from_millis(400));
                    let _ = stream.write_all(
                        b"HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx",
                    );
                }
            }
        });

        let client = DecypharrClient::new(&format!("http://{}", addr), None);
        let err = client
            .probe_webdav_path(
                Path::new("__all__/slow/file.mkv"),
                Duration::from_millis(50),
            )
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("webdav probe transport error"),
            "unexpected error: {err}"
        );
    }
}

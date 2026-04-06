use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Client;
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

/// Repair job status from Decypharr
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct RepairJob {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub arrs: Vec<String>,
    #[serde(default)]
    pub error: String,
}

/// Request body for triggering a repair
#[derive(Debug, Serialize)]
struct RepairRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    arr_name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    media_ids: Vec<String>,
    auto_process: bool,
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

    /// Trigger a repair job in Decypharr.
    /// If `arr_name` is None, repairs all configured *Arrs.
    pub async fn trigger_repair(
        &self,
        arr_name: Option<&str>,
        media_ids: Vec<String>,
        auto_process: bool,
    ) -> Result<String> {
        let url = format!("{}/api/repair", self.base_url);
        info!(
            "Decypharr: POST /api/repair (arr={:?}, ids={:?})",
            arr_name, media_ids
        );

        let body = RepairRequest {
            arr_name: arr_name.map(|s| s.to_string()),
            media_ids,
            auto_process,
        };

        let mut req = self.client.post(&url).json(&body);
        if let Some((key, val)) = self.auth_header() {
            req = req.header(key, val);
        }

        let resp = http::send_with_retry(req).await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Decypharr repair error {}: {}", status, body_text);
        }

        #[derive(Deserialize)]
        struct RepairResponse {
            message: String,
        }

        let result: RepairResponse = resp.json().await?;
        info!("Decypharr: {}", result.message);
        Ok(result.message)
    }

    /// Get all repair jobs from Decypharr.
    #[allow(dead_code)]
    pub async fn get_repair_jobs(&self) -> Result<Vec<RepairJob>> {
        let url = format!("{}/api/repair/jobs", self.base_url);
        debug!("Decypharr: GET /api/repair/jobs");

        let mut req = self.client.get(&url);
        if let Some((key, val)) = self.auth_header() {
            req = req.header(key, val);
        }

        let resp = http::send_with_retry(req).await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Decypharr jobs error {}: {}", status, body);
        }

        let jobs: Vec<RepairJob> = resp.json().await?;
        Ok(jobs)
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
    fn test_parse_repair_job() {
        let json = r#"[{
            "id": "job-123",
            "status": "completed",
            "arrs": ["sonarr"],
            "error": ""
        }]"#;

        let jobs: Vec<RepairJob> = serde_json::from_str(json).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, "completed");
        assert_eq!(jobs[0].arrs, vec!["sonarr"]);
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
}

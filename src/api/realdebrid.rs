use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::api::http;
use crate::config::RealDebridConfig;

const RD_BASE_URL: &str = "https://api.real-debrid.com/rest/1.0";

/// A torrent entry from Real-Debrid's torrent list
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RdTorrent {
    /// Real-Debrid torrent ID
    pub id: String,
    /// Torrent filename / title
    pub filename: String,
    /// Hash of the torrent
    pub hash: String,
    /// Size in bytes
    pub bytes: i64,
    /// Status: "downloaded", "magnet_error", "waiting_files_selection", etc.
    pub status: String,
    /// Number of links generated
    #[serde(default)]
    pub links: Vec<String>,
}

/// Detailed info for a single torrent, including file list
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RdTorrentInfo {
    pub id: String,
    pub filename: String,
    pub hash: String,
    pub bytes: i64,
    pub status: String,
    #[serde(default)]
    pub files: Vec<RdFile>,
    #[serde(default)]
    pub links: Vec<String>,
}

/// A file within an RD torrent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RdFile {
    /// File ID within the torrent
    pub id: u64,
    /// Relative path within the torrent
    pub path: String,
    /// File size in bytes
    pub bytes: i64,
    /// Whether this file was selected for download (1 = yes)
    pub selected: i32,
}

/// Client for the Real-Debrid REST API
pub struct RealDebridClient {
    client: Client,
    api_token: String,
    torrents_page_limit: u32,
    pagination_delay_ms: u64,
    max_pages: u32,
}

impl RealDebridClient {
    #[allow(dead_code)]
    pub fn new(api_token: &str) -> Self {
        Self::with_settings(api_token, 5000, 200, 5000)
    }

    pub fn from_config(cfg: &RealDebridConfig) -> Self {
        Self::with_settings(
            &cfg.api_token,
            cfg.torrents_page_limit,
            cfg.pagination_delay_ms,
            cfg.max_pages,
        )
    }

    pub fn with_settings(
        api_token: &str,
        torrents_page_limit: u32,
        pagination_delay_ms: u64,
        max_pages: u32,
    ) -> Self {
        Self {
            client: http::build_client(),
            api_token: api_token.to_string(),
            torrents_page_limit,
            pagination_delay_ms,
            max_pages,
        }
    }

    /// List all torrents in the user's RD account.
    /// Returns up to `limit` results, paginated by `page`.
    pub async fn list_torrents(&self, page: u32, limit: u32) -> Result<Vec<RdTorrent>> {
        let url = format!("{}/torrents", RD_BASE_URL);
        debug!("RD API: GET /torrents (page={}, limit={})", page, limit);

        let req = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_token))
            .query(&[("page", page.to_string()), ("limit", limit.to_string())]);
        let resp = http::send_with_retry(req).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("RD API error {}: {}", status, body);
        }

        let torrents: Vec<RdTorrent> = resp.json().await?;
        debug!("RD API: Got {} torrents", torrents.len());
        Ok(torrents)
    }

    /// Fetch all torrents by paginating through the full list.
    pub async fn list_all_torrents(&self) -> Result<Vec<RdTorrent>> {
        let mut all = Vec::new();
        let mut page = 1u32;
        let limit = self.torrents_page_limit;

        loop {
            let batch = self.list_torrents(page, limit).await?;
            let count = batch.len();
            all.extend(batch);

            info!(
                "RD Sync: Page {} complete. Total torrents fetched: {}...",
                page,
                all.len()
            );

            if (count as u32) < limit {
                break;
            }
            page += 1;

            // Be gentle on the API during pagination
            tokio::time::sleep(std::time::Duration::from_millis(self.pagination_delay_ms)).await;

            // Safety valve: don't fetch more than the configured maximum page count.
            if page > self.max_pages {
                warn!(
                    "RD API: Stopping pagination at page {} (safety limit)",
                    self.max_pages
                );
                break;
            }
        }

        info!("RD API: Fetched {} total torrents", all.len());
        Ok(all)
    }

    /// Get detailed info for a specific torrent, including its file list.
    pub async fn get_torrent_info(&self, torrent_id: &str) -> Result<RdTorrentInfo> {
        let url = format!("{}/torrents/info/{}", RD_BASE_URL, torrent_id);
        debug!("RD API: GET /torrents/info/{}", torrent_id);

        let req = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_token));
        let resp = http::send_with_retry(req).await?;

        if resp.status().is_success() {
            let info: RdTorrentInfo = resp.json().await?;
            Ok(info)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("RD API error {}: {}", status, body);
        }
    }

    /// Add a magnet link to Real-Debrid.
    #[allow(dead_code)]
    pub async fn add_magnet(&self, magnet: &str) -> Result<String> {
        let url = format!("{}/torrents/addMagnet", RD_BASE_URL);
        debug!("RD API: POST /torrents/addMagnet");

        let req = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_token))
            .header(
                "Idempotency-Key",
                http::stable_idempotency_key("rd-add-magnet", magnet),
            )
            .form(&[("magnet", magnet)]);
        let resp = http::send_with_retry(req).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("RD API addMagnet error {}: {}", status, body);
        }

        #[derive(Deserialize)]
        struct AddResponse {
            id: String,
        }

        let result: AddResponse = resp.json().await?;
        info!("RD API: Added magnet, torrent id={}", result.id);
        Ok(result.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rd_torrent_json() {
        let json = r#"[{
            "id": "ABC123",
            "filename": "Breaking.Bad.S01.1080p.BluRay",
            "hash": "deadbeef1234",
            "bytes": 5368709120,
            "status": "downloaded",
            "links": ["https://real-debrid.com/d/abc"]
        }]"#;

        let torrents: Vec<RdTorrent> = serde_json::from_str(json).unwrap();
        assert_eq!(torrents.len(), 1);
        assert_eq!(torrents[0].id, "ABC123");
        assert_eq!(torrents[0].filename, "Breaking.Bad.S01.1080p.BluRay");
        assert_eq!(torrents[0].status, "downloaded");
    }

    #[test]
    fn test_parse_rd_torrent_info_json() {
        let json = r#"{
            "id": "ABC123",
            "filename": "Breaking.Bad.S01.1080p.BluRay",
            "hash": "deadbeef1234",
            "bytes": 5368709120,
            "status": "downloaded",
            "files": [{
                "id": 1,
                "path": "/Breaking.Bad.S01E01.1080p.mkv",
                "bytes": 1073741824,
                "selected": 1
            }],
            "links": ["https://real-debrid.com/d/abc"]
        }"#;

        let info: RdTorrentInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.files.len(), 1);
        assert_eq!(info.files[0].path, "/Breaking.Bad.S01E01.1080p.mkv");
        assert_eq!(info.files[0].selected, 1);
    }
}

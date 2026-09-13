use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use crate::api::http;

pub struct SonarrClient {
    client: Client,
    base_url: String,
    api_key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SonarrSeries {
    pub id: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub path: String,
    #[serde(default, rename = "alternateTitles")]
    pub alternate_titles: Vec<SonarrAlternateTitle>,
    #[serde(default, rename = "tvdbId")]
    pub tvdb_id: i64,
    #[serde(default, rename = "tmdbId")]
    pub tmdb_id: i64,
    #[serde(default, rename = "imdbId")]
    pub imdb_id: String,
    #[serde(default)]
    pub monitored: bool,
    #[serde(default)]
    pub statistics: Option<SonarrSeriesStatistics>,
    #[serde(default, rename = "useSceneNumbering")]
    pub use_scene_numbering: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SonarrSeriesStatistics {
    #[serde(default, rename = "episodeFileCount")]
    pub episode_file_count: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SonarrAlternateTitle {
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "sceneSeasonNumber")]
    pub scene_season_number: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SonarrEpisode {
    #[allow(dead_code)]
    pub id: i64,
    #[allow(dead_code)]
    #[serde(default, rename = "seriesId")]
    pub series_id: i64,
    #[serde(default, rename = "seasonNumber")]
    pub season_number: u32,
    #[serde(default, rename = "episodeNumber")]
    pub episode_number: u32,
    #[serde(default)]
    pub title: String,
    #[allow(dead_code)]
    #[serde(default, rename = "absoluteEpisodeNumber")]
    pub absolute_episode_number: Option<u32>,
    #[allow(dead_code)]
    #[serde(default, rename = "sceneSeasonNumber")]
    pub scene_season_number: Option<u32>,
    #[allow(dead_code)]
    #[serde(default, rename = "sceneEpisodeNumber")]
    pub scene_episode_number: Option<u32>,
    #[allow(dead_code)]
    #[serde(default, rename = "sceneAbsoluteEpisodeNumber")]
    pub scene_absolute_episode_number: Option<u32>,
    #[serde(default, rename = "episodeFileId")]
    pub episode_file_id: Option<i64>,
    #[serde(default, rename = "hasFile")]
    pub has_file: bool,
    #[serde(default, rename = "airDateUtc")]
    pub air_date_utc: Option<DateTime<Utc>>,
    #[serde(default)]
    pub monitored: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SonarrWantedMissingPage {
    #[allow(dead_code)]
    #[serde(default)]
    pub page: u32,
    #[allow(dead_code)]
    #[serde(default, rename = "pageSize")]
    pub page_size: u32,
    #[serde(default, rename = "totalRecords")]
    pub total_records: u32,
    #[serde(default)]
    pub records: Vec<SonarrWantedMissingRecord>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SonarrWantedMissingRecord {
    #[serde(default, rename = "seriesId")]
    pub series_id: i64,
    #[allow(dead_code)]
    #[serde(default, rename = "tvdbId")]
    pub tvdb_id: i64,
    #[serde(default, rename = "seasonNumber")]
    pub season_number: u32,
    #[serde(default, rename = "episodeNumber")]
    pub episode_number: u32,
    #[serde(default, rename = "absoluteEpisodeNumber")]
    pub absolute_episode_number: Option<u32>,
    #[serde(default, rename = "sceneSeasonNumber")]
    pub scene_season_number: Option<u32>,
    #[serde(default, rename = "sceneEpisodeNumber")]
    pub scene_episode_number: Option<u32>,
    #[serde(default, rename = "sceneAbsoluteEpisodeNumber")]
    pub scene_absolute_episode_number: Option<u32>,
    #[allow(dead_code)]
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "hasFile")]
    pub has_file: bool,
    #[allow(dead_code)]
    #[serde(default, rename = "episodeFileId")]
    pub episode_file_id: Option<i64>,
    #[serde(default, rename = "airDateUtc")]
    pub air_date_utc: Option<DateTime<Utc>>,
    #[serde(default)]
    pub monitored: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct SonarrEpisodeFile {
    pub id: i64,
    #[serde(default)]
    pub path: String,
    #[serde(default, rename = "relativePath")]
    pub relative_path: String,
}

impl SonarrClient {
    pub fn new(url: &str, api_key: &str) -> Self {
        Self {
            client: http::build_client(),
            base_url: url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }

    pub async fn get_system_status(&self) -> Result<()> {
        crate::api::http::check_system_status(
            &self.client,
            &self.base_url,
            &self.api_key,
            "v3",
            "Sonarr",
        )
        .await
    }

    pub async fn get_series(&self) -> Result<Vec<SonarrSeries>> {
        let url = format!("{}/api/v3/series", self.base_url);
        let req = self.client.get(&url).header("X-Api-Key", &self.api_key);
        let resp = http::send_with_retry(req).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Sonarr series lookup failed (HTTP {}): {}", status, body);
        }

        Ok(resp.json::<Vec<SonarrSeries>>().await?)
    }

    pub async fn get_episodes_for_series(&self, series_id: i64) -> Result<Vec<SonarrEpisode>> {
        let url = format!("{}/api/v3/episode", self.base_url);
        let req = self
            .client
            .get(&url)
            .query(&[("seriesId", series_id)])
            .header("X-Api-Key", &self.api_key);
        let resp = http::send_with_retry(req).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Sonarr get_episodes_for_series error {}: {}", status, body);
        }

        Ok(resp.json::<Vec<SonarrEpisode>>().await?)
    }

    #[allow(dead_code)]
    pub async fn get_episode_file(&self, file_id: i64) -> Result<Option<SonarrEpisodeFile>> {
        if file_id <= 0 {
            return Ok(None);
        }

        let url = format!("{}/api/v3/episodefile/{}", self.base_url, file_id);
        let req = self.client.get(&url).header("X-Api-Key", &self.api_key);
        let resp = http::send_with_retry(req).await?;

        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Sonarr get_episode_file error {}: {}", status, body);
        }

        Ok(Some(resp.json::<SonarrEpisodeFile>().await?))
    }

    pub async fn get_wanted_missing_page(
        &self,
        page: u32,
        page_size: u32,
    ) -> Result<SonarrWantedMissingPage> {
        let url = format!("{}/api/v3/wanted/missing", self.base_url);
        let req = self
            .client
            .get(&url)
            .query(&[("page", page), ("pageSize", page_size)])
            .header("X-Api-Key", &self.api_key);
        let req = req.query(&[
            ("sortKey", "episodes.airDateUtc"),
            ("sortDirection", "descending"),
        ]);
        let resp = http::send_with_retry(req).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Sonarr get_wanted_missing_page error {}: {}", status, body);
        }

        Ok(resp.json::<SonarrWantedMissingPage>().await?)
    }

    pub async fn get_wanted_cutoff_page(
        &self,
        page: u32,
        page_size: u32,
    ) -> Result<SonarrWantedMissingPage> {
        let url = format!("{}/api/v3/wanted/cutoff", self.base_url);
        let req = self
            .client
            .get(&url)
            .query(&[("page", page), ("pageSize", page_size)])
            .header("X-Api-Key", &self.api_key);
        let req = req.query(&[
            ("sortKey", "episodes.airDateUtc"),
            ("sortDirection", "descending"),
        ]);
        let resp = http::send_with_retry(req).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Sonarr get_wanted_cutoff_page error {}: {}", status, body);
        }

        Ok(resp.json::<SonarrWantedMissingPage>().await?)
    }

    /// Ask Sonarr to rescan one item on disk (or everything when `series_id` is None) so a
    /// freshly written symlink shows up without waiting for its periodic disk scan.
    pub async fn rescan_series(&self, series_id: Option<i64>) -> Result<()> {
        let url = format!("{}/api/v3/command", self.base_url);
        let mut body = serde_json::json!({ "name": "RescanSeries" });
        if let Some(id) = series_id {
            body["seriesId"] = serde_json::json!(id);
        }
        let req = self
            .client
            .post(&url)
            .header("X-Api-Key", &self.api_key)
            .json(&body);
        let resp = http::send_with_retry(req).await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Sonarr RescanSeries command failed (HTTP {}): {}",
                status,
                text
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod rescan_tests {
    use super::*;
    use crate::api::test_helpers::spawn_sequence_http_server;

    #[test]
    fn rescan_series_posts_a_rescanseries_command() {
        let Some((base_url, requests)) =
            spawn_sequence_http_server(&[("HTTP/1.1 201 Created", r#"{"id":1}"#)])
        else {
            return;
        };
        let client = SonarrClient::new(&base_url, "secret-key");
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(client.rescan_series(Some(42)))
            .unwrap();
        let captured = requests.lock().unwrap();
        let request = captured.first().unwrap();
        assert!(request.starts_with("POST /api/v3/command HTTP/1.1"));
        assert!(request.to_lowercase().contains("x-api-key: secret-key"));
        assert!(request.contains(r#""name":"RescanSeries""#));
        assert!(request.contains(r#""seriesId":42"#));
    }
}

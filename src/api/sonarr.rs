#![allow(dead_code)] // New cleanup audit helpers are phased in incrementally

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
    #[serde(default, rename = "alternateTitles")]
    pub alternate_titles: Vec<SonarrAlternateTitle>,
    #[serde(default, rename = "tvdbId")]
    pub tvdb_id: i64,
    #[serde(default, rename = "tmdbId")]
    pub tmdb_id: i64,
    #[serde(default, rename = "useSceneNumbering")]
    pub use_scene_numbering: bool,
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
    pub id: i64,
    #[serde(default, rename = "seriesId")]
    pub series_id: i64,
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
    #[serde(default, rename = "episodeFileId")]
    pub episode_file_id: Option<i64>,
    #[serde(default, rename = "hasFile")]
    pub has_file: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SonarrWantedMissingPage {
    #[serde(default)]
    pub page: u32,
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
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "hasFile")]
    pub has_file: bool,
    #[serde(default, rename = "episodeFileId")]
    pub episode_file_id: Option<i64>,
    #[serde(default, rename = "airDateUtc")]
    pub air_date_utc: Option<DateTime<Utc>>,
    #[serde(default)]
    pub monitored: bool,
}

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
            anyhow::bail!("Sonarr get_series error {}: {}", status, body);
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
}

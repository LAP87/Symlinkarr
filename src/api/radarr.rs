use anyhow::Result;
use reqwest::Client;
use serde::Deserialize;

use crate::api::http;

pub struct RadarrClient {
    client: Client,
    base_url: String,
    api_key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RadarrMovie {
    pub title: String,
    #[serde(default)]
    pub path: String,
    #[serde(default, rename = "tmdbId")]
    pub tmdb_id: i64,
    #[serde(default, rename = "imdbId")]
    pub imdb_id: String,
    #[serde(default)]
    pub year: u32,
    #[serde(default)]
    pub monitored: bool,
    #[serde(default, rename = "hasFile")]
    pub has_file: bool,
    #[serde(default, rename = "movieFileId")]
    pub movie_file_id: Option<i64>,
}

impl RadarrClient {
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
            "Radarr",
        )
        .await
    }

    pub async fn get_movies(&self) -> Result<Vec<RadarrMovie>> {
        let url = format!("{}/api/v3/movie", self.base_url);
        let req = self.client.get(&url).header("X-Api-Key", &self.api_key);
        let resp = http::send_with_retry(req).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Radarr movie lookup failed (HTTP {}): {}", status, body);
        }

        Ok(resp.json::<Vec<RadarrMovie>>().await?)
    }
}

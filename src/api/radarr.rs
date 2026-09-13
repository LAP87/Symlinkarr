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
    #[serde(default)]
    pub id: i64,
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

    /// Ask Radarr to rescan one item on disk (or everything when `movie_id` is None) so a
    /// freshly written symlink shows up without waiting for its periodic disk scan.
    pub async fn rescan_movie(&self, movie_id: Option<i64>) -> Result<()> {
        let url = format!("{}/api/v3/command", self.base_url);
        let mut body = serde_json::json!({ "name": "RescanMovie" });
        if let Some(id) = movie_id {
            body["movieId"] = serde_json::json!(id);
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
                "Radarr RescanMovie command failed (HTTP {}): {}",
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
    fn rescan_movie_posts_a_rescanmovie_command() {
        let Some((base_url, requests)) =
            spawn_sequence_http_server(&[("HTTP/1.1 201 Created", r#"{"id":1}"#)])
        else {
            return;
        };
        let client = RadarrClient::new(&base_url, "secret-key");
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(client.rescan_movie(Some(42)))
            .unwrap();
        let captured = requests.lock().unwrap();
        let request = captured.first().unwrap();
        assert!(request.starts_with("POST /api/v3/command HTTP/1.1"));
        assert!(request.to_lowercase().contains("x-api-key: secret-key"));
        assert!(request.contains(r#""name":"RescanMovie""#));
        assert!(request.contains(r#""movieId":42"#));
    }
}

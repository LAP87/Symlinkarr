#![allow(dead_code)] // Module scaffolded for future Bazarr integration

use anyhow::Result;
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::api::http;
use crate::config::BazarrConfig;

// ─── Response types ─────────────────────────────────────────────────

/// Episode subtitle status from Bazarr
#[derive(Debug, Clone, Deserialize)]
pub struct BazarrEpisode {
    #[serde(default)]
    pub sonarr_episode_file_id: Option<i64>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub missing_subtitles: Vec<BazarrLanguage>,
}

/// Movie subtitle status from Bazarr
#[derive(Debug, Clone, Deserialize)]
pub struct BazarrMovie {
    #[serde(default)]
    pub radarr_id: Option<i64>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub missing_subtitles: Vec<BazarrLanguage>,
}

/// Language entry from Bazarr
#[derive(Debug, Clone, Deserialize)]
pub struct BazarrLanguage {
    #[serde(default)]
    pub code2: String,
    #[serde(default)]
    pub name: String,
}

// ─── Client ─────────────────────────────────────────────────────────

/// Client for Bazarr's REST API
pub struct BazarrClient {
    client: Client,
    base_url: String,
    api_key: String,
}

impl BazarrClient {
    pub fn new(config: &BazarrConfig) -> Self {
        let base_url = config.url.trim_end_matches('/').to_string();
        Self {
            client: http::build_client(),
            base_url,
            api_key: config.api_key.clone(),
        }
    }

    fn endpoint_url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    fn with_query_api_key(&self, url: &str) -> String {
        if url.contains('?') {
            format!("{}&apikey={}", url, self.api_key)
        } else {
            format!("{}?apikey={}", url, self.api_key)
        }
    }

    async fn send_authenticated<F>(&self, url: &str, build: F) -> Result<Response>
    where
        F: Fn(&str) -> RequestBuilder,
    {
        let resp = http::send_with_retry(build(url).header("X-Api-Key", &self.api_key)).await?;

        if should_retry_with_query_auth(resp.status()) {
            debug!(
                "Bazarr: header auth was not accepted (status={}), retrying with apikey query parameter",
                resp.status()
            );
            return http::send_with_retry(build(&self.with_query_api_key(url))).await;
        }

        Ok(resp)
    }

    /// Trigger a subtitle search for a series episode via the Sonarr webhook.
    /// This mimics Sonarr's "Download" event to make Bazarr re-scan for subtitles.
    pub async fn notify_episode_changed(
        &self,
        sonarr_series_id: i64,
        sonarr_episode_file_id: i64,
    ) -> Result<()> {
        let url = self.endpoint_url("api/webhooks/sonarr");
        debug!(
            "Bazarr: notifying episode changed (series={}, file={})",
            sonarr_series_id, sonarr_episode_file_id
        );

        let body = serde_json::json!({
            "eventType": "Download",
            "series": {
                "id": sonarr_series_id,
            },
            "episodeFile": {
                "id": sonarr_episode_file_id,
            }
        });

        let resp = self
            .send_authenticated(&url, |url| self.client.post(url).json(&body))
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            warn!("Bazarr sonarr webhook error {}: {}", status, body_text);
            anyhow::bail!("Bazarr episode notification failed: {}", status);
        }

        info!(
            "Bazarr: episode subtitle search triggered (file_id={})",
            sonarr_episode_file_id
        );
        Ok(())
    }

    /// Trigger a subtitle search for a movie via the Radarr webhook.
    pub async fn notify_movie_changed(
        &self,
        radarr_movie_id: i64,
        radarr_movie_file_id: i64,
    ) -> Result<()> {
        let url = self.endpoint_url("api/webhooks/radarr");
        debug!(
            "Bazarr: notifying movie changed (movie={}, file={})",
            radarr_movie_id, radarr_movie_file_id
        );

        let body = serde_json::json!({
            "eventType": "Download",
            "movie": {
                "id": radarr_movie_id,
            },
            "movieFile": {
                "id": radarr_movie_file_id,
            }
        });

        let resp = self
            .send_authenticated(&url, |url| self.client.post(url).json(&body))
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            warn!("Bazarr radarr webhook error {}: {}", status, body_text);
            anyhow::bail!("Bazarr movie notification failed: {}", status);
        }

        info!(
            "Bazarr: movie subtitle search triggered (file_id={})",
            radarr_movie_file_id
        );
        Ok(())
    }

    /// Trigger Bazarr's system tasks to search for missing subtitles.
    ///
    /// This runs the "wanted subtitles" search for both series and movies,
    /// which will pick up any newly created symlinks that need subtitles.
    pub async fn trigger_sync(&self) -> Result<()> {
        info!("Bazarr: triggering subtitle sync...");

        // Trigger series subtitle search
        let url = self.endpoint_url("api/system/tasks");

        for task_name in &[
            "search_wanted_subtitles_series",
            "search_wanted_subtitles_movies",
        ] {
            let body = serde_json::json!({ "taskid": task_name });
            debug!("Bazarr: triggering task '{}'", task_name);

            let resp = self
                .send_authenticated(&url, |url| self.client.post(url).json(&body))
                .await?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body_text = resp.text().await.unwrap_or_default();
                warn!(
                    "Bazarr task '{}' trigger failed {}: {}",
                    task_name, status, body_text
                );
            } else {
                info!("Bazarr: task '{}' triggered successfully", task_name);
            }
        }

        Ok(())
    }

    /// Lightweight Bazarr health check without side effects.
    /// Note: Bazarr uses a dual-auth strategy (header + query-param fallback)
    /// that differs from the standard *arr pattern in `http::check_system_status`.
    pub async fn health_check(&self) -> Result<()> {
        let url = self.endpoint_url("api/system/tasks");
        let resp = self
            .send_authenticated(&url, |url| self.client.get(url))
            .await
            .map_err(|e| anyhow::anyhow!("Bazarr health check failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            warn!("Bazarr health check failed {}: {}", status, body_text);
            anyhow::bail!("Bazarr health check failed: {}", status);
        }

        Ok(())
    }
}

fn should_retry_with_query_auth(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn spawn_one_shot_http_server(status_line: &str, body: &str) -> Option<String> {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(_) => return None,
        };
        let addr = listener.local_addr().unwrap();
        let status = status_line.to_string();
        let response_body = body.to_string();

        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut req_buf = [0u8; 1024];
                let _ = stream.read(&mut req_buf);
                let response = format!(
                    "{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    response_body.len(),
                    response_body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        Some(format!("http://{}", addr))
    }

    fn spawn_sequence_http_server(
        responses: &[(&str, &str)],
    ) -> Option<(String, Arc<Mutex<Vec<String>>>)> {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(_) => return None,
        };
        let addr = listener.local_addr().unwrap();
        let planned_responses: Vec<(String, String)> = responses
            .iter()
            .map(|(status, body)| ((*status).to_string(), (*body).to_string()))
            .collect();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured_requests = Arc::clone(&requests);

        std::thread::spawn(move || {
            for (status, response_body) in planned_responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut req_buf = [0u8; 4096];
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let size = stream.read(&mut req_buf).unwrap_or(0);
                captured_requests
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&req_buf[..size]).to_string());
                let response = format!(
                    "{}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                    status,
                    response_body.len(),
                    response_body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        Some((format!("http://{}", addr), requests))
    }

    #[test]
    fn with_query_api_key_appends_expected_parameter() {
        let cfg = BazarrConfig {
            url: "http://localhost:6767".to_string(),
            api_key: "secret".to_string(),
        };
        let client = BazarrClient::new(&cfg);

        assert_eq!(
            client.with_query_api_key("http://localhost:6767/api/system/tasks"),
            "http://localhost:6767/api/system/tasks?apikey=secret"
        );
        assert_eq!(
            client.with_query_api_key("http://localhost:6767/api/system/tasks?cmd=test"),
            "http://localhost:6767/api/system/tasks?cmd=test&apikey=secret"
        );
    }

    #[test]
    fn should_retry_with_query_auth_for_known_auth_failures() {
        assert!(should_retry_with_query_auth(StatusCode::BAD_REQUEST));
        assert!(should_retry_with_query_auth(StatusCode::UNAUTHORIZED));
        assert!(should_retry_with_query_auth(StatusCode::FORBIDDEN));
        assert!(!should_retry_with_query_auth(
            StatusCode::INTERNAL_SERVER_ERROR
        ));
    }

    #[test]
    fn test_parse_bazarr_episode() {
        let json = r#"{
            "sonarrEpisodeFileId": 1234,
            "title": "Breaking Bad - S01E01",
            "missing_subtitles": [
                {"code2": "sv", "name": "Swedish"},
                {"code2": "en", "name": "English"}
            ]
        }"#;

        let ep: BazarrEpisode = serde_json::from_str(json).unwrap();
        assert_eq!(ep.title, "Breaking Bad - S01E01");
        assert_eq!(ep.missing_subtitles.len(), 2);
        assert_eq!(ep.missing_subtitles[0].code2, "sv");
    }

    #[test]
    fn test_parse_bazarr_movie() {
        let json = r#"{
            "radarrId": 42,
            "title": "The Matrix (1999)",
            "missing_subtitles": []
        }"#;

        let movie: BazarrMovie = serde_json::from_str(json).unwrap();
        assert_eq!(movie.title, "The Matrix (1999)");
        assert!(movie.missing_subtitles.is_empty());
    }

    #[tokio::test]
    async fn test_health_check_success() {
        let Some(base_url) = spawn_one_shot_http_server("HTTP/1.1 200 OK", "[]") else {
            return;
        };
        let cfg = BazarrConfig {
            url: base_url,
            api_key: "test".to_string(),
        };
        let client = BazarrClient::new(&cfg);
        client.health_check().await.unwrap();
    }

    #[tokio::test]
    async fn test_health_check_failure_status() {
        let Some(base_url) =
            spawn_one_shot_http_server("HTTP/1.1 500 Internal Server Error", "boom")
        else {
            return;
        };
        let cfg = BazarrConfig {
            url: base_url,
            api_key: "test".to_string(),
        };
        let client = BazarrClient::new(&cfg);
        let err = client.health_check().await.unwrap_err();
        assert!(err.to_string().contains("health check failed"));
    }

    #[tokio::test]
    async fn test_health_check_retries_with_query_param_after_bad_request() {
        let Some((base_url, requests)) = spawn_sequence_http_server(&[
            (
                "HTTP/1.1 400 Bad Request",
                r#"{"error":"apikey is required"}"#,
            ),
            ("HTTP/1.1 200 OK", "[]"),
        ]) else {
            return;
        };
        let cfg = BazarrConfig {
            url: base_url,
            api_key: "test".to_string(),
        };
        let client = BazarrClient::new(&cfg);

        client.health_check().await.unwrap();

        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert!(captured[0].contains("GET /api/system/tasks"));
        assert!(!captured[0].contains("apikey=test"));
        assert!(captured[1].contains("GET /api/system/tasks?apikey=test"));
    }
}

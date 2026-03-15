#![allow(dead_code)] // Serde fields + future-use methods

use anyhow::Result;
use reqwest::{Client, Response, StatusCode};
use serde::Deserialize;
use tracing::{debug, info};

use crate::api::http;
use crate::config::TautulliConfig;

// ─── Response types ─────────────────────────────────────────────────

/// Wrapper for Tautulli API responses
#[derive(Debug, Deserialize)]
pub struct TautulliResponse<T> {
    pub response: TautulliResponseInner<T>,
}

#[derive(Debug, Deserialize)]
pub struct TautulliResponseInner<T> {
    pub result: String,
    pub data: T,
}

/// Current activity data
#[derive(Debug, Deserialize)]
pub struct ActivityData {
    #[serde(default = "default_zero_string")]
    pub stream_count: String,
    #[serde(default)]
    pub sessions: Vec<TautulliSession>,
}

fn default_zero_string() -> String {
    "0".to_string()
}

/// An active Plex session
#[derive(Debug, Clone, Deserialize)]
pub struct TautulliSession {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub grandparent_title: String,
    #[serde(default)]
    pub parent_title: String,
    #[serde(default)]
    pub year: String,
    #[serde(default)]
    pub media_type: String,
    #[serde(default)]
    pub friendly_name: String,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub progress_percent: String,
}

impl std::fmt::Display for TautulliSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.media_type.as_str() {
            "episode" => write!(
                f,
                "📺 {} - {} ({}) [{}]",
                self.grandparent_title, self.title, self.friendly_name, self.state
            ),
            "movie" => write!(
                f,
                "🎬 {} ({}) [{}]",
                self.title, self.friendly_name, self.state
            ),
            _ => write!(
                f,
                "🎵 {} ({}) [{}]",
                self.title, self.friendly_name, self.state
            ),
        }
    }
}

/// History data wrapper
#[derive(Debug, Deserialize)]
pub struct HistoryData {
    #[serde(default)]
    pub data: Vec<TautulliHistoryEntry>,
    #[serde(default, rename = "recordsFiltered")]
    pub records_filtered: i64,
    #[serde(default, rename = "recordsTotal")]
    pub records_total: i64,
}

/// A historical playback entry
#[derive(Debug, Clone, Deserialize)]
pub struct TautulliHistoryEntry {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub grandparent_title: String,
    #[serde(default)]
    pub full_title: String,
    #[serde(default)]
    pub year: i32,
    #[serde(default)]
    pub media_type: String,
    #[serde(default)]
    pub friendly_name: String,
    #[serde(default)]
    pub date: i64,
    #[serde(default)]
    pub duration: i64,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub rating_key: i64,
}

// ─── Client ─────────────────────────────────────────────────────────

/// Client for Tautulli's API v2
pub struct TautulliClient {
    client: Client,
    base_url: String,
    api_key: String,
}

impl TautulliClient {
    pub fn new(config: &TautulliConfig) -> Self {
        let base_url = config.url.trim_end_matches('/').to_string();
        Self {
            client: http::build_client(),
            base_url,
            api_key: config.api_key.clone(),
        }
    }

    async fn get_with_auth(&self, params: &[(&str, &str)]) -> Result<Response> {
        let url = format!("{}/api/v2", self.base_url);
        let resp = http::send_with_retry(
            self.client
                .get(&url)
                .header("X-Api-Key", &self.api_key)
                .query(params),
        )
        .await?;

        if should_retry_with_query_auth(resp.status()) {
            debug!(
                "Tautulli: header auth was not accepted (status={}), retrying with apikey query parameter",
                resp.status()
            );
            let mut fallback_params = params.to_vec();
            fallback_params.push(("apikey", self.api_key.as_str()));
            return http::send_with_retry(self.client.get(&url).query(&fallback_params)).await;
        }

        Ok(resp)
    }

    /// Get current Plex activity (who is watching what right now).
    pub async fn get_activity(&self) -> Result<ActivityData> {
        debug!("Tautulli: GET get_activity");
        let resp = self.get_with_auth(&[("cmd", "get_activity")]).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Tautulli error {}: {}", status, body);
        }

        let wrapper: TautulliResponse<ActivityData> = resp.json().await?;
        info!(
            "Tautulli: {} active streams",
            wrapper.response.data.stream_count
        );

        Ok(wrapper.response.data)
    }

    /// Get playback history.
    pub async fn get_history(&self, length: u32, media_type: Option<&str>) -> Result<HistoryData> {
        debug!("Tautulli: GET get_history (length={})", length);

        let length_str = length.to_string();
        let mut params: Vec<(&str, &str)> = vec![("cmd", "get_history"), ("length", &length_str)];

        if let Some(mt) = media_type {
            params.push(("media_type", mt));
        }

        let resp = self.get_with_auth(&params).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Tautulli history error {}: {}", status, body);
        }

        let wrapper: TautulliResponse<HistoryData> = resp.json().await?;
        info!(
            "Tautulli: {} history entries fetched",
            wrapper.response.data.data.len()
        );

        Ok(wrapper.response.data)
    }

    /// Get file paths of *currently* playing content.
    /// Useful for repair priority: don't mess with files someone is watching.
    pub async fn get_active_file_paths(&self) -> Result<Vec<String>> {
        let activity = self.get_activity().await?;
        let paths: Vec<String> = activity
            .sessions
            .iter()
            .filter(|s| !s.file.is_empty())
            .map(|s| s.file.clone())
            .collect();
        Ok(paths)
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
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn spawn_sequence_http_server(
        responses: &[(&str, &str)],
    ) -> Option<(String, Arc<Mutex<Vec<String>>>)> {
        use std::io::{Read, Write};
        use std::net::TcpListener;

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
    fn test_parse_activity_response() {
        let json = r#"{
            "response": {
                "result": "success",
                "data": {
                    "stream_count": "2",
                    "sessions": [
                        {
                            "title": "Ozymandias",
                            "grandparent_title": "Breaking Bad",
                            "parent_title": "Season 5",
                            "year": "2013",
                            "media_type": "episode",
                            "friendly_name": "Lenny",
                            "file": "/mnt/plex/tv/Breaking Bad/Season 5/S05E14.mkv",
                            "state": "playing",
                            "progress_percent": "45"
                        },
                        {
                            "title": "The Matrix",
                            "grandparent_title": "",
                            "parent_title": "",
                            "year": "1999",
                            "media_type": "movie",
                            "friendly_name": "Guest",
                            "file": "/mnt/plex/movies/The Matrix (1999)/The.Matrix.mkv",
                            "state": "playing",
                            "progress_percent": "72"
                        }
                    ]
                }
            }
        }"#;

        let resp: TautulliResponse<ActivityData> = serde_json::from_str(json).unwrap();
        assert_eq!(resp.response.data.stream_count, "2");
        assert_eq!(resp.response.data.sessions.len(), 2);
        assert_eq!(
            resp.response.data.sessions[0].grandparent_title,
            "Breaking Bad"
        );
        assert_eq!(resp.response.data.sessions[1].media_type, "movie");
    }

    #[test]
    fn test_parse_activity_response_without_stream_count_defaults_to_zero() {
        let json = r#"{
            "response": {
                "result": "success",
                "data": {
                    "sessions": []
                }
            }
        }"#;

        let resp: TautulliResponse<ActivityData> = serde_json::from_str(json).unwrap();
        assert_eq!(resp.response.data.stream_count, "0");
        assert!(resp.response.data.sessions.is_empty());
    }

    #[test]
    fn test_parse_history_response() {
        let json = r#"{
            "response": {
                "result": "success",
                "data": {
                    "recordsFiltered": 1,
                    "recordsTotal": 100,
                    "data": [
                        {
                            "title": "Pilot",
                            "grandparent_title": "Breaking Bad",
                            "full_title": "Breaking Bad - S01E01 - Pilot",
                            "year": 2008,
                            "media_type": "episode",
                            "friendly_name": "Lenny",
                            "date": 1707753600,
                            "duration": 3480,
                            "file": "/mnt/plex/tv/Breaking Bad/Season 1/S01E01.mkv",
                            "rating_key": 12345
                        }
                    ]
                }
            }
        }"#;

        let resp: TautulliResponse<HistoryData> = serde_json::from_str(json).unwrap();
        let data = resp.response.data;
        assert_eq!(data.data.len(), 1);
        assert_eq!(data.data[0].full_title, "Breaking Bad - S01E01 - Pilot");
        assert_eq!(data.data[0].year, 2008);
    }

    #[test]
    fn test_session_display() {
        let session = TautulliSession {
            title: "Ozymandias".to_string(),
            grandparent_title: "Breaking Bad".to_string(),
            parent_title: "Season 5".to_string(),
            year: "2013".to_string(),
            media_type: "episode".to_string(),
            friendly_name: "Lenny".to_string(),
            file: String::new(),
            state: "playing".to_string(),
            progress_percent: "45".to_string(),
        };

        let display = format!("{}", session);
        assert!(display.contains("Breaking Bad"));
        assert!(display.contains("Ozymandias"));
        assert!(display.contains("Lenny"));
    }

    #[test]
    fn should_retry_with_query_auth_for_known_auth_failures() {
        assert!(should_retry_with_query_auth(StatusCode::BAD_REQUEST));
        assert!(should_retry_with_query_auth(StatusCode::UNAUTHORIZED));
        assert!(should_retry_with_query_auth(StatusCode::FORBIDDEN));
        assert!(!should_retry_with_query_auth(StatusCode::NOT_FOUND));
    }

    #[tokio::test]
    async fn get_activity_retries_with_query_param_after_bad_request() {
        let Some((base_url, requests)) = spawn_sequence_http_server(&[
            (
                "HTTP/1.1 400 Bad Request",
                r#"{"response":{"result":"error","message":"Parameter apikey is required","data":{}}}"#,
            ),
            (
                "HTTP/1.1 200 OK",
                r#"{"response":{"result":"success","data":{"stream_count":"0","sessions":[]}}}"#,
            ),
        ]) else {
            return;
        };

        let cfg = TautulliConfig {
            url: base_url,
            api_key: "test".to_string(),
        };
        let client = TautulliClient::new(&cfg);

        let activity = client.get_activity().await.unwrap();
        assert_eq!(activity.stream_count, "0");

        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert!(captured[0].contains("GET /api/v2?cmd=get_activity"));
        assert!(!captured[0].contains("apikey=test"));
        assert!(captured[1].contains("GET /api/v2?cmd=get_activity&apikey=test"));
    }
}

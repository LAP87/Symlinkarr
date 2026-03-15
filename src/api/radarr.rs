use anyhow::Result;
use reqwest::Client;

use crate::api::http;

pub struct RadarrClient {
    client: Client,
    base_url: String,
    api_key: String,
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
        crate::api::http::check_system_status(&self.client, &self.base_url, &self.api_key, "v3", "Radarr").await
    }
}

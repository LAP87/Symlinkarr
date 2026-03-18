#![allow(dead_code)] // Serde fields + scaffolded search API

use anyhow::Result;
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashSet;
use tracing::debug;

use crate::api::http;
use crate::config::ProwlarrConfig;

// ─── Prowlarr search categories ─────────────────────────────────────

/// Newznab/Torznab category IDs used by Prowlarr
pub mod categories {
    pub const MOVIES: i32 = 2000;
    pub const TV: i32 = 5000;
    pub const TV_ANIME: i32 = 5070;
}

// ─── Response types ─────────────────────────────────────────────────

/// A search result from Prowlarr
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProwlarrResult {
    pub guid: String,
    pub title: String,
    pub indexer_id: i32,
    #[serde(default)]
    pub indexer: String,
    pub size: i64,
    #[serde(default)]
    pub seeders: Option<i32>,
    #[serde(default)]
    pub leechers: Option<i32>,
    #[serde(default)]
    pub download_url: Option<String>,
    #[serde(default)]
    pub magnet_url: Option<String>,
    #[serde(default)]
    pub categories: Vec<ProwlarrCategory>,
    #[serde(default)]
    pub protocol: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProwlarrCategory {
    pub id: i32,
    #[serde(default)]
    pub name: String,
}

impl ProwlarrResult {
    /// Get the best downloadable URL (prefer magnet, then download_url)
    pub fn best_url(&self) -> Option<&str> {
        self.magnet_url.as_deref().or(self.download_url.as_deref())
    }
}

// ─── Client ─────────────────────────────────────────────────────────

/// Client for Prowlarr's API v1
pub struct ProwlarrClient {
    client: Client,
    base_url: String,
    api_key: String,
}

impl ProwlarrClient {
    pub fn new(config: &ProwlarrConfig) -> Self {
        let base_url = config.url.trim_end_matches('/').to_string();
        Self {
            client: http::build_client(),
            base_url,
            api_key: config.api_key.clone(),
        }
    }

    /// Search all torrent indexers for the given query.
    /// Uses indexerIds=-2 (all torrent indexers).
    pub async fn search(&self, query: &str, categories: &[i32]) -> Result<Vec<ProwlarrResult>> {
        let url = format!("{}/api/v1/search", self.base_url);
        debug!(
            "Prowlarr: searching '{}' (categories: {:?})",
            query, categories
        );

        let cat_params: Vec<String> = categories.iter().map(|c| c.to_string()).collect();

        let mut req = self
            .client
            .get(&url)
            .header("X-Api-Key", &self.api_key)
            .query(&[("query", query)])
            .query(&[("indexerIds", "-2")]); // -2 = all torrent indexers

        for cat in &cat_params {
            req = req.query(&[("categories", cat.as_str())]);
        }

        let resp = http::send_with_retry(req).await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Prowlarr search error {}: {}", status, body);
        }

        let results: Vec<ProwlarrResult> = resp.json().await?;
        debug!("Prowlarr: {} results for '{}'", results.len(), query);

        Ok(results)
    }

    /// Grab/download a release through Prowlarr (sends to configured download client).
    pub async fn grab(&self, guid: &str, indexer_id: i32) -> Result<()> {
        let url = format!("{}/api/v1/search", self.base_url);
        debug!(
            "Prowlarr: grabbing release {} (indexer {})",
            guid, indexer_id
        );

        let body = serde_json::json!({
            "guid": guid,
            "indexerId": indexer_id,
        });

        let req = self
            .client
            .post(&url)
            .header("X-Api-Key", &self.api_key)
            .json(&body);
        let resp = http::send_with_retry(req).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Prowlarr grab error {}: {}", status, body_text);
        }

        debug!("Prowlarr: release sent to download client");
        Ok(())
    }

    /// Search + rank results by seeders (descending), keeping only verified downloadable hits.
    pub async fn search_ranked(
        &self,
        query: &str,
        categories: &[i32],
    ) -> Result<Vec<ProwlarrResult>> {
        let mut results = self.search(query, categories).await?;

        // Filter out results without a downloadable URL
        results.retain(|r| r.best_url().is_some());

        // Title verification: ensure normalized result title contains normalized query terms
        results.retain(|r| title_satisfies_query(query, &r.title));

        if results.is_empty() {
            debug!(
                "Prowlarr: No results passed title verification for '{}'",
                query
            );
            return Ok(Vec::new());
        }

        // Prefer semantically closer title hits before falling back to raw seeder counts.
        results.sort_by(|a, b| {
            search_rank_score(query, &b.title)
                .cmp(&search_rank_score(query, &a.title))
                .then_with(|| b.seeders.unwrap_or(0).cmp(&a.seeders.unwrap_or(0)))
                .then_with(|| b.size.cmp(&a.size))
        });

        Ok(results)
    }

    /// Search + pick best result (most seeders, with downloadable URL + title verification).
    pub async fn search_best(
        &self,
        query: &str,
        categories: &[i32],
    ) -> Result<Option<ProwlarrResult>> {
        let results = self.search_ranked(query, categories).await?;
        let best = results.into_iter().next();
        if let Some(ref r) = best {
            debug!(
                "Prowlarr: best result: '{}' ({} seeders, {})",
                r.title,
                r.seeders.unwrap_or(0),
                r.indexer
            );
        }

        Ok(best)
    }

    pub async fn get_system_status(&self) -> Result<()> {
        crate::api::http::check_system_status(
            &self.client,
            &self.base_url,
            &self.api_key,
            "v1",
            "Prowlarr",
        )
        .await
    }
}

fn title_satisfies_query(query: &str, title: &str) -> bool {
    let query_tokens = normalized_tokens(query);
    if query_tokens.is_empty() {
        return false;
    }

    let title_tokens = expanded_tokens(title);
    let mut numeric_tokens = 0usize;
    let mut matched_text_tokens = 0usize;
    let mut total_text_tokens = 0usize;

    for token in query_tokens {
        if parse_season_episode_token(&token).is_some()
            || parse_season_token(&token).is_some()
            || parse_small_number_token(&token).is_some()
        {
            numeric_tokens += 1;
            if !title_tokens.contains(token.as_str()) {
                return false;
            }
            continue;
        }

        total_text_tokens += 1;
        if title_text_token_matches(&token, &title_tokens) {
            matched_text_tokens += 1;
        }
    }

    if total_text_tokens == 0 {
        return true;
    }

    if numeric_tokens > 0 {
        matched_text_tokens >= 1
    } else {
        matched_text_tokens == total_text_tokens
    }
}

fn normalized_tokens(value: &str) -> Vec<String> {
    crate::utils::normalize(value)
        .split_whitespace()
        .map(|token| token.to_string())
        .collect()
}

fn search_rank_score(query: &str, title: &str) -> i64 {
    let title_tokens = expanded_tokens(title);
    normalized_tokens(query)
        .into_iter()
        .map(|token| {
            if title_tokens.contains(token.as_str())
                || title_text_token_matches(&token, &title_tokens)
            {
                if parse_season_episode_token(&token).is_some() {
                    200
                } else if parse_season_token(&token).is_some() {
                    140
                } else if parse_small_number_token(&token).is_some() {
                    80
                } else {
                    100
                }
            } else {
                0
            }
        })
        .sum()
}

fn title_text_token_matches(query_token: &str, title_tokens: &HashSet<String>) -> bool {
    title_tokens.iter().any(|title_token| {
        title_token == query_token
            || (query_token.len() >= 5 && title_token.starts_with(query_token))
            || (title_token.len() >= 5 && query_token.starts_with(title_token))
    })
}

fn expanded_tokens(value: &str) -> HashSet<String> {
    let raw_tokens = normalized_tokens(value);
    let mut expanded = HashSet::new();

    for token in &raw_tokens {
        insert_expanded_token(&mut expanded, token);
    }

    for window in raw_tokens.windows(2) {
        match window {
            [season, number] if *season == "season" => {
                if let Some(value) = parse_small_number_token(number) {
                    expanded.insert(format!("s{:02}", value));
                }
            }
            [episode, number] if *episode == "episode" || *episode == "ep" => {
                if let Some(value) = parse_small_number_token(number) {
                    expanded.insert(format!("e{:02}", value));
                    expanded.insert(value.to_string());
                }
            }
            _ => {}
        }
    }

    expanded
}

fn insert_expanded_token(expanded: &mut HashSet<String>, token: &str) {
    expanded.insert(token.to_string());

    if let Some((season, episode)) = parse_season_episode_token(token) {
        expanded.insert(format!("s{:02}e{:02}", season, episode));
        expanded.insert(format!("s{:02}", season));
        expanded.insert(format!("e{:02}", episode));
        expanded.insert(episode.to_string());
        return;
    }

    if let Some(season) = parse_season_token(token) {
        expanded.insert(format!("s{:02}", season));
        return;
    }

    if let Some(number) = parse_small_number_token(token) {
        expanded.insert(number.to_string());
    }
}

fn parse_season_episode_token(token: &str) -> Option<(u32, u32)> {
    let lower = token.to_ascii_lowercase();
    let (season, episode) = lower.split_once('e')?;
    let season = season.strip_prefix('s')?;
    if season.is_empty()
        || episode.is_empty()
        || !season.chars().all(|ch| ch.is_ascii_digit())
        || !episode.chars().all(|ch| ch.is_ascii_digit())
    {
        return None;
    }

    Some((season.parse().ok()?, episode.parse().ok()?))
}

fn parse_season_token(token: &str) -> Option<u32> {
    let lower = token.to_ascii_lowercase();
    let season = lower.strip_prefix('s')?;
    if season.is_empty() || !season.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    season.parse().ok()
}

fn parse_small_number_token(token: &str) -> Option<u32> {
    if token.is_empty() || !token.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }

    let value = token.parse::<u32>().ok()?;
    (1..=500).contains(&value).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_helpers::spawn_one_shot_http_server;

    #[test]
    fn test_parse_prowlarr_search_result() {
        let json = r#"[
            {
                "guid": "abc123",
                "title": "Breaking.Bad.S01.1080p.BluRay.x264",
                "indexerId": 5,
                "indexer": "1337x",
                "size": 5368709120,
                "seeders": 42,
                "leechers": 5,
                "downloadUrl": "https://example.com/torrent/abc123",
                "magnetUrl": "magnet:?xt=urn:btih:abc123",
                "categories": [{"id": 5000, "name": "TV"}],
                "protocol": "torrent"
            },
            {
                "guid": "def456",
                "title": "Breaking.Bad.S01.720p.WEB-DL",
                "indexerId": 3,
                "indexer": "RARBG",
                "size": 3221225472,
                "seeders": 15,
                "categories": [{"id": 5000, "name": "TV"}],
                "protocol": "torrent"
            }
        ]"#;

        let results: Vec<ProwlarrResult> = serde_json::from_str(json).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Breaking.Bad.S01.1080p.BluRay.x264");
        assert_eq!(results[0].seeders, Some(42));
        assert_eq!(results[0].best_url(), Some("magnet:?xt=urn:btih:abc123"));
        assert_eq!(results[1].magnet_url, None);
        assert!(results[1].best_url().is_none()); // no download_url field either? Let me check
    }

    #[test]
    fn test_best_url_preference() {
        // Magnet preferred over download URL
        let json = r#"{
            "guid": "test",
            "title": "Test",
            "indexerId": 1,
            "size": 1000,
            "downloadUrl": "https://dl.example.com/test.torrent",
            "magnetUrl": "magnet:?xt=urn:btih:test",
            "protocol": "torrent"
        }"#;

        let result: ProwlarrResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.best_url(), Some("magnet:?xt=urn:btih:test"));

        // Fallback to download URL when no magnet
        let json2 = r#"{
            "guid": "test2",
            "title": "Test2",
            "indexerId": 1,
            "size": 1000,
            "downloadUrl": "https://dl.example.com/test.torrent",
            "protocol": "torrent"
        }"#;

        let result2: ProwlarrResult = serde_json::from_str(json2).unwrap();
        assert_eq!(
            result2.best_url(),
            Some("https://dl.example.com/test.torrent")
        );
    }

    #[test]
    fn title_satisfies_query_uses_token_boundaries() {
        assert!(title_satisfies_query(
            "breaking bad s01e01",
            "Breaking.Bad.S01E01.1080p.BluRay"
        ));
        assert!(!title_satisfies_query("it", "Little Women 2019"));
        assert!(!title_satisfies_query("show s01e01", "Showgroup.S01E01"));
    }

    #[test]
    fn title_satisfies_query_matches_season_and_absolute_variants() {
        assert!(title_satisfies_query(
            "the darwin incident s01",
            "The.Darwin.Incident.S01E10.1080p"
        ));
        assert!(title_satisfies_query(
            "the darwin incident 10",
            "[Judas] Darwin Jihen (The Darwin Incident) - S01E10"
        ));
        assert!(title_satisfies_query(
            "frieren s01",
            "Frieren Season 1 Complete 1080p"
        ));
        assert!(!title_satisfies_query(
            "the darwin incident s01e10",
            "The.Darwin.Incident.S01E09.1080p"
        ));
    }

    #[tokio::test]
    async fn test_get_system_status_success() {
        let Some(base_url) = spawn_one_shot_http_server("HTTP/1.1 200 OK", "{}") else {
            return;
        };
        let cfg = ProwlarrConfig {
            url: base_url,
            api_key: "test".to_string(),
        };
        let client = ProwlarrClient::new(&cfg);
        client.get_system_status().await.unwrap();
    }
}

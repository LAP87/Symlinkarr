#![allow(dead_code)] // Module scaffolded for future TVDB integration

use anyhow::Result;
use reqwest::Client;
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::api::http;
use crate::db::Database;
use crate::models::{ContentMetadata, EpisodeInfo, SeasonInfo};

const TVDB_BASE_URL: &str = "https://api4.thetvdb.com/v4";
const NEGATIVE_METADATA_SENTINEL: &str =
    r#"{"_symlinkarr_not_found":true,"title":"","aliases":[],"year":null,"seasons":[]}"#;

/// TVDB API client (optional). Uses v4 API with JWT authentication.
pub struct TvdbClient {
    client: Client,
    api_key: String,
    token: Option<String>,
    cache_ttl: u64,
}

// --- TVDB API response types ---

#[derive(Debug, Deserialize)]
struct TvdbAuthResponse {
    data: Option<TvdbAuthData>,
}

#[derive(Debug, Deserialize)]
struct TvdbAuthData {
    token: String,
}

#[derive(Debug, Deserialize)]
struct TvdbSeriesResponse {
    data: Option<TvdbSeries>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TvdbSeries {
    name: Option<String>,
    first_aired: Option<String>,
    #[serde(default)]
    aliases: Vec<TvdbAlias>,
    #[serde(default)]
    seasons: Vec<TvdbSeason>,
}

#[derive(Debug, Deserialize)]
struct TvdbAlias {
    name: Option<String>,
    language: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TvdbSeason {
    number: Option<u32>,
    #[serde(rename = "type")]
    season_type: Option<TvdbSeasonType>,
}

#[derive(Debug, Deserialize)]
struct TvdbSeasonType {
    id: Option<u32>,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TvdbEpisodesResponse {
    data: Option<TvdbEpisodesData>,
}

#[derive(Debug, Deserialize)]
struct TvdbEpisodesData {
    #[serde(default)]
    episodes: Vec<TvdbEpisode>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TvdbEpisode {
    number: Option<u32>,
    season_number: Option<u32>,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CachedMetadataSentinel {
    #[serde(default)]
    _symlinkarr_not_found: bool,
}

impl TvdbClient {
    pub fn new(api_key: &str, cache_ttl: u64) -> Self {
        Self {
            client: http::build_client(),
            api_key: api_key.to_string(),
            token: None,
            cache_ttl,
        }
    }

    /// Authenticate with TVDB and get a JWT token.
    pub async fn authenticate(&mut self) -> Result<()> {
        info!("Authenticating with TVDB API...");

        let req = self
            .client
            .post(format!("{}/login", TVDB_BASE_URL))
            .json(&serde_json::json!({
                "apikey": self.api_key
            }));
        let resp: TvdbAuthResponse = http::send_with_retry(req).await?.json().await?;

        if let Some(data) = resp.data {
            self.token = Some(data.token);
            info!("TVDB authentication successful");
        } else {
            anyhow::bail!("TVDB authentication failed — no token received");
        }

        Ok(())
    }

    /// Fetch metadata for a TV series from TVDB.
    pub fn get_series_metadata<'a>(
        &'a mut self,
        tvdb_id: u64,
        db: &'a Database,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ContentMetadata>> + Send + 'a>>
    {
        Box::pin(async move { self.get_series_metadata_inner(tvdb_id, db, false).await })
    }

    fn get_series_metadata_inner<'a>(
        &'a mut self,
        tvdb_id: u64,
        db: &'a Database,
        retried: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ContentMetadata>> + Send + 'a>>
    {
        Box::pin(async move {
            // Check cache
            let cache_key = format!("tvdb:series:{}", tvdb_id);
            if let Some(cached) = db.get_cached(&cache_key).await? {
                if cached_metadata_is_negative(&cached) {
                    debug!("Negative cache hit for TVDB {}", tvdb_id);
                    anyhow::bail!("No data for TVDB {}", tvdb_id);
                }
                if let Ok(metadata) = serde_json::from_str::<ContentMetadata>(&cached) {
                    debug!("Cache hit for TVDB {}", tvdb_id);
                    return Ok(metadata);
                }
            }

            // Ensure we have a valid token
            if self.token.is_none() {
                self.authenticate().await?;
            }

            let token = self.token.as_ref().unwrap();

            // Fetch series details with aliases
            let url = format!("{}/series/{}/extended", TVDB_BASE_URL, tvdb_id);
            let resp = http::send_with_retry(self.client.get(&url).bearer_auth(token)).await?;

            if resp.status() == 401 {
                if retried {
                    anyhow::bail!(
                        "TVDB authentication failed for {}: invalid API key or token",
                        tvdb_id
                    );
                }
                // Token expired, re-authenticate and retry once
                self.authenticate().await?;
                return self.get_series_metadata_inner(tvdb_id, db, true).await;
            }

            if resp.status() == 404 {
                cache_negative_metadata(db, &cache_key, self.cache_ttl).await;
                anyhow::bail!("No data for TVDB {}", tvdb_id);
            }

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!(
                    "TVDB series lookup error {} for {}: {}",
                    status,
                    tvdb_id,
                    body
                );
            }

            let series_resp: TvdbSeriesResponse = resp.json().await?;
            let Some(series) = series_resp.data else {
                cache_negative_metadata(db, &cache_key, self.cache_ttl).await;
                anyhow::bail!("No data for TVDB {}", tvdb_id);
            };

            let title = series.name.unwrap_or_default();
            let year = series
                .first_aired
                .as_deref()
                .and_then(|d| d.get(..4))
                .and_then(|y| y.parse().ok());

            let aliases: Vec<String> = series.aliases.into_iter().filter_map(|a| a.name).collect();

            // Get official seasons (type id 1 = "Aired Order")
            let official_seasons: Vec<u32> = series
                .seasons
                .iter()
                .filter(|s| {
                    s.season_type
                        .as_ref()
                        .map(|t| t.id == Some(1))
                        .unwrap_or(false)
                })
                .filter_map(|s| s.number)
                .filter(|n| *n > 0)
                .collect();

            // Fetch episodes
            let mut seasons = Vec::new();
            let episodes_url = format!("{}/series/{}/episodes/default", TVDB_BASE_URL, tvdb_id);
            match self.fetch_episodes(&episodes_url).await {
                Ok(all_episodes) => {
                    for season_num in &official_seasons {
                        let season_episodes: Vec<EpisodeInfo> = all_episodes
                            .iter()
                            .filter(|e| e.season_number == Some(*season_num))
                            .map(|e| EpisodeInfo {
                                episode_number: e.number.unwrap_or(0),
                                title: e.name.clone().unwrap_or_default(),
                            })
                            .collect();

                        seasons.push(SeasonInfo {
                            season_number: *season_num,
                            episodes: season_episodes,
                        });
                    }
                }
                Err(e) => warn!("Could not fetch episodes for TVDB {}: {}", tvdb_id, e),
            }

            let metadata = ContentMetadata {
                title,
                aliases,
                year,
                seasons,
            };

            // Cache the result
            if let Ok(json) = serde_json::to_string(&metadata) {
                let _ = db.set_cached(&cache_key, &json, self.cache_ttl).await;
            }

            Ok(metadata)
        })
    }

    async fn fetch_episodes(&self, url: &str) -> Result<Vec<TvdbEpisode>> {
        let token = self
            .token
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Not authenticated"))?;

        let req = self.client.get(url).bearer_auth(token);
        let resp: TvdbEpisodesResponse = http::send_with_retry(req).await?.json().await?;

        Ok(resp.data.map(|d| d.episodes).unwrap_or_default())
    }
}

async fn cache_negative_metadata(db: &Database, cache_key: &str, ttl_hours: u64) {
    if let Err(err) = db
        .set_cached(cache_key, NEGATIVE_METADATA_SENTINEL, ttl_hours)
        .await
    {
        warn!(
            "Failed to cache negative TVDB metadata for {}: {}",
            cache_key, err
        );
    }
}

fn cached_metadata_is_negative(cached: &str) -> bool {
    serde_json::from_str::<CachedMetadataSentinel>(cached)
        .map(|sentinel| sentinel._symlinkarr_not_found)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_negative_metadata_cache_entry() {
        assert!(cached_metadata_is_negative(NEGATIVE_METADATA_SENTINEL));
        assert!(!cached_metadata_is_negative(
            r#"{"title":"Frieren","aliases":[],"year":2023,"seasons":[]}"#
        ));
    }
}

#![allow(dead_code)] // Serde fields + future-use methods (get_tvdb_id)

use anyhow::Result;
use reqwest::Client;
use serde::Deserialize;
use tracing::{debug, warn};

use crate::api::http;
use crate::db::Database;
use crate::models::{ContentMetadata, EpisodeInfo, SeasonInfo};

const TMDB_BASE_URL: &str = "https://api.themoviedb.org/3";

/// TMDB API client for fetching metadata, aliases, and episode information.
#[derive(Clone)]
pub struct TmdbClient {
    client: Client,
    api_key: Option<String>,
    read_access_token: Option<String>,
    cache_ttl: u64,
}

// --- TMDB API response types ---

#[derive(Debug, Deserialize)]
struct TmdbTvDetails {
    name: Option<String>,
    first_air_date: Option<String>,
    #[serde(default)]
    seasons: Vec<TmdbSeason>,
}

#[derive(Debug, Deserialize)]
struct TmdbMovieDetails {
    title: Option<String>,
    release_date: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TmdbSeason {
    season_number: u32,
    episode_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct TmdbSeasonDetails {
    #[serde(default)]
    episodes: Vec<TmdbEpisode>,
}

#[derive(Debug, Deserialize)]
struct TmdbEpisode {
    episode_number: u32,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TmdbAlternativeTitles {
    #[serde(default, alias = "titles")]
    results: Vec<TmdbAlternativeTitle>,
}

#[derive(Debug, Deserialize)]
struct TmdbAlternativeTitle {
    title: Option<String>,
    #[serde(alias = "iso_3166_1")]
    country: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
struct TmdbExternalIds {
    tvdb_id: Option<u64>,
    imdb_id: Option<String>,
}

impl TmdbClient {
    /// Create a new TMDB client.
    pub fn new(api_key: &str, read_access_token: Option<&str>, cache_ttl: u64) -> Self {
        Self {
            client: http::build_client(),
            api_key: (!api_key.is_empty()).then(|| api_key.to_string()),
            read_access_token: read_access_token
                .filter(|token| !token.is_empty())
                .map(|token| token.to_string()),
            cache_ttl,
        }
    }

    fn authenticated_get(&self, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}/{}", TMDB_BASE_URL, path.trim_start_matches('/'));
        let req = self.client.get(&url);

        if let Some(token) = self.read_access_token.as_deref() {
            req.bearer_auth(token)
        } else if let Some(api_key) = self.api_key.as_deref() {
            req.query(&[("api_key", api_key)])
        } else {
            req
        }
    }

    /// Fetch full metadata for a TV show: title, aliases, seasons, and episodes.
    pub async fn get_tv_metadata(&self, tmdb_id: u64, db: &Database) -> Result<ContentMetadata> {
        // Check cache first
        let cache_key = format!("tmdb:tv:{}", tmdb_id);
        if let Some(cached) = db.get_cached(&cache_key).await? {
            if let Ok(metadata) = serde_json::from_str::<CachedMetadata>(&cached) {
                debug!("Cache hit for TMDB TV {}", tmdb_id);
                return Ok(metadata.into());
            }
        }

        // Fetch details
        let details: TmdbTvDetails =
            http::send_with_retry(self.authenticated_get(&format!("tv/{}", tmdb_id)))
                .await?
                .json()
                .await?;

        let title = details.name.unwrap_or_default();
        let year = details.first_air_date.as_deref().and_then(|d| {
            if d.len() >= 4 {
                d[..4].parse().ok()
            } else {
                None
            }
        });

        // Fetch alternative titles (aliases)
        let aliases = self.get_tv_aliases(tmdb_id).await.unwrap_or_default();

        // Fetch episode details for each season concurrently
        let mut season_set = tokio::task::JoinSet::new();
        for s in &details.seasons {
            if s.season_number == 0 {
                continue; // Skip specials
            }
            let client = self.clone();
            let season_num = s.season_number;
            season_set.spawn(async move {
                (
                    season_num,
                    client.get_season_details(tmdb_id, season_num).await,
                )
            });
        }

        let mut seasons = Vec::new();
        while let Some(res) = season_set.join_next().await {
            match res {
                Ok((_season_num, Ok(season_info))) => seasons.push(season_info),
                Ok((season_num, Err(e))) => warn!(
                    "Could not fetch season {} for TMDB {}: {}",
                    season_num, tmdb_id, e
                ),
                Err(e) => warn!("Season fetch task failed for TMDB {}: {}", tmdb_id, e),
            }
        }
        seasons.sort_by_key(|s| s.season_number);

        let metadata = ContentMetadata {
            title: title.clone(),
            aliases,
            year,
            seasons,
        };

        // Cache the result
        let cached = CachedMetadata::from(&metadata);
        if let Ok(json) = serde_json::to_string(&cached) {
            let _ = db.set_cached(&cache_key, &json, self.cache_ttl).await;
        }

        Ok(metadata)
    }

    /// Fetch full metadata for a movie.
    pub async fn get_movie_metadata(&self, tmdb_id: u64, db: &Database) -> Result<ContentMetadata> {
        let cache_key = format!("tmdb:movie:{}", tmdb_id);
        if let Some(cached) = db.get_cached(&cache_key).await? {
            if let Ok(metadata) = serde_json::from_str::<CachedMetadata>(&cached) {
                debug!("Cache hit for TMDB Movie {}", tmdb_id);
                return Ok(metadata.into());
            }
        }

        let details: TmdbMovieDetails =
            http::send_with_retry(self.authenticated_get(&format!("movie/{}", tmdb_id)))
                .await?
                .json()
                .await?;

        let title = details.title.unwrap_or_default();
        let year = details.release_date.as_deref().and_then(|d| {
            if d.len() >= 4 {
                d[..4].parse().ok()
            } else {
                None
            }
        });

        let aliases = self.get_movie_aliases(tmdb_id).await.unwrap_or_default();

        let metadata = ContentMetadata {
            title,
            aliases,
            year,
            seasons: Vec::new(),
        };

        let cached = CachedMetadata::from(&metadata);
        if let Ok(json) = serde_json::to_string(&cached) {
            let _ = db.set_cached(&cache_key, &json, self.cache_ttl).await;
        }

        Ok(metadata)
    }

    /// Fetch the TVDB ID for a TMDB TV show (cross-reference).
    pub async fn get_tvdb_id(&self, tmdb_id: u64, db: &Database) -> Result<Option<u64>> {
        Ok(self.get_external_ids("tv", tmdb_id, db).await?.tvdb_id)
    }

    pub async fn get_tv_imdb_id(&self, tmdb_id: u64, db: &Database) -> Result<Option<String>> {
        Ok(self.get_external_ids("tv", tmdb_id, db).await?.imdb_id)
    }

    pub async fn get_movie_imdb_id(&self, tmdb_id: u64, db: &Database) -> Result<Option<String>> {
        Ok(self.get_external_ids("movie", tmdb_id, db).await?.imdb_id)
    }

    // --- Private helpers ---

    async fn get_external_ids(
        &self,
        media_kind: &str,
        tmdb_id: u64,
        db: &Database,
    ) -> Result<TmdbExternalIds> {
        let cache_key = format!("tmdb:{}:external_ids:{}", media_kind, tmdb_id);
        if let Some(cached) = db.get_cached(&cache_key).await? {
            if let Ok(ids) = serde_json::from_str::<TmdbExternalIds>(&cached) {
                debug!("Cache hit for TMDB {} external ids {}", media_kind, tmdb_id);
                return Ok(ids);
            }
        }

        let ids: TmdbExternalIds = http::send_with_retry(
            self.authenticated_get(&format!("{}/{}/external_ids", media_kind, tmdb_id)),
        )
        .await?
        .json()
        .await?;

        if let Ok(json) = serde_json::to_string(&ids) {
            let _ = db.set_cached(&cache_key, &json, self.cache_ttl).await;
        }

        Ok(ids)
    }

    async fn get_tv_aliases(&self, tmdb_id: u64) -> Result<Vec<String>> {
        let resp: TmdbAlternativeTitles = http::send_with_retry(
            self.authenticated_get(&format!("tv/{}/alternative_titles", tmdb_id)),
        )
        .await?
        .json()
        .await?;
        Ok(resp.results.into_iter().filter_map(|t| t.title).collect())
    }

    async fn get_movie_aliases(&self, tmdb_id: u64) -> Result<Vec<String>> {
        let resp: TmdbAlternativeTitles = http::send_with_retry(
            self.authenticated_get(&format!("movie/{}/alternative_titles", tmdb_id)),
        )
        .await?
        .json()
        .await?;
        Ok(resp.results.into_iter().filter_map(|t| t.title).collect())
    }

    async fn get_season_details(&self, tmdb_id: u64, season_number: u32) -> Result<SeasonInfo> {
        let details: TmdbSeasonDetails = http::send_with_retry(
            self.authenticated_get(&format!("tv/{}/season/{}", tmdb_id, season_number)),
        )
        .await?
        .json()
        .await?;

        Ok(SeasonInfo {
            season_number,
            episodes: details
                .episodes
                .into_iter()
                .map(|e| EpisodeInfo {
                    episode_number: e.episode_number,
                    title: e.name.unwrap_or_default(),
                })
                .collect(),
        })
    }
}

// --- Serializable cache wrapper ---

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct CachedMetadata {
    title: String,
    aliases: Vec<String>,
    year: Option<u32>,
    seasons: Vec<CachedSeason>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct CachedSeason {
    season_number: u32,
    episodes: Vec<CachedEpisode>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct CachedEpisode {
    episode_number: u32,
    title: String,
}

impl From<&ContentMetadata> for CachedMetadata {
    fn from(m: &ContentMetadata) -> Self {
        Self {
            title: m.title.clone(),
            aliases: m.aliases.clone(),
            year: m.year,
            seasons: m
                .seasons
                .iter()
                .map(|s| CachedSeason {
                    season_number: s.season_number,
                    episodes: s
                        .episodes
                        .iter()
                        .map(|e| CachedEpisode {
                            episode_number: e.episode_number,
                            title: e.title.clone(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

impl From<CachedMetadata> for ContentMetadata {
    fn from(c: CachedMetadata) -> Self {
        Self {
            title: c.title,
            aliases: c.aliases,
            year: c.year,
            seasons: c
                .seasons
                .into_iter()
                .map(|s| SeasonInfo {
                    season_number: s.season_number,
                    episodes: s
                        .episodes
                        .into_iter()
                        .map(|e| EpisodeInfo {
                            episode_number: e.episode_number,
                            title: e.title,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::AUTHORIZATION;

    #[test]
    fn authenticated_get_uses_api_key_query_when_bearer_is_missing() {
        let client = TmdbClient::new("api-key", None, 24);
        let request = client.authenticated_get("tv/42").build().unwrap();

        assert!(request.url().as_str().contains("api_key=api-key"));
        assert!(request.headers().get(AUTHORIZATION).is_none());
    }

    #[test]
    fn authenticated_get_prefers_bearer_token_over_query_param() {
        let client = TmdbClient::new("api-key", Some("bearer-token"), 24);
        let request = client.authenticated_get("tv/42").build().unwrap();

        assert!(!request.url().as_str().contains("api_key="));
        assert_eq!(
            request.headers().get(AUTHORIZATION).unwrap(),
            "Bearer bearer-token"
        );
    }
}

use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Deserializer};
use tracing::debug;

use crate::api::http;
use crate::config::DmmConfig;

const DMM_SALT: &str = "debridmediamanager.com%%fe7#td00rA3vHz%VmI";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DmmMediaKind {
    Movie,
    Show,
}

impl DmmMediaKind {
    fn as_search_hint(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Show => "show",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DmmTitleCandidate {
    pub title: String,
    pub imdb_id: String,
    pub year: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct DmmTorrentResult {
    pub title: String,
    pub hash: String,
    pub file_size: i64,
}

#[derive(Debug, Clone)]
pub enum DmmTorrentLookup {
    Results(Vec<DmmTorrentResult>),
    Pending(String),
    Empty,
}

#[derive(Debug, Deserialize)]
struct DmmTitleSearchResponse {
    #[serde(default)]
    results: Vec<DmmTitleSearchItem>,
}

#[derive(Debug, Deserialize)]
struct DmmTitleSearchItem {
    #[serde(default)]
    title: String,
    #[serde(default)]
    year: u32,
    #[serde(default, rename = "imdbid")]
    imdb_id: String,
    #[serde(default, rename = "type")]
    media_type: String,
}

#[derive(Debug, Deserialize)]
struct DmmTorrentResponse {
    #[serde(default)]
    results: Vec<DmmTorrentWire>,
}

#[derive(Debug, Deserialize)]
struct DmmTorrentWire {
    #[serde(default)]
    title: String,
    #[serde(default)]
    hash: String,
    #[serde(
        default,
        deserialize_with = "deserialize_file_size",
        rename = "fileSize",
        alias = "size_bytes",
        alias = "bytes"
    )]
    file_size: i64,
}

pub struct DmmClient {
    client: Client,
    base_url: String,
    only_trusted: bool,
}

impl DmmClient {
    pub fn new(base_url: &str, only_trusted: bool) -> Self {
        Self {
            client: http::build_client(),
            base_url: base_url.trim_end_matches('/').to_string(),
            only_trusted,
        }
    }

    pub fn from_config(cfg: &DmmConfig) -> Self {
        Self::new(&cfg.url, cfg.only_trusted)
    }

    pub async fn search_title(
        &self,
        query: &str,
        kind: DmmMediaKind,
    ) -> Result<Vec<DmmTitleCandidate>> {
        let url = format!("{}/api/search/title", self.base_url);
        let hinted_query = format!("{} {}", query.trim(), kind.as_search_hint());
        debug!("DMM: searching '{}'", hinted_query);

        let req = self.client.get(&url).query(&[("keyword", hinted_query)]);
        let resp = http::send_with_retry(req).await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("DMM title search error {}: {}", status, body);
        }

        let response: DmmTitleSearchResponse = resp.json().await?;
        Ok(response
            .results
            .into_iter()
            .filter_map(|item| {
                let item_kind = parse_media_kind(&item.media_type)?;
                (!item.imdb_id.trim().is_empty() && item_kind == kind).then_some(
                    DmmTitleCandidate {
                        title: item.title,
                        imdb_id: item.imdb_id,
                        year: (item.year > 0).then_some(item.year),
                    },
                )
            })
            .collect())
    }

    pub async fn fetch_movie_results(&self, imdb_id: &str) -> Result<DmmTorrentLookup> {
        self.fetch_torrents("movie", &[("imdbId", imdb_id.to_string())])
            .await
    }

    pub async fn fetch_tv_results(&self, imdb_id: &str, season: u32) -> Result<DmmTorrentLookup> {
        self.fetch_torrents(
            "tv",
            &[
                ("imdbId", imdb_id.to_string()),
                ("seasonNum", season.to_string()),
            ],
        )
        .await
    }

    async fn fetch_torrents(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<DmmTorrentLookup> {
        let (token, solution) = generate_dmm_auth_pair()?;
        let url = format!("{}/api/torrents/{}", self.base_url, path);
        let mut req = self.client.get(&url).query(&[
            ("dmmProblemKey", token.as_str()),
            ("solution", solution.as_str()),
            (
                "onlyTrusted",
                if self.only_trusted { "true" } else { "false" },
            ),
        ]);

        for (key, value) in params {
            req = req.query(&[(key, value.as_str())]);
        }

        let resp = http::send_with_retry(req).await?;
        if resp.status() == StatusCode::NO_CONTENT {
            let status = resp
                .headers()
                .get("status")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("processing");
            return Ok(DmmTorrentLookup::Pending(status.to_string()));
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("DMM {} lookup error {}: {}", path, status, body);
        }

        let response: DmmTorrentResponse = resp.json().await?;
        let results = response
            .results
            .into_iter()
            .filter_map(|item| {
                let hash = item.hash.trim().to_string();
                (!hash.is_empty()).then_some(DmmTorrentResult {
                    title: item.title,
                    hash,
                    file_size: item.file_size,
                })
            })
            .collect::<Vec<_>>();

        if results.is_empty() {
            Ok(DmmTorrentLookup::Empty)
        } else {
            Ok(DmmTorrentLookup::Results(results))
        }
    }
}

fn parse_media_kind(value: &str) -> Option<DmmMediaKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "movie" => Some(DmmMediaKind::Movie),
        "show" | "series" => Some(DmmMediaKind::Show),
        _ => None,
    }
}

fn deserialize_file_size<'de, D>(deserializer: D) -> std::result::Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum FileSizeValue {
        Integer(i64),
        Float(f64),
        Text(String),
        Null,
    }

    match FileSizeValue::deserialize(deserializer)? {
        FileSizeValue::Integer(value) => Ok(value),
        FileSizeValue::Float(value) => Ok(value.round() as i64),
        FileSizeValue::Text(value) => Ok(value
            .trim()
            .parse::<f64>()
            .map(|parsed| parsed.round() as i64)
            .unwrap_or(0)),
        FileSizeValue::Null => Ok(0),
    }
}

fn generate_dmm_auth_pair() -> Result<(String, String)> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let token = format!("{:x}{:x}", process::id(), nanos);
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let token_with_timestamp = format!("{}-{}", token, timestamp);
    let token_timestamp_hash = js_hash_hex(&token_with_timestamp);
    let token_salt_hash = js_hash_hex(&format!("{}-{}", DMM_SALT, token));
    let solution = combine_hashes(&token_timestamp_hash, &token_salt_hash);
    Ok((token_with_timestamp, solution))
}

fn js_hash_hex(input: &str) -> String {
    let mut hash1 = 0xdeadbeefu32 ^ input.len() as u32;
    let mut hash2 = 0x41c6ce57u32 ^ input.len() as u32;

    for byte in input.bytes() {
        hash1 = (hash1 ^ byte as u32).wrapping_mul(2_654_435_761);
        hash2 = (hash2 ^ byte as u32).wrapping_mul(1_597_334_677);
        hash1 = hash1.rotate_left(5);
        hash2 = hash2.rotate_left(5);
    }

    hash1 = hash1.wrapping_add(hash2.wrapping_mul(1_566_083_941));
    hash2 = hash2.wrapping_add(hash1.wrapping_mul(2_024_237_689));

    format!("{:x}", hash1 ^ hash2)
}

fn combine_hashes(hash1: &str, hash2: &str) -> String {
    let half_length = hash1.len() / 2;
    let first_part1 = &hash1[..half_length];
    let second_part1 = &hash1[half_length..];
    let first_part2 = &hash2[..half_length.min(hash2.len())];
    let second_part2 = &hash2[half_length.min(hash2.len())..];

    let mut combined = String::new();
    for (left, right) in first_part1.chars().zip(first_part2.chars()) {
        combined.push(left);
        combined.push(right);
    }

    combined.extend(second_part2.chars().rev());
    combined.extend(second_part1.chars().rev());
    combined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_media_kind_accepts_show_aliases() {
        assert_eq!(parse_media_kind("movie"), Some(DmmMediaKind::Movie));
        assert_eq!(parse_media_kind("show"), Some(DmmMediaKind::Show));
        assert_eq!(parse_media_kind("series"), Some(DmmMediaKind::Show));
        assert_eq!(parse_media_kind("anime"), None);
    }

    #[test]
    fn combine_hashes_interleaves_and_reverses_suffixes() {
        assert_eq!(combine_hashes("abcd", "wxyz"), "awbxzydc");
    }

    #[test]
    fn generate_dmm_auth_pair_produces_expected_shapes() {
        let (token, solution) = generate_dmm_auth_pair().unwrap();
        assert!(token.contains('-'));
        assert!(!solution.is_empty());
    }

    #[test]
    fn torrent_response_accepts_float_file_sizes() {
        let json = r#"{
            "results": [
                {
                    "hash": "3977b980b0740b4d9fa3cd8e7eb3ad096d881443",
                    "title": "Frieren Season 1",
                    "fileSize": 253573.12
                }
            ]
        }"#;

        let parsed: DmmTorrentResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.results.len(), 1);
        assert_eq!(parsed.results[0].file_size, 253573);
    }
}

use std::collections::HashMap;

use anyhow::Result;
use quick_xml::de::from_str;
use serde::Deserialize;
use tracing::{debug, warn};

use crate::api::http;
use crate::api::sonarr::SonarrWantedMissingRecord;
use crate::config::Config;
use crate::db::Database;
use crate::models::{LibraryItem, MediaId};

const ANIME_LISTS_URL: &str =
    "https://raw.githubusercontent.com/Anime-Lists/anime-lists/master/anime-list.xml";
const ANIME_LISTS_CACHE_KEY: &str = "anime-lists:anime-list.xml:v1";
pub(crate) const ANIME_LISTS_CACHE_TTL_HOURS: u64 = 24 * 7;

#[derive(Debug, Clone)]
pub(crate) struct AnimeIdentityGraph {
    entries: Vec<AnimeIdentityEntry>,
    by_tvdb: HashMap<u64, Vec<usize>>,
    by_tmdb_tv: HashMap<u64, Vec<usize>>,
}

#[derive(Debug, Clone)]
struct AnimeIdentityEntry {
    canonical_title: String,
    tvdb_id: Option<u64>,
    default_tvdb_season: Option<u32>,
    episode_offset: Option<i32>,
    tmdb_tv_id: Option<u64>,
    tvdb_mappings: Vec<AnimeEpisodeMapping>,
}

#[derive(Debug, Clone)]
struct AnimeEpisodeMapping {
    anidb_season: u32,
    tvdb_season: u32,
    kind: AnimeEpisodeMappingKind,
}

#[derive(Debug, Clone)]
enum AnimeEpisodeMappingKind {
    Offset { start: u32, end: u32, offset: i32 },
    ExplicitPairs(Vec<(u32, u32)>),
}

#[derive(Debug, Deserialize)]
#[serde(rename = "anime-list")]
struct AnimeListXml {
    #[serde(rename = "anime", default)]
    anime: Vec<AnimeXmlEntry>,
}

#[derive(Debug, Deserialize)]
struct AnimeXmlEntry {
    #[serde(rename = "@tvdbid", default)]
    tvdb_id: String,
    #[serde(rename = "@defaulttvdbseason", default)]
    default_tvdb_season: String,
    #[serde(rename = "@episodeoffset", default)]
    episode_offset: String,
    #[serde(rename = "@tmdbtv", default)]
    tmdb_tv: String,
    #[serde(rename = "name", default)]
    name: String,
    #[serde(rename = "mapping-list", default)]
    mapping_list: Option<AnimeXmlMappingList>,
}

#[derive(Debug, Deserialize, Default)]
struct AnimeXmlMappingList {
    #[serde(rename = "mapping", default)]
    mappings: Vec<AnimeXmlMapping>,
}

#[derive(Debug, Deserialize)]
struct AnimeXmlMapping {
    #[serde(rename = "@anidbseason", default)]
    anidb_season: String,
    #[serde(rename = "@tvdbseason", default)]
    tvdb_season: String,
    #[serde(rename = "@start", default)]
    start: String,
    #[serde(rename = "@end", default)]
    end: String,
    #[serde(rename = "@offset", default)]
    offset: String,
    #[serde(rename = "$text", default)]
    pairs: String,
}

impl AnimeIdentityGraph {
    pub(crate) async fn load(cfg: &Config, db: &Database) -> Option<Self> {
        Self::load_with_ttl(db, cfg.api.cache_ttl_hours.min(ANIME_LISTS_CACHE_TTL_HOURS)).await
    }

    pub(crate) async fn load_with_ttl(db: &Database, ttl_hours: u64) -> Option<Self> {
        match Self::load_inner(db, ttl_hours).await {
            Ok(graph) => Some(graph),
            Err(err) => {
                warn!(
                    "Anime identity graph unavailable; continuing without anime-lists hints: {}",
                    err
                );
                None
            }
        }
    }

    async fn load_inner(db: &Database, ttl_hours: u64) -> Result<Self> {
        if let Some(cached) = db.get_cached(ANIME_LISTS_CACHE_KEY).await? {
            match Self::from_xml(&cached) {
                Ok(graph) => return Ok(graph),
                Err(err) => {
                    warn!(
                        "Cached anime-lists XML could not be parsed; invalidating and refetching: {}",
                        err
                    );
                    let _ = db.invalidate_cached(ANIME_LISTS_CACHE_KEY).await;
                }
            }
        }

        let fetched = fetch_anime_lists_xml().await?;
        db.set_cached(ANIME_LISTS_CACHE_KEY, &fetched, ttl_hours)
            .await?;
        Self::from_xml(&fetched)
    }

    pub(crate) fn from_xml(xml: &str) -> Result<Self> {
        let parsed: AnimeListXml = from_str(xml)?;
        let mut entries = Vec::new();
        let mut by_tvdb = HashMap::<u64, Vec<usize>>::new();
        let mut by_tmdb_tv = HashMap::<u64, Vec<usize>>::new();

        for entry in parsed.anime {
            let canonical_title = entry.name.trim().to_string();
            if canonical_title.is_empty() {
                continue;
            }

            let graph_entry = AnimeIdentityEntry {
                canonical_title,
                tvdb_id: parse_u64(&entry.tvdb_id),
                default_tvdb_season: parse_tvdb_season(&entry.default_tvdb_season),
                episode_offset: parse_i32(&entry.episode_offset),
                tmdb_tv_id: parse_u64(&entry.tmdb_tv),
                tvdb_mappings: parse_tvdb_mappings(entry.mapping_list),
            };

            let index = entries.len();
            if let Some(tvdb_id) = graph_entry.tvdb_id {
                by_tvdb.entry(tvdb_id).or_default().push(index);
            }
            if let Some(tmdb_tv_id) = graph_entry.tmdb_tv_id {
                by_tmdb_tv.entry(tmdb_tv_id).or_default().push(index);
            }
            entries.push(graph_entry);
        }

        Ok(Self {
            entries,
            by_tvdb,
            by_tmdb_tv,
        })
    }

    pub(crate) fn resolve_absolute_episode(
        &self,
        item: &LibraryItem,
        absolute_episode: u32,
    ) -> Option<(u32, u32)> {
        if absolute_episode == 0 {
            return None;
        }

        let entries = self.entries_for_item(item);
        if entries.is_empty() {
            return None;
        }

        let mut explicit_resolutions = Vec::new();
        for entry in &entries {
            if let Some(resolution) = entry.resolve_anidb_episode_explicit(absolute_episode) {
                if !explicit_resolutions.contains(&resolution) {
                    explicit_resolutions.push(resolution);
                }
            }
        }

        if explicit_resolutions.len() == 1 {
            return explicit_resolutions.into_iter().next();
        }
        if explicit_resolutions.len() > 1 {
            debug!(
                title = %item.title,
                absolute_episode,
                resolutions = ?explicit_resolutions,
                "ambiguous anime absolute-episode resolution via explicit mappings"
            );
            return None;
        }

        let default_resolutions = collect_unique_resolutions(
            entries
                .iter()
                .filter_map(|entry| entry.resolve_anidb_episode_default(absolute_episode)),
        );
        if default_resolutions.len() == 1 {
            return default_resolutions.into_iter().next();
        }
        if default_resolutions.len() > 1 {
            debug!(
                title = %item.title,
                absolute_episode,
                resolutions = ?default_resolutions,
                "ambiguous anime absolute-episode resolution via default season mapping"
            );
        }

        None
    }

    pub(crate) fn resolve_scene_episode(
        &self,
        item: &LibraryItem,
        anidb_season: u32,
        episode: u32,
    ) -> Option<(u32, u32)> {
        if episode == 0 {
            return None;
        }

        let entries = self.entries_for_item(item);
        if entries.is_empty() {
            return None;
        }

        let mut explicit_resolutions = Vec::new();
        for entry in &entries {
            if let Some(resolution) =
                entry.resolve_anidb_episode_explicit_for_season(anidb_season, episode)
            {
                if !explicit_resolutions.contains(&resolution) {
                    explicit_resolutions.push(resolution);
                }
            }
        }

        if explicit_resolutions.len() == 1 {
            return explicit_resolutions.into_iter().next();
        }
        if explicit_resolutions.len() > 1 {
            debug!(
                title = %item.title,
                anidb_season,
                episode,
                resolutions = ?explicit_resolutions,
                "ambiguous anime scene-episode resolution via explicit mappings"
            );
            return None;
        }

        let default_resolutions = collect_unique_resolutions(entries.iter().filter_map(|entry| {
            entry.resolve_anidb_episode_default_for_season(anidb_season, episode)
        }));
        if default_resolutions.len() == 1 {
            return default_resolutions.into_iter().next();
        }
        if default_resolutions.len() > 1 {
            debug!(
                title = %item.title,
                anidb_season,
                episode,
                resolutions = ?default_resolutions,
                "ambiguous anime scene-episode resolution via default season mapping"
            );
        }

        None
    }

    pub(crate) fn build_query_hints(
        &self,
        item: &LibraryItem,
        record: &SonarrWantedMissingRecord,
    ) -> Vec<String> {
        let Some(entry) = self.best_entry_for_request(item, record.season_number) else {
            return Vec::new();
        };

        let mut hints = Vec::new();
        let title = entry.canonical_title.as_str();
        let mapped_scene_slot = record
            .scene_season_number
            .zip(record.scene_episode_number)
            .or_else(|| {
                entry.resolve_tvdb_episode_slot(record.season_number, record.episode_number)
            });

        if let Some((scene_season, scene_episode)) = mapped_scene_slot {
            push_hint(
                &mut hints,
                format!("{} S{:02}E{:02}", title, scene_season, scene_episode),
            );
        }

        if let Some(absolute_episode) = record
            .scene_absolute_episode_number
            .or(record.absolute_episode_number)
            .or_else(|| {
                (record.season_number > 0)
                    .then_some(mapped_scene_slot)
                    .flatten()
                    .and_then(|(scene_season, scene_episode)| {
                        (scene_season == 1).then_some(scene_episode)
                    })
            })
        {
            push_hint(&mut hints, format!("{} {}", title, absolute_episode));
        }

        push_hint(
            &mut hints,
            format!(
                "{} S{:02}E{:02}",
                title, record.season_number, record.episode_number
            ),
        );

        hints
    }

    fn best_entry_for_request(
        &self,
        item: &LibraryItem,
        season_number: u32,
    ) -> Option<&AnimeIdentityEntry> {
        self.entries_for_item(item)
            .into_iter()
            .max_by_key(|entry| entry.match_score(season_number))
    }

    fn entries_for_item(&self, item: &LibraryItem) -> Vec<&AnimeIdentityEntry> {
        let indexes = match item.id {
            MediaId::Tvdb(id) => self.by_tvdb.get(&id),
            MediaId::Tmdb(id) => self.by_tmdb_tv.get(&id),
        };

        indexes
            .into_iter()
            .flat_map(|indexes| indexes.iter().copied())
            .filter_map(|index| self.entries.get(index))
            .collect()
    }
}

impl AnimeIdentityEntry {
    fn match_score(&self, season_number: u32) -> i64 {
        let mut score = 0;

        if self.default_tvdb_season == Some(season_number) {
            score += 1_000;
        } else if self
            .tvdb_mappings
            .iter()
            .any(|mapping| mapping.tvdb_season == season_number)
        {
            score += 700;
        } else if self.default_tvdb_season == Some(1) && season_number == 1 {
            score += 300;
        }

        score - self.canonical_title.len() as i64
    }

    fn resolve_tvdb_episode_slot(
        &self,
        season_number: u32,
        episode_number: u32,
    ) -> Option<(u32, u32)> {
        for mapping in &self.tvdb_mappings {
            if mapping.tvdb_season != season_number {
                continue;
            }

            match &mapping.kind {
                AnimeEpisodeMappingKind::Offset { start, end, offset } => {
                    let mapped_start = (*start as i64).saturating_add(i64::from(*offset));
                    let mapped_end = (*end as i64).saturating_add(i64::from(*offset));
                    let episode = i64::from(episode_number);
                    if episode >= mapped_start && episode <= mapped_end {
                        let resolved = episode.saturating_sub(i64::from(*offset));
                        if resolved > 0 {
                            return Some((mapping.anidb_season, resolved as u32));
                        }
                    }
                }
                AnimeEpisodeMappingKind::ExplicitPairs(pairs) => {
                    for (anidb_episode, tvdb_episode) in pairs {
                        if *tvdb_episode == episode_number && *anidb_episode > 0 {
                            return Some((mapping.anidb_season, *anidb_episode));
                        }
                    }
                }
            }
        }

        if self.default_tvdb_season == Some(season_number) {
            let resolved = i64::from(episode_number) - i64::from(self.episode_offset.unwrap_or(0));
            if resolved > 0 {
                let default_anidb_season = if season_number == 0 { 0 } else { 1 };
                return Some((default_anidb_season, resolved as u32));
            }
        }

        None
    }

    fn resolve_anidb_episode_explicit(&self, absolute_episode: u32) -> Option<(u32, u32)> {
        self.resolve_anidb_episode_explicit_for_season(1, absolute_episode)
    }

    fn resolve_anidb_episode_default(&self, absolute_episode: u32) -> Option<(u32, u32)> {
        self.resolve_anidb_episode_default_for_season(1, absolute_episode)
    }

    fn resolve_anidb_episode_explicit_for_season(
        &self,
        anidb_season: u32,
        episode: u32,
    ) -> Option<(u32, u32)> {
        let mut resolutions = Vec::new();

        for mapping in &self.tvdb_mappings {
            if mapping.anidb_season != anidb_season {
                continue;
            }

            match &mapping.kind {
                AnimeEpisodeMappingKind::Offset { start, end, offset } => {
                    if episode < *start || episode > *end {
                        continue;
                    }
                    let resolved = i64::from(episode) + i64::from(*offset);
                    if resolved > 0 {
                        let candidate = (mapping.tvdb_season, resolved as u32);
                        if !resolutions.contains(&candidate) {
                            resolutions.push(candidate);
                        }
                    }
                }
                AnimeEpisodeMappingKind::ExplicitPairs(pairs) => {
                    for (anidb_episode, tvdb_episode) in pairs {
                        if *anidb_episode == episode {
                            let candidate = (mapping.tvdb_season, *tvdb_episode);
                            if !resolutions.contains(&candidate) {
                                resolutions.push(candidate);
                            }
                        }
                    }
                }
            }
        }

        (resolutions.len() == 1)
            .then(|| resolutions.into_iter().next())
            .flatten()
    }

    fn resolve_anidb_episode_default_for_season(
        &self,
        anidb_season: u32,
        episode: u32,
    ) -> Option<(u32, u32)> {
        if anidb_season != 1 && !(anidb_season == 0 && self.default_tvdb_season == Some(0)) {
            return None;
        }

        let season = self.default_tvdb_season?;
        let resolved = i64::from(episode) + i64::from(self.episode_offset.unwrap_or(0));
        (resolved > 0).then_some((season, resolved as u32))
    }
}

fn parse_tvdb_mappings(mapping_list: Option<AnimeXmlMappingList>) -> Vec<AnimeEpisodeMapping> {
    let Some(mapping_list) = mapping_list else {
        return Vec::new();
    };

    let mut mappings = Vec::new();
    for mapping in mapping_list.mappings {
        let Some(anidb_season) = parse_u32_allow_zero(&mapping.anidb_season) else {
            continue;
        };
        let Some(tvdb_season) = parse_u32_allow_zero(&mapping.tvdb_season) else {
            continue;
        };

        if let Some(explicit_pairs) = parse_mapping_pairs(&mapping.pairs) {
            mappings.push(AnimeEpisodeMapping {
                anidb_season,
                tvdb_season,
                kind: AnimeEpisodeMappingKind::ExplicitPairs(explicit_pairs),
            });
            continue;
        }

        let (Some(start), Some(end), Some(offset)) = (
            parse_u32(&mapping.start),
            parse_u32(&mapping.end),
            parse_i32(&mapping.offset),
        ) else {
            continue;
        };
        mappings.push(AnimeEpisodeMapping {
            anidb_season,
            tvdb_season,
            kind: AnimeEpisodeMappingKind::Offset { start, end, offset },
        });
    }

    mappings
}

fn parse_mapping_pairs(raw: &str) -> Option<Vec<(u32, u32)>> {
    let mut pairs = Vec::new();

    for segment in raw
        .split(';')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
    {
        let Some((left, right)) = segment.split_once('-') else {
            continue;
        };
        let Some(anidb_episode) = parse_u32(left) else {
            continue;
        };
        for tvdb_episode in right.split('+').filter_map(parse_u32) {
            if tvdb_episode == 0 {
                continue;
            }
            pairs.push((anidb_episode, tvdb_episode));
        }
    }

    (!pairs.is_empty()).then_some(pairs)
}

async fn fetch_anime_lists_xml() -> Result<String> {
    let client = http::build_client();
    let response = http::send_with_retry(client.get(ANIME_LISTS_URL)).await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("anime-lists fetch failed with {}: {}", status, body);
    }

    Ok(response.text().await?)
}

fn push_hint(hints: &mut Vec<String>, candidate: String) {
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        return;
    }

    let normalized = crate::utils::normalize(trimmed);
    if normalized.is_empty()
        || hints
            .iter()
            .any(|existing| crate::utils::normalize(existing) == normalized)
    {
        return;
    }

    hints.push(trimmed.to_string());
}

fn collect_unique_resolutions(
    resolutions: impl IntoIterator<Item = (u32, u32)>,
) -> Vec<(u32, u32)> {
    let mut unique = Vec::new();
    for resolution in resolutions {
        if !unique.contains(&resolution) {
            unique.push(resolution);
        }
    }
    unique
}

fn parse_u64(value: &str) -> Option<u64> {
    value.trim().parse().ok().filter(|value| *value > 0)
}

fn parse_u32(value: &str) -> Option<u32> {
    value.trim().parse().ok().filter(|value| *value > 0)
}

fn parse_u32_allow_zero(value: &str) -> Option<u32> {
    value.trim().parse().ok()
}

fn parse_i32(value: &str) -> Option<i32> {
    value.trim().parse().ok()
}

fn parse_tvdb_season(value: &str) -> Option<u32> {
    parse_u32_allow_zero(value)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::ContentType;
    use crate::models::MediaType;

    const SAMPLE_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<anime-list>
  <anime anidbid="5" tvdbid="72025" defaulttvdbseason="3" tmdbtv="26209" tmdbseason="3">
    <name>Seikai no Senki II</name>
    <mapping-list>
      <mapping anidbseason="0" tvdbseason="0">;1-4;</mapping>
    </mapping-list>
  </anime>
  <anime anidbid="100" tvdbid="11111" defaulttvdbseason="1" episodeoffset="12">
    <name>Example Offset Show</name>
  </anime>
  <anime anidbid="101" tvdbid="22222" defaulttvdbseason="2">
    <name>Example Explicit Show 2</name>
    <mapping-list>
      <mapping anidbseason="1" tvdbseason="2">;1-1;2-2;3-3;4-4;</mapping>
    </mapping-list>
  </anime>
  <anime anidbid="102" tvdbid="33333" defaulttvdbseason="1" episodeoffset="-1">
    <name>Example Negative Offset Show</name>
  </anime>
  <anime anidbid="103" tvdbid="44444" defaulttvdbseason="1">
    <name>Example Shared TVDB Main</name>
  </anime>
  <anime anidbid="104" tvdbid="44444" defaulttvdbseason="0" episodeoffset="-5">
    <name>Example Shared TVDB Specials</name>
  </anime>
</anime-list>
"#;

    fn anime_item(id: MediaId) -> LibraryItem {
        LibraryItem {
            id,
            path: PathBuf::from("/library/Anime"),
            title: "Example".to_string(),
            library_name: "Anime".to_string(),
            media_type: MediaType::Tv,
            content_type: ContentType::Anime,
        }
    }

    fn record(season_number: u32, episode_number: u32) -> SonarrWantedMissingRecord {
        SonarrWantedMissingRecord {
            series_id: 1,
            tvdb_id: 0,
            season_number,
            episode_number,
            absolute_episode_number: None,
            scene_season_number: None,
            scene_episode_number: None,
            scene_absolute_episode_number: None,
            title: "Episode".to_string(),
            has_file: false,
            episode_file_id: None,
            air_date_utc: None,
            monitored: true,
        }
    }

    #[test]
    fn parses_anime_list_xml_into_lookup_graph() {
        let graph = AnimeIdentityGraph::from_xml(SAMPLE_XML).unwrap();

        let item = anime_item(MediaId::Tvdb(11111));
        let hints = graph.build_query_hints(&item, &record(1, 15));
        assert!(hints.contains(&"Example Offset Show 3".to_string()));
        assert!(hints.contains(&"Example Offset Show S01E15".to_string()));
    }

    #[test]
    fn explicit_mapping_inverts_tvdb_episode_back_to_anidb_absolute() {
        let graph = AnimeIdentityGraph::from_xml(SAMPLE_XML).unwrap();

        let item = anime_item(MediaId::Tvdb(22222));
        let hints = graph.build_query_hints(&item, &record(2, 4));
        assert!(hints.contains(&"Example Explicit Show 2 S01E04".to_string()));
        assert!(hints.contains(&"Example Explicit Show 2 4".to_string()));
    }

    #[test]
    fn special_mapping_resolves_scene_episode_without_absolute_hint() {
        let graph = AnimeIdentityGraph::from_xml(SAMPLE_XML).unwrap();

        let item = anime_item(MediaId::Tvdb(72025));
        assert_eq!(graph.resolve_scene_episode(&item, 0, 1), Some((0, 4)));

        let hints = graph.build_query_hints(&item, &record(0, 4));
        assert!(hints.contains(&"Seikai no Senki II S00E01".to_string()));
        assert!(hints.contains(&"Seikai no Senki II S00E04".to_string()));
        assert!(!hints.contains(&"Seikai no Senki II 4".to_string()));
    }

    #[test]
    fn positive_episode_offset_round_trips_between_anidb_and_tvdb_slots() {
        let graph = AnimeIdentityGraph::from_xml(SAMPLE_XML).unwrap();

        let item = anime_item(MediaId::Tvdb(11111));

        assert_eq!(graph.resolve_absolute_episode(&item, 3), Some((1, 15)));
        assert_eq!(graph.resolve_scene_episode(&item, 1, 3), Some((1, 15)));
    }

    #[test]
    fn negative_episode_offset_round_trips_between_anidb_and_tvdb_slots() {
        let graph = AnimeIdentityGraph::from_xml(SAMPLE_XML).unwrap();

        let item = anime_item(MediaId::Tvdb(33333));

        assert_eq!(graph.resolve_absolute_episode(&item, 2), Some((1, 1)));
        assert_eq!(graph.resolve_scene_episode(&item, 1, 2), Some((1, 1)));

        let hints = graph.build_query_hints(&item, &record(1, 1));
        assert!(hints.contains(&"Example Negative Offset Show 2".to_string()));
        assert!(hints.contains(&"Example Negative Offset Show S01E02".to_string()));
        assert!(hints.contains(&"Example Negative Offset Show S01E01".to_string()));
    }

    #[test]
    fn multi_entry_tvdb_ids_can_still_resolve_unique_default_absolute_episode() {
        let graph = AnimeIdentityGraph::from_xml(SAMPLE_XML).unwrap();

        let item = anime_item(MediaId::Tvdb(44444));

        assert_eq!(graph.resolve_absolute_episode(&item, 3), Some((1, 3)));
    }

    #[test]
    fn multi_entry_tvdb_ids_can_still_resolve_unique_default_scene_episode() {
        let graph = AnimeIdentityGraph::from_xml(SAMPLE_XML).unwrap();

        let item = anime_item(MediaId::Tvdb(44444));

        assert_eq!(graph.resolve_scene_episode(&item, 0, 6), Some((0, 1)));
    }
}

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::Utc;

use crate::anime_identity::AnimeIdentityGraph;
use crate::api::prowlarr;
use crate::api::sonarr::{SonarrClient, SonarrSeries, SonarrWantedMissingRecord};
use crate::api::tmdb::TmdbClient;
use crate::auto_acquire::{AutoAcquireRequest, RelinkCheck};
use crate::config::{Config, ContentType};
use crate::db::{AnimeSearchOverrideRecord, Database};
use crate::models::{LibraryItem, MediaId, MediaType};

pub(crate) enum AnimeEpisodeKind {
    Missing,
    CutoffUpgrade,
}

#[derive(Clone, Copy)]
pub(crate) struct AnimeAcquireContext<'a> {
    pub cfg: &'a Config,
    pub db: &'a Database,
    pub tmdb: Option<&'a TmdbClient>,
    pub anime_identity: Option<&'a AnimeIdentityGraph>,
}

pub(crate) async fn lookup_anime_series_imdb_id(
    tmdb: Option<&TmdbClient>,
    db: &Database,
    item: &LibraryItem,
    series: &SonarrSeries,
) -> Option<String> {
    let tmdb = tmdb?;

    if series.tmdb_id > 0 {
        return tmdb
            .get_tv_imdb_id(series.tmdb_id as u64, db)
            .await
            .ok()
            .flatten();
    }

    crate::commands::scan::lookup_item_imdb_id(Some(tmdb), db, item).await
}

pub(crate) async fn build_anime_episode_requests(
    kind: AnimeEpisodeKind,
    ctx: AnimeAcquireContext<'_>,
    library_items: &[LibraryItem],
    matched_media_ids: &HashSet<String>,
    limit: usize,
) -> Result<Vec<AutoAcquireRequest>> {
    let AnimeAcquireContext {
        cfg,
        db,
        tmdb,
        anime_identity,
    } = ctx;
    if limit == 0 || !cfg.has_sonarr_anime() {
        return Ok(Vec::new());
    }

    let mut item_by_tvdb = HashMap::<i64, &LibraryItem>::new();
    let mut item_by_tmdb = HashMap::<i64, &LibraryItem>::new();
    for item in library_items {
        if item.media_type != MediaType::Tv || item.content_type != ContentType::Anime {
            continue;
        }

        match &item.id {
            MediaId::Tvdb(id) => {
                item_by_tvdb.insert(*id as i64, item);
            }
            MediaId::Tmdb(id) => {
                item_by_tmdb.insert(*id as i64, item);
            }
        }
    }

    if item_by_tvdb.is_empty() && item_by_tmdb.is_empty() {
        return Ok(Vec::new());
    }

    let sonarr = SonarrClient::new(&cfg.sonarr_anime.url, &cfg.sonarr_anime.api_key);
    let mut series_by_id = HashMap::<i64, SonarrSeries>::new();
    let mut item_by_series_id = HashMap::<i64, &LibraryItem>::new();

    for series in sonarr.get_series().await? {
        let item = if series.tvdb_id > 0 {
            item_by_tvdb.get(&series.tvdb_id).copied()
        } else if series.tmdb_id > 0 {
            item_by_tmdb.get(&series.tmdb_id).copied()
        } else {
            None
        };

        let Some(item) = item else {
            continue;
        };

        item_by_series_id.insert(series.id, item);
        series_by_id.insert(series.id, series);
    }

    if item_by_series_id.is_empty() {
        return Ok(Vec::new());
    }

    const PAGE_SIZE: u32 = 500;
    const MAX_PAGES: u32 = 20;

    let mut requests = Vec::new();
    let mut queued_keys = HashSet::<String>::new();
    let mut page = 1u32;
    let override_by_media_id = db
        .list_anime_search_overrides()
        .await?
        .into_iter()
        .map(|entry| (entry.media_id.clone(), entry))
        .collect::<HashMap<_, _>>();

    while requests.len() < limit && page <= MAX_PAGES {
        let page_result = match kind {
            AnimeEpisodeKind::Missing => sonarr.get_wanted_missing_page(page, PAGE_SIZE).await?,
            AnimeEpisodeKind::CutoffUpgrade => {
                sonarr.get_wanted_cutoff_page(page, PAGE_SIZE).await?
            }
        };
        if page_result.records.is_empty() {
            break;
        }

        let total_records = page_result.total_records;
        let mut records = page_result.records;

        // For missing: sort so matched series come first
        if matches!(kind, AnimeEpisodeKind::Missing) {
            records.sort_by_key(|record| {
                let matched = item_by_series_id
                    .get(&record.series_id)
                    .map(|item| matched_media_ids.contains(&item.id.to_string()))
                    .unwrap_or(false);
                !matched
            });
        }

        for record in records {
            if requests.len() >= limit {
                break;
            }

            let has_file_filter = match kind {
                AnimeEpisodeKind::Missing => record.has_file, // skip if has file
                AnimeEpisodeKind::CutoffUpgrade => !record.has_file, // skip if no file
            };
            if !record.monitored || has_file_filter {
                continue;
            }
            if !wanted_episode_is_searchable(&record) {
                continue;
            }

            let Some(item) = item_by_series_id.get(&record.series_id).copied() else {
                continue;
            };
            let Some(series) = series_by_id.get(&record.series_id) else {
                continue;
            };
            if !wanted_episode_has_supported_numbering(&record) {
                continue;
            }

            let media_id = item.id.to_string();
            let has_active = db
                .has_active_link_for_episode(&media_id, record.season_number, record.episode_number)
                .await?;
            let skip_due_to_link = match kind {
                AnimeEpisodeKind::Missing => has_active, // already linked → skip
                AnimeEpisodeKind::CutoffUpgrade => !has_active, // not linked → skip
            };
            if skip_due_to_link {
                continue;
            }

            let relink_check = RelinkCheck::MediaEpisode {
                media_id: media_id.clone(),
                season: record.season_number,
                episode: record.episode_number,
            };
            let request_key = anime_missing_request_key(&relink_check)?;
            if !queued_keys.insert(request_key) {
                continue;
            }

            let label = match kind {
                AnimeEpisodeKind::Missing => {
                    if matched_media_ids.contains(&media_id) {
                        format!(
                            "{} S{:02}E{:02}",
                            item.title, record.season_number, record.episode_number
                        )
                    } else {
                        format!(
                            "{} S{:02}E{:02} (new)",
                            item.title, record.season_number, record.episode_number
                        )
                    }
                }
                AnimeEpisodeKind::CutoffUpgrade => {
                    if matched_media_ids.contains(&media_id) {
                        format!(
                            "{} S{:02}E{:02} upgrade",
                            item.title, record.season_number, record.episode_number
                        )
                    } else {
                        format!(
                            "{} S{:02}E{:02} upgrade (unlinked)",
                            item.title, record.season_number, record.episode_number
                        )
                    }
                }
            };
            let imdb_id = lookup_anime_series_imdb_id(tmdb, db, item, series).await;
            let search_override = override_by_media_id.get(&media_id);
            let Some(query) = build_anime_missing_search_query(series, &record, search_override)
            else {
                continue;
            };
            requests.push(AutoAcquireRequest {
                label,
                query,
                query_hints: anime_query_hints(item, &record, anime_identity, search_override),
                imdb_id,
                categories: vec![prowlarr::categories::TV_ANIME],
                arr: "sonarr-anime".to_string(),
                library_filter: Some(item.library_name.clone()),
                relink_check,
            });
        }

        if total_records <= page * PAGE_SIZE {
            break;
        }
        page += 1;
    }

    Ok(requests)
}

fn wanted_episode_is_searchable(record: &SonarrWantedMissingRecord) -> bool {
    if let Some(air_date) = record.air_date_utc {
        if air_date > Utc::now() {
            return false;
        }
    }

    true
}

fn wanted_episode_has_supported_numbering(record: &SonarrWantedMissingRecord) -> bool {
    record.episode_number > 0
}

fn anime_query_hints(
    item: &LibraryItem,
    record: &SonarrWantedMissingRecord,
    anime_identity: Option<&AnimeIdentityGraph>,
    search_override: Option<&AnimeSearchOverrideRecord>,
) -> Vec<String> {
    let mut hints = Vec::new();

    if let Some(search_override) = search_override {
        if let Some(preferred_title) = search_override.preferred_title.as_deref() {
            push_query_hint(&mut hints, preferred_title.to_string());
        }
        for hint in &search_override.extra_hints {
            push_query_hint(&mut hints, hint.clone());
        }
    }

    if let Some(graph) = anime_identity {
        for hint in graph.build_query_hints(item, record) {
            push_query_hint(&mut hints, hint);
        }
    }

    hints
}

fn build_anime_missing_search_query(
    series: &SonarrSeries,
    record: &SonarrWantedMissingRecord,
    search_override: Option<&AnimeSearchOverrideRecord>,
) -> Option<String> {
    let title = select_anime_query_title(series, record, search_override);
    let query = if let (Some(scene_season), Some(scene_episode)) =
        (record.scene_season_number, record.scene_episode_number)
    {
        format!("{} S{:02}E{:02}", title, scene_season, scene_episode)
    } else if let Some(absolute_episode) = (record.season_number > 0)
        .then(|| {
            record
                .scene_absolute_episode_number
                .or(record.absolute_episode_number)
        })
        .flatten()
    {
        format!("{} {}", title, absolute_episode)
    } else {
        format!(
            "{} S{:02}E{:02}",
            title, record.season_number, record.episode_number
        )
    };

    crate::commands::is_safe_auto_acquire_query(&query).then_some(query)
}

fn select_anime_query_title(
    series: &SonarrSeries,
    record: &SonarrWantedMissingRecord,
    search_override: Option<&AnimeSearchOverrideRecord>,
) -> String {
    if let Some(preferred_title) = search_override
        .and_then(|entry| entry.preferred_title.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return preferred_title.to_string();
    }

    let mut best = series.title.trim().to_string();
    let mut best_score = anime_query_title_score(
        &best,
        series.use_scene_numbering,
        record.scene_season_number,
        None,
    );

    for alternate in &series.alternate_titles {
        let candidate = alternate.title.trim();
        if candidate.is_empty() {
            continue;
        }

        let score = anime_query_title_score(
            candidate,
            series.use_scene_numbering,
            record.scene_season_number,
            Some(alternate.scene_season_number),
        );
        if score > best_score
            || (score == best_score
                && crate::utils::normalize(candidate).len() < crate::utils::normalize(&best).len())
        {
            best = candidate.to_string();
            best_score = score;
        }
    }

    best
}

fn push_query_hint(hints: &mut Vec<String>, candidate: String) {
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

fn anime_query_title_score(
    title: &str,
    use_scene_numbering: bool,
    wanted_scene_season: Option<u32>,
    alternate_scene_season: Option<i32>,
) -> i64 {
    let normalized = crate::utils::normalize(title);
    if normalized.is_empty() {
        return i64::MIN / 4;
    }

    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    let strong_words = tokens
        .iter()
        .filter(|token| token.chars().any(|c| c.is_ascii_alphabetic()) && token.len() >= 4)
        .count() as i64;
    let ascii_alpha = title.chars().any(|ch| ch.is_ascii_alphabetic());
    let has_meaningful_title = strong_words > 0 || normalized.len() >= 4;

    let mut score = strong_words * 100;
    score -= tokens.len() as i64 * 6;
    score -= normalized.len() as i64 / 8;
    if ascii_alpha {
        score += 15;
    }
    if !has_meaningful_title {
        score -= 420;
    }
    if use_scene_numbering {
        score += 10;
    }
    if let Some(scene_season) = alternate_scene_season {
        if use_scene_numbering && scene_season == -1 && has_meaningful_title {
            score += 320;
        }
        if use_scene_numbering
            && scene_season >= 0
            && wanted_scene_season == Some(scene_season as u32)
            && has_meaningful_title
        {
            score += 220;
        }
    }

    score
}

fn anime_missing_request_key(check: &RelinkCheck) -> Result<String> {
    match check {
        RelinkCheck::MediaEpisode {
            media_id,
            season,
            episode,
        } => Ok(format!("episode:{}:{}:{}", media_id, season, episode)),
        _ => anyhow::bail!("unexpected relink kind for anime missing request"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::sonarr::SonarrWantedMissingRecord;

    const SAMPLE_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<anime-list>
  <anime anidbid="100" tvdbid="11111" defaulttvdbseason="1" episodeoffset="12">
    <name>Example Offset Show</name>
  </anime>
</anime-list>
"#;

    #[test]
    fn anime_missing_query_prefers_scene_numbering_and_alt_title() {
        let series = SonarrSeries {
            id: 1,
            title: "The Yuzuki Family's Four Sons".to_string(),
            alternate_titles: vec![crate::api::sonarr::SonarrAlternateTitle {
                title: "Yuzuki-san Chi no Yonkyoudai".to_string(),
                scene_season_number: -1,
            }],
            tvdb_id: 434312,
            tmdb_id: 0,
            use_scene_numbering: true,
        };
        let record = SonarrWantedMissingRecord {
            series_id: 1,
            tvdb_id: 434312,
            season_number: 1,
            episode_number: 9,
            absolute_episode_number: Some(9),
            scene_season_number: Some(1),
            scene_episode_number: Some(9),
            scene_absolute_episode_number: Some(9),
            title: "Classroom Visitation".to_string(),
            has_file: false,
            episode_file_id: None,
            air_date_utc: None,
            monitored: true,
        };

        assert_eq!(
            build_anime_missing_search_query(&series, &record, None),
            Some("Yuzuki-san Chi no Yonkyoudai S01E09".to_string())
        );
    }

    #[test]
    fn anime_missing_query_falls_back_to_absolute_numbering() {
        let series = SonarrSeries {
            id: 2,
            title: "Jujutsu Kaisen".to_string(),
            alternate_titles: Vec::new(),
            tvdb_id: 0,
            tmdb_id: 0,
            use_scene_numbering: false,
        };
        let record = SonarrWantedMissingRecord {
            series_id: 2,
            tvdb_id: 0,
            season_number: 1,
            episode_number: 3,
            absolute_episode_number: Some(3),
            scene_season_number: None,
            scene_episode_number: None,
            scene_absolute_episode_number: None,
            title: "Episode 3".to_string(),
            has_file: false,
            episode_file_id: None,
            air_date_utc: None,
            monitored: true,
        };

        assert_eq!(
            build_anime_missing_search_query(&series, &record, None),
            Some("Jujutsu Kaisen 3".to_string())
        );
    }

    #[test]
    fn anime_missing_query_keeps_specials_in_s00_form() {
        let series = SonarrSeries {
            id: 3,
            title: "Attack on Titan OAD".to_string(),
            alternate_titles: Vec::new(),
            tvdb_id: 0,
            tmdb_id: 0,
            use_scene_numbering: false,
        };
        let record = SonarrWantedMissingRecord {
            series_id: 3,
            tvdb_id: 0,
            season_number: 0,
            episode_number: 2,
            absolute_episode_number: Some(2),
            scene_season_number: None,
            scene_episode_number: None,
            scene_absolute_episode_number: None,
            title: "Sudden Visitor".to_string(),
            has_file: false,
            episode_file_id: None,
            air_date_utc: None,
            monitored: true,
        };

        assert_eq!(
            build_anime_missing_search_query(&series, &record, None),
            Some("Attack on Titan OAD S00E02".to_string())
        );
    }

    #[test]
    fn anime_missing_query_uses_manual_override_title_when_present() {
        let series = SonarrSeries {
            id: 4,
            title: "Call of the Night".to_string(),
            alternate_titles: Vec::new(),
            tvdb_id: 0,
            tmdb_id: 0,
            use_scene_numbering: false,
        };
        let record = SonarrWantedMissingRecord {
            series_id: 4,
            tvdb_id: 0,
            season_number: 1,
            episode_number: 2,
            absolute_episode_number: Some(2),
            scene_season_number: None,
            scene_episode_number: None,
            scene_absolute_episode_number: None,
            title: "Episode 2".to_string(),
            has_file: false,
            episode_file_id: None,
            air_date_utc: None,
            monitored: true,
        };
        let search_override = AnimeSearchOverrideRecord {
            media_id: "tvdb-4".to_string(),
            preferred_title: Some("Yofukashi no Uta".to_string()),
            extra_hints: vec!["Call of the Night".to_string()],
            note: Some("Prefer scene title".to_string()),
            created_at: "2026-04-22 00:00:00".to_string(),
            updated_at: "2026-04-22 00:00:00".to_string(),
        };

        assert_eq!(
            build_anime_missing_search_query(&series, &record, Some(&search_override)),
            Some("Yofukashi no Uta 2".to_string())
        );
    }

    #[test]
    fn anime_query_hints_merge_override_and_identity_hints_without_duplicates() {
        let item = crate::models::LibraryItem {
            id: crate::models::MediaId::Tvdb(11111),
            path: std::path::PathBuf::from("/library/Anime"),
            title: "Example Offset Show".to_string(),
            library_name: "Anime".to_string(),
            media_type: crate::models::MediaType::Tv,
            content_type: crate::config::ContentType::Anime,
        };
        let record = SonarrWantedMissingRecord {
            series_id: 1,
            tvdb_id: 11111,
            season_number: 1,
            episode_number: 15,
            absolute_episode_number: None,
            scene_season_number: None,
            scene_episode_number: None,
            scene_absolute_episode_number: None,
            title: "Episode".to_string(),
            has_file: false,
            episode_file_id: None,
            air_date_utc: None,
            monitored: true,
        };
        let graph = AnimeIdentityGraph::from_xml(SAMPLE_XML).unwrap();
        let search_override = AnimeSearchOverrideRecord {
            media_id: "tvdb-11111".to_string(),
            preferred_title: Some("Example Offset Show".to_string()),
            extra_hints: vec![
                "Example Offset Show".to_string(),
                "Offset Alt".to_string(),
                "Offset Alt".to_string(),
            ],
            note: None,
            created_at: "2026-04-22 00:00:00".to_string(),
            updated_at: "2026-04-22 00:00:00".to_string(),
        };

        let hints = anime_query_hints(&item, &record, Some(&graph), Some(&search_override));

        assert!(hints.contains(&"Example Offset Show".to_string()));
        assert!(hints.contains(&"Offset Alt".to_string()));
        assert!(hints.contains(&"Example Offset Show 3".to_string()));
        assert_eq!(
            hints
                .iter()
                .filter(|hint| hint.as_str() == "Offset Alt")
                .count(),
            1
        );
    }

    #[test]
    fn wanted_episode_searchable_skips_future_airings() {
        let future = SonarrWantedMissingRecord {
            series_id: 2,
            tvdb_id: 0,
            season_number: 1,
            episode_number: 11,
            absolute_episode_number: Some(11),
            scene_season_number: None,
            scene_episode_number: None,
            scene_absolute_episode_number: None,
            title: "Future Episode".to_string(),
            has_file: false,
            episode_file_id: None,
            air_date_utc: Some(chrono::Utc::now() + chrono::Duration::hours(2)),
            monitored: true,
        };
        let past = SonarrWantedMissingRecord {
            air_date_utc: Some(chrono::Utc::now() - chrono::Duration::hours(2)),
            ..future.clone()
        };

        assert!(!wanted_episode_is_searchable(&future));
        assert!(wanted_episode_is_searchable(&past));
    }

    #[test]
    fn wanted_episode_supported_numbering_allows_specials() {
        let special = SonarrWantedMissingRecord {
            series_id: 2,
            tvdb_id: 0,
            season_number: 0,
            episode_number: 3,
            absolute_episode_number: None,
            scene_season_number: None,
            scene_episode_number: None,
            scene_absolute_episode_number: None,
            title: "Special".to_string(),
            has_file: false,
            episode_file_id: None,
            air_date_utc: None,
            monitored: true,
        };
        let invalid = SonarrWantedMissingRecord {
            episode_number: 0,
            ..special.clone()
        };

        assert!(wanted_episode_has_supported_numbering(&special));
        assert!(!wanted_episode_has_supported_numbering(&invalid));
    }

    #[test]
    fn anime_query_title_score_rejects_too_short_scene_aliases() {
        let short = anime_query_title_score("X", true, Some(1), Some(-1));
        let normal = anime_query_title_score("Gundam Wing", true, Some(1), Some(-1));

        assert!(normal > short);
    }
}

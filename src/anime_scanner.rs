use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::Utc;

use crate::api::prowlarr;
use crate::api::sonarr::{SonarrClient, SonarrSeries, SonarrWantedMissingRecord};
use crate::api::tmdb::TmdbClient;
use crate::auto_acquire::{AutoAcquireRequest, RelinkCheck};
use crate::config::{Config, ContentType};
use crate::db::Database;
use crate::models::{LibraryItem, MediaId, MediaType};

pub(crate) enum AnimeEpisodeKind {
    Missing,
    CutoffUpgrade,
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

    crate::lookup_item_imdb_id(Some(tmdb), db, item).await
}

pub(crate) async fn build_anime_episode_requests(
    kind: AnimeEpisodeKind,
    cfg: &Config,
    db: &Database,
    tmdb: Option<&TmdbClient>,
    library_items: &[LibraryItem],
    matched_media_ids: &HashSet<String>,
    limit: usize,
) -> Result<Vec<AutoAcquireRequest>> {
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
                AnimeEpisodeKind::Missing => record.has_file,         // skip if has file
                AnimeEpisodeKind::CutoffUpgrade => !record.has_file,  // skip if no file
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
            if record.season_number == 0 || record.episode_number == 0 {
                continue;
            }

            let media_id = item.id.to_string();
            let has_active = db
                .has_active_link_for_episode(&media_id, record.season_number, record.episode_number)
                .await?;
            let skip_due_to_link = match kind {
                AnimeEpisodeKind::Missing => has_active,         // already linked → skip
                AnimeEpisodeKind::CutoffUpgrade => !has_active,  // not linked → skip
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

            let Some(query) = build_anime_missing_search_query(series, &record) else {
                continue;
            };
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
            requests.push(AutoAcquireRequest {
                label,
                query,
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

fn build_anime_missing_search_query(
    series: &SonarrSeries,
    record: &SonarrWantedMissingRecord,
) -> Option<String> {
    let title = select_anime_query_title(series, record);
    let query = if let (Some(scene_season), Some(scene_episode)) =
        (record.scene_season_number, record.scene_episode_number)
    {
        format!("{} S{:02}E{:02}", title, scene_season, scene_episode)
    } else if let Some(absolute_episode) = record
        .scene_absolute_episode_number
        .or(record.absolute_episode_number)
    {
        format!("{} {}", title, absolute_episode)
    } else {
        format!(
            "{} S{:02}E{:02}",
            title, record.season_number, record.episode_number
        )
    };

    crate::is_safe_auto_acquire_query(&query).then_some(query)
}

fn select_anime_query_title(series: &SonarrSeries, record: &SonarrWantedMissingRecord) -> String {
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

    let mut score = strong_words * 100;
    score -= tokens.len() as i64 * 6;
    score -= normalized.len() as i64 / 8;
    if ascii_alpha {
        score += 15;
    }
    if use_scene_numbering {
        score += 10;
    }
    if let Some(scene_season) = alternate_scene_season {
        if use_scene_numbering && scene_season == -1 {
            score += 320;
        }
        if use_scene_numbering
            && scene_season >= 0
            && wanted_scene_season == Some(scene_season as u32)
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
            build_anime_missing_search_query(&series, &record),
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
            build_anime_missing_search_query(&series, &record),
            Some("Jujutsu Kaisen 3".to_string())
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
}

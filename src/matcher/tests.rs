
use super::*;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

use crate::anime_identity::AnimeIdentityGraph;
use crate::db::Database;
use crate::models::{EpisodeInfo, SeasonInfo};

fn metadata_with_seasons(seasons: &[(u32, u32)]) -> ContentMetadata {
    ContentMetadata {
        title: "Example Anime".to_string(),
        aliases: Vec::new(),
        year: Some(2024),
        seasons: seasons
            .iter()
            .map(|(season_number, episode_count)| SeasonInfo {
                season_number: *season_number,
                episodes: (1..=*episode_count)
                    .map(|episode_number| EpisodeInfo {
                        episode_number,
                        title: format!("Episode {}", episode_number),
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn candidate(path: &str, score: f64) -> MatchCandidate {
    MatchCandidate {
        source_idx: 0,
        library_idx: 0,
        media_id: "tvdb-1".to_string(),
        score,
        alias: "x".to_string(),
        source_item: SourceItem {
            path: PathBuf::from(path),
            parsed_title: "title".to_string(),
            season: Some(1),
            episode: Some(1),
            episode_end: None,
            quality: None,
            extension: "mkv".to_string(),
            year: None,
        },
    }
}

fn candidate_with_quality(path: &str, score: f64, quality: Option<&str>) -> MatchCandidate {
    let mut candidate = candidate(path, score);
    candidate.source_item.quality = quality.map(|value| value.to_string());
    candidate
}

fn movie_item(title: &str, tmdb_id: u64) -> LibraryItem {
    LibraryItem {
        title: title.to_string(),
        path: PathBuf::from(format!("/library/{title} {{tmdb-{tmdb_id}}}")),
        id: MediaId::Tmdb(tmdb_id),
        library_name: "Movies".to_string(),
        media_type: MediaType::Movie,
        content_type: ContentType::Movie,
    }
}

fn tv_item(title: &str, tvdb_id: u64) -> LibraryItem {
    LibraryItem {
        title: title.to_string(),
        path: PathBuf::from(format!("/library/{title} {{tvdb-{tvdb_id}}}")),
        id: MediaId::Tvdb(tvdb_id),
        library_name: "Series".to_string(),
        media_type: MediaType::Tv,
        content_type: ContentType::Tv,
    }
}

fn tv_metadata(title: &str, year: u32, seasons: &[u32]) -> ContentMetadata {
    ContentMetadata {
        title: title.to_string(),
        aliases: Vec::new(),
        year: Some(year),
        seasons: seasons
            .iter()
            .map(|season_number| SeasonInfo {
                season_number: *season_number,
                episodes: vec![EpisodeInfo {
                    episode_number: 1,
                    title: "Episode 1".to_string(),
                }],
            })
            .collect(),
    }
}

fn movie_metadata(title: &str, year: u32) -> ContentMetadata {
    ContentMetadata {
        title: title.to_string(),
        aliases: Vec::new(),
        year: Some(year),
        seasons: Vec::new(),
    }
}

fn anime_item(title: &str, tvdb_id: u64) -> LibraryItem {
    LibraryItem {
        title: title.to_string(),
        path: PathBuf::from(format!("/library/{title} {{tvdb-{tvdb_id}}}")),
        id: MediaId::Tvdb(tvdb_id),
        library_name: "Anime".to_string(),
        media_type: MediaType::Tv,
        content_type: ContentType::Anime,
    }
}

fn parsed_standard_source(path: &str) -> SourceItem {
    SourceScanner::new()
        .parse_filename_with_type(Path::new(path), ContentType::Tv)
        .expect("expected source to parse")
}

fn parsed_movie_source(path: &str) -> SourceItem {
    SourceScanner::new()
        .parse_filename_with_type(Path::new(path), ContentType::Movie)
        .expect("expected movie source to parse")
}

#[test]
fn test_strict_no_substring_group_collision() {
    let aliases = vec!["show".to_string()];
    assert!(best_alias_score(MatchingMode::Strict, &aliases, "showgroup 03").is_none());
    assert!(best_alias_score(MatchingMode::Strict, &aliases, "show").is_some());
}

#[test]
fn test_parser_kind_for_content_type() {
    assert_eq!(
        parser_kind_for_content(ContentType::Anime),
        ParserKind::Anime
    );
    assert_eq!(
        parser_kind_for_content(ContentType::Tv),
        ParserKind::Standard
    );
}

#[test]
fn test_ambiguous_rejected_in_strict() {
    let candidates = vec![candidate("/a", 0.90), candidate("/b", 0.85)];
    assert!(should_reject_ambiguous(MatchingMode::Strict, &candidates));
    assert!(!should_reject_ambiguous(
        MatchingMode::Aggressive,
        &candidates
    ));
}

#[test]
fn test_destination_conflict_keeps_higher_score() {
    let existing = candidate("/old", 0.81);
    let challenger = candidate("/new", 0.92);
    assert!(should_replace_destination(&existing, &challenger));
}

#[test]
fn test_destination_conflict_tie_is_deterministic() {
    let existing = candidate("/z-path", 0.90);
    let challenger = candidate("/a-path", 0.90);
    assert!(should_replace_destination(&existing, &challenger));
}

#[test]
fn test_destination_conflict_prefers_higher_quality_when_scores_tie() {
    let existing = candidate_with_quality("/z-path", 0.90, Some("720p"));
    let challenger = candidate_with_quality("/a-path", 0.90, Some("1080p"));
    assert!(should_replace_destination(&existing, &challenger));
}

#[test]
fn test_candidate_prefilter_matches_relevant_library_indices() {
    let mut alias_map = HashMap::new();
    alias_map.insert(0usize, vec!["breaking bad".to_string()]);
    alias_map.insert(1usize, vec!["game of thrones".to_string()]);
    alias_map.insert(2usize, vec!["jujutsu kaisen".to_string()]);
    let index = build_alias_token_index(&alias_map);

    let mut variants = HashMap::new();
    variants.insert(
        ParserKind::Standard,
        SourceItem {
            path: PathBuf::from("/rd/Breaking.Bad.S01E01.mkv"),
            parsed_title: "Breaking Bad".to_string(),
            season: Some(1),
            episode: Some(1),
            episode_end: None,
            quality: None,
            extension: "mkv".to_string(),
            year: None,
        },
    );

    let indices = candidate_library_indices(&variants, &index, 3, true);
    assert_eq!(indices, vec![0]);
}

#[test]
fn test_candidate_prefilter_falls_back_to_all_when_no_token_hit() {
    let mut alias_map = HashMap::new();
    alias_map.insert(0usize, vec!["breaking bad".to_string()]);
    alias_map.insert(1usize, vec!["game of thrones".to_string()]);
    let index = build_alias_token_index(&alias_map);

    let mut variants = HashMap::new();
    variants.insert(
        ParserKind::Standard,
        SourceItem {
            path: PathBuf::from("/rd/Some.Unknown.Show.S01E01.mkv"),
            parsed_title: "Completely Unknown".to_string(),
            season: Some(1),
            episode: Some(1),
            episode_end: None,
            quality: None,
            extension: "mkv".to_string(),
            year: None,
        },
    );

    let indices = candidate_library_indices(&variants, &index, 2, true);
    assert_eq!(indices, vec![0, 1]);
}

#[test]
fn test_candidate_prefilter_can_skip_global_fallback() {
    let mut alias_map = HashMap::new();
    alias_map.insert(0usize, vec!["breaking bad".to_string()]);
    alias_map.insert(1usize, vec!["game of thrones".to_string()]);
    let index = build_alias_token_index(&alias_map);

    let mut variants = HashMap::new();
    variants.insert(
        ParserKind::Standard,
        SourceItem {
            path: PathBuf::from("/rd/Some.Unknown.Show.S01E01.mkv"),
            parsed_title: "Completely Unknown".to_string(),
            season: Some(1),
            episode: Some(1),
            episode_end: None,
            quality: None,
            extension: "mkv".to_string(),
            year: None,
        },
    );

    let indices = candidate_library_indices(&variants, &index, 2, false);
    assert!(indices.is_empty());
}

#[test]
fn test_resolve_anime_episode_mapping_single_season_bare_episode() {
    let metadata = metadata_with_seasons(&[(1, 12)]);
    assert_eq!(
        resolve_anime_episode_mapping(Some(&metadata), 3),
        Some((1, 3))
    );
}

#[test]
fn test_resolve_anime_episode_mapping_multi_season_absolute_numbering() {
    let metadata = metadata_with_seasons(&[(1, 12), (2, 12)]);
    assert_eq!(
        resolve_anime_episode_mapping(Some(&metadata), 21),
        Some((2, 9))
    );
}

#[test]
fn test_resolve_anime_episode_mapping_prefers_unique_high_episode_local_match() {
    let metadata = metadata_with_seasons(&[(1, 12), (20, 130)]);
    assert_eq!(
        resolve_anime_episode_mapping(Some(&metadata), 129),
        Some((20, 129))
    );
}

#[test]
fn test_resolve_anime_scene_episode_mapping_falls_back_to_absolute_episode() {
    let metadata = metadata_with_seasons(&[(1, 12), (20, 130)]);
    let item = anime_item("Example Anime", 456);
    assert_eq!(
        resolve_anime_scene_episode_mapping(&item, Some(&metadata), None, 25, 129),
        Some((20, 129))
    );
}

#[test]
fn test_resolve_anime_scene_episode_mapping_does_not_treat_in_season_numbers_as_absolute() {
    let metadata = metadata_with_seasons(&[(1, 12), (3, 12), (20, 130)]);
    let item = anime_item("Example Anime", 456);
    assert_eq!(
        resolve_anime_scene_episode_mapping(&item, Some(&metadata), None, 3, 129),
        None
    );
}

#[test]
fn test_resolve_source_for_library_item_uses_anime_identity_for_absolute_numbering() {
    let metadata = metadata_with_seasons(&[(1, 12), (2, 12)]);
    let item = anime_item("Example Anime", 22222);
    let parsed = SourceItem {
        path: PathBuf::from("/rd/[SubsPlease] Example Anime - 15.mkv"),
        parsed_title: "Example Anime".to_string(),
        season: None,
        episode: Some(15),
        episode_end: None,
        quality: None,
        extension: "mkv".to_string(),
        year: None,
    };
    let graph = AnimeIdentityGraph::from_xml(
        r#"<?xml version="1.0" encoding="utf-8"?>
<anime-list>
  <anime anidbid="101" tvdbid="22222" defaulttvdbseason="2">
    <name>Example Anime</name>
    <mapping-list>
      <mapping anidbseason="1" tvdbseason="2">;13-1;14-2;15-3;</mapping>
    </mapping-list>
  </anime>
</anime-list>
"#,
    )
    .unwrap();

    let resolved =
        resolve_source_for_library_item(&item, &parsed, Some(&metadata), Some(&graph)).unwrap();
    assert_eq!(resolved.season, Some(2));
    assert_eq!(resolved.episode, Some(3));
}

#[test]
fn test_resolve_source_for_library_item_prefers_anime_identity_scene_mapping_over_metadata() {
    let metadata = metadata_with_seasons(&[(1, 24), (2, 12)]);
    let item = anime_item("Example Anime", 33333);
    let parsed = SourceItem {
        path: PathBuf::from("/rd/[SubsPlease] Example Anime S01E15.mkv"),
        parsed_title: "Example Anime".to_string(),
        season: Some(1),
        episode: Some(15),
        episode_end: None,
        quality: None,
        extension: "mkv".to_string(),
        year: None,
    };
    let graph = AnimeIdentityGraph::from_xml(
        r#"<?xml version="1.0" encoding="utf-8"?>
<anime-list>
  <anime anidbid="102" tvdbid="33333" defaulttvdbseason="1">
    <name>Example Anime</name>
    <mapping-list>
      <mapping anidbseason="1" tvdbseason="2">;15-3;</mapping>
    </mapping-list>
  </anime>
</anime-list>
"#,
    )
    .unwrap();

    let resolved =
        resolve_source_for_library_item(&item, &parsed, Some(&metadata), Some(&graph)).unwrap();
    assert_eq!(resolved.season, Some(2));
    assert_eq!(resolved.episode, Some(3));
}

#[test]
fn test_movie_source_shape_rejects_episode_like_source() {
    let item = movie_item("The Avengers", 24428);
    let source = parsed_standard_source("/rd/Avengers.Assemble.S01E01.mkv");
    assert!(!source_shape_matches_media_type(&item, &source));
}

#[test]
fn test_match_source_slice_skips_movie_candidate_for_episode_source() {
    let library_items = vec![movie_item("The Avengers", 24428)];
    let source_items = vec![parsed_standard_source("/rd/Avengers.Assemble.S01E01.mkv")];

    let mut alias_map = HashMap::new();
    alias_map.insert(0usize, vec!["avengers assemble".to_string()]);
    let mut metadata_map = HashMap::new();
    metadata_map.insert(0usize, None);
    let alias_token_index = build_alias_token_index(&alias_map);

    let chunk = match_source_slice(
        0,
        &source_items,
        &library_items,
        &alias_map,
        &metadata_map,
        &alias_token_index,
        MatchingMode::Strict,
        true,
        None,
    );

    assert!(chunk.best_per_source.is_empty());
    assert_eq!(
        chunk.skip_reasons.get("matcher_media_shape_mismatch"),
        Some(&1)
    );
}

#[test]
fn test_exact_id_path_respects_movie_episode_shape_guard() {
    let library_items = vec![movie_item("The Avengers", 24428)];
    let source_items = vec![parsed_standard_source(
        "/rd/tmdb-24428/Avengers.Assemble.S01E01.mkv",
    )];

    let mut alias_map = HashMap::new();
    alias_map.insert(0usize, vec!["avengers assemble".to_string()]);
    let mut metadata_map = HashMap::new();
    metadata_map.insert(0usize, None);
    let alias_token_index = build_alias_token_index(&alias_map);

    let chunk = match_source_slice(
        0,
        &source_items,
        &library_items,
        &alias_map,
        &metadata_map,
        &alias_token_index,
        MatchingMode::Strict,
        true,
        None,
    );

    assert!(chunk.best_per_source.is_empty());
    assert_eq!(chunk.exact_id_hits, 0);
}

#[test]
fn test_source_path_contains_media_id_requires_boundaries() {
    assert!(source_path_contains_media_id(
        "/rd/tmdb-24428/Movie.mkv",
        "tmdb-24428"
    ));
    assert!(source_path_contains_media_id(
        "/rd/[tmdb-24428]/Movie.mkv",
        "tmdb-24428"
    ));
    assert!(!source_path_contains_media_id(
        "/rd/tmdb-244281/Movie.mkv",
        "tmdb-24428"
    ));
    assert!(!source_path_contains_media_id(
        "/rd/pretmdb-24428/Movie.mkv",
        "tmdb-24428"
    ));
}

#[test]
fn test_exact_id_path_does_not_match_prefix_of_longer_id() {
    let library_items = vec![movie_item("Example", 60), movie_item("Example", 6043)];
    let source_items = vec![parsed_standard_source("/rd/tmdb-6043/Example.2024.mkv")];

    let mut alias_map = HashMap::new();
    alias_map.insert(0usize, vec!["example".to_string()]);
    alias_map.insert(1usize, vec!["example".to_string()]);
    let mut metadata_map = HashMap::new();
    metadata_map.insert(0usize, None);
    metadata_map.insert(1usize, None);
    let alias_token_index = build_alias_token_index(&alias_map);

    let chunk = match_source_slice(
        0,
        &source_items,
        &library_items,
        &alias_map,
        &metadata_map,
        &alias_token_index,
        MatchingMode::Strict,
        true,
        None,
    );

    assert_eq!(chunk.best_per_source.len(), 1);
    assert_eq!(chunk.best_per_source[0].media_id, "tmdb-6043");
    assert_eq!(chunk.exact_id_hits, 1);
}

#[test]
fn test_tv_same_title_year_selects_matching_series() {
    let library_items = vec![
        tv_item("Dark Matter (2024)", 2024),
        tv_item("Dark Matter (2015)", 2015),
    ];
    let source_items = vec![parsed_standard_source("/rd/Dark.Matter.2024.S01E01.mkv")];

    let mut alias_map = HashMap::new();
    alias_map.insert(
        0usize,
        vec!["dark matter 2024".to_string(), "dark matter".to_string()],
    );
    alias_map.insert(
        1usize,
        vec!["dark matter 2015".to_string(), "dark matter".to_string()],
    );

    let mut metadata_map = HashMap::new();
    metadata_map.insert(0usize, Some(tv_metadata("Dark Matter", 2024, &[1])));
    metadata_map.insert(1usize, Some(tv_metadata("Dark Matter", 2015, &[1, 2, 3])));

    let alias_token_index = build_alias_token_index(&alias_map);
    let chunk = match_source_slice(
        0,
        &source_items,
        &library_items,
        &alias_map,
        &metadata_map,
        &alias_token_index,
        MatchingMode::Strict,
        true,
        None,
    );

    assert_eq!(chunk.best_per_source.len(), 1);
    assert_eq!(chunk.best_per_source[0].media_id, "tvdb-2024");
}

#[test]
fn test_tv_wrong_year_candidate_rejected_when_correct_show_lacks_plain_alias() {
    let library_items = vec![
        tv_item("Dark Matter (2024)", 2024),
        tv_item("Dark Matter (2015)", 2015),
    ];
    let source_items = vec![parsed_standard_source("/rd/Dark.Matter.2024.S01E01.mkv")];

    let mut alias_map = HashMap::new();
    alias_map.insert(0usize, vec!["dark matter 2024".to_string()]);
    alias_map.insert(
        1usize,
        vec!["dark matter 2015".to_string(), "dark matter".to_string()],
    );

    let mut metadata_map = HashMap::new();
    metadata_map.insert(0usize, None);
    metadata_map.insert(1usize, Some(tv_metadata("Dark Matter", 2015, &[1, 2, 3])));

    let alias_token_index = build_alias_token_index(&alias_map);
    let chunk = match_source_slice(
        0,
        &source_items,
        &library_items,
        &alias_map,
        &metadata_map,
        &alias_token_index,
        MatchingMode::Strict,
        true,
        None,
    );

    assert!(chunk.best_per_source.is_empty());
}

#[test]
fn test_match_source_slice_records_no_library_candidates_reason() {
    let library_items = vec![anime_item("Example Anime", 1234)];
    let source_items = vec![parsed_standard_source(
        "/rd/Completely.Different.S01E01.mkv",
    )];

    let mut alias_map = HashMap::new();
    alias_map.insert(0usize, vec!["example anime".to_string()]);
    let mut metadata_map = HashMap::new();
    metadata_map.insert(0usize, Some(tv_metadata("Example Anime", 2024, &[1])));
    let alias_token_index = build_alias_token_index(&alias_map);

    let chunk = match_source_slice(
        0,
        &source_items,
        &library_items,
        &alias_map,
        &metadata_map,
        &alias_token_index,
        MatchingMode::Strict,
        false,
        None,
    );

    assert!(chunk.best_per_source.is_empty());
    assert_eq!(
        chunk.skip_reasons.get("matcher_no_library_candidates"),
        Some(&1)
    );
}

#[test]
fn test_match_source_slice_records_metadata_mismatch_reason() {
    let library_items = vec![movie_item("The Thing (1982)", 1982)];
    let source_items = vec![parsed_movie_source(
        "/rd/The.Thing.2011.1080p.BluRay.x264-GROUP.mkv",
    )];

    let mut alias_map = HashMap::new();
    alias_map.insert(0usize, vec!["the thing".to_string()]);
    let mut metadata_map = HashMap::new();
    metadata_map.insert(0usize, Some(movie_metadata("The Thing", 1982)));
    let alias_token_index = build_alias_token_index(&alias_map);

    let chunk = match_source_slice(
        0,
        &source_items,
        &library_items,
        &alias_map,
        &metadata_map,
        &alias_token_index,
        MatchingMode::Strict,
        true,
        None,
    );

    assert!(chunk.best_per_source.is_empty());
    assert_eq!(
        chunk.skip_reasons.get("matcher_metadata_mismatch"),
        Some(&1)
    );
}

#[test]
fn test_match_source_slice_records_alias_threshold_reason() {
    let library_items = vec![movie_item("The Avengers", 24428)];
    let source_items = vec![parsed_movie_source("/rd/Avengers.of.Ultron.2015.mkv")];

    let mut alias_map = HashMap::new();
    alias_map.insert(0usize, vec!["the avengers".to_string()]);
    let mut metadata_map = HashMap::new();
    metadata_map.insert(0usize, None);
    let alias_token_index = build_alias_token_index(&alias_map);

    let chunk = match_source_slice(
        0,
        &source_items,
        &library_items,
        &alias_map,
        &metadata_map,
        &alias_token_index,
        MatchingMode::Strict,
        true,
        None,
    );

    assert!(chunk.best_per_source.is_empty());
    assert_eq!(
        chunk
            .skip_reasons
            .get("matcher_alias_score_below_threshold"),
        Some(&1)
    );
}

#[test]
fn test_tv_same_title_season_guard_prefers_series_with_known_season() {
    let library_items = vec![
        tv_item("Dark Matter (2024)", 2024),
        tv_item("Dark Matter (2015)", 2015),
    ];
    let source_items = vec![parsed_standard_source("/rd/Dark.Matter.S03E01.mkv")];

    let mut alias_map = HashMap::new();
    alias_map.insert(
        0usize,
        vec!["dark matter 2024".to_string(), "dark matter".to_string()],
    );
    alias_map.insert(
        1usize,
        vec!["dark matter 2015".to_string(), "dark matter".to_string()],
    );

    let mut metadata_map = HashMap::new();
    metadata_map.insert(0usize, Some(tv_metadata("Dark Matter", 2024, &[1])));
    metadata_map.insert(1usize, Some(tv_metadata("Dark Matter", 2015, &[1, 2, 3])));

    let alias_token_index = build_alias_token_index(&alias_map);
    let chunk = match_source_slice(
        0,
        &source_items,
        &library_items,
        &alias_map,
        &metadata_map,
        &alias_token_index,
        MatchingMode::Strict,
        true,
        None,
    );

    assert_eq!(chunk.best_per_source.len(), 1);
    assert_eq!(chunk.best_per_source[0].media_id, "tvdb-2015");
}

#[test]
fn test_movie_same_title_year_selects_matching_release() {
    let library_items = vec![
        movie_item("The Thing (1982)", 1982),
        movie_item("The Thing (2011)", 2011),
    ];
    let source_items = vec![parsed_movie_source(
        "/rd/The.Thing.2011.1080p.BluRay.x264-GROUP.mkv",
    )];

    let mut alias_map = HashMap::new();
    alias_map.insert(
        0usize,
        vec!["the thing 1982".to_string(), "the thing".to_string()],
    );
    alias_map.insert(
        1usize,
        vec!["the thing 2011".to_string(), "the thing".to_string()],
    );

    let mut metadata_map = HashMap::new();
    metadata_map.insert(0usize, Some(movie_metadata("The Thing", 1982)));
    metadata_map.insert(1usize, Some(movie_metadata("The Thing", 2011)));

    let alias_token_index = build_alias_token_index(&alias_map);
    let chunk = match_source_slice(
        0,
        &source_items,
        &library_items,
        &alias_map,
        &metadata_map,
        &alias_token_index,
        MatchingMode::Strict,
        true,
        None,
    );

    assert_eq!(chunk.best_per_source.len(), 1);
    assert_eq!(chunk.best_per_source[0].media_id, "tmdb-2011");
}

#[tokio::test]
async fn test_metadata_off_returns_none() {
    let tmp = tempdir().unwrap();
    let db = Database::new(tmp.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let item = LibraryItem {
        title: "Example Show".to_string(),
        path: PathBuf::from("/library/Example Show {tmdb-123}"),
        id: MediaId::Tmdb(123),
        library_name: "Series".to_string(),
        media_type: MediaType::Tv,
        content_type: ContentType::Tv,
    };

    let matcher = Matcher::new(None, None, MatchingMode::Strict, MetadataMode::Off, 1);
    let metadata = matcher.get_metadata(&item, &db).await.unwrap();
    assert!(metadata.is_none());
}

#[tokio::test]
async fn test_metadata_cache_only_reads_cached_entry() {
    let tmp = tempdir().unwrap();
    let db = Database::new(tmp.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let cached = ContentMetadata {
        title: "Example Show".to_string(),
        aliases: vec!["Example Alias".to_string()],
        year: Some(2024),
        seasons: vec![],
    };
    db.set_cached("tmdb:tv:123", &serde_json::to_string(&cached).unwrap(), 24)
        .await
        .unwrap();

    let item = LibraryItem {
        title: "Example Show".to_string(),
        path: PathBuf::from("/library/Example Show {tmdb-123}"),
        id: MediaId::Tmdb(123),
        library_name: "Series".to_string(),
        media_type: MediaType::Tv,
        content_type: ContentType::Tv,
    };

    let matcher = Matcher::new(None, None, MatchingMode::Strict, MetadataMode::CacheOnly, 1);
    let metadata = matcher.get_metadata(&item, &db).await.unwrap();
    assert_eq!(metadata.unwrap().aliases, vec!["Example Alias".to_string()]);
}

#[tokio::test]
async fn test_negative_metadata_cache_skips_remote_lookup() {
    let tmp = tempdir().unwrap();
    let db = Database::new(tmp.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    db.set_cached(
        "tvdb:series:456",
        r#"{"_symlinkarr_not_found":true,"title":"","aliases":[],"year":null,"seasons":[]}"#,
        24,
    )
    .await
    .unwrap();

    let item = LibraryItem {
        title: "Example Anime".to_string(),
        path: PathBuf::from("/library/Example Anime {tvdb-456}"),
        id: MediaId::Tvdb(456),
        library_name: "Anime".to_string(),
        media_type: MediaType::Tv,
        content_type: ContentType::Anime,
    };

    let matcher = Matcher::new(None, None, MatchingMode::Strict, MetadataMode::Full, 1);
    let metadata = matcher.get_metadata(&item, &db).await.unwrap();
    assert!(metadata.is_none());
}

#[test]
fn test_expand_episode_slots_single() {
    let source = SourceItem {
        path: PathBuf::from("/rd/Show.S01E05.mkv"),
        parsed_title: "show".to_string(),
        season: Some(1),
        episode: Some(5),
        episode_end: None,
        quality: None,
        extension: "mkv".to_string(),
        year: None,
    };
    assert_eq!(expand_episode_slots(&source), vec![5]);
}

#[test]
fn test_expand_episode_slots_multi() {
    let source = SourceItem {
        path: PathBuf::from("/rd/Show.S01E01E02E03.mkv"),
        parsed_title: "show".to_string(),
        season: Some(1),
        episode: Some(1),
        episode_end: Some(3),
        quality: None,
        extension: "mkv".to_string(),
        year: None,
    };
    assert_eq!(expand_episode_slots(&source), vec![1, 2, 3]);
}

#[test]
fn test_expand_episode_slots_movie() {
    let source = SourceItem {
        path: PathBuf::from("/rd/Movie.2024.mkv"),
        parsed_title: "movie".to_string(),
        season: None,
        episode: None,
        episode_end: None,
        quality: None,
        extension: "mkv".to_string(),
        year: Some(2024),
    };
    assert!(expand_episode_slots(&source).is_empty());
}

#[test]
fn test_expand_episode_slots_caps_pathological_range() {
    let source = SourceItem {
        path: PathBuf::from("/rd/Show.S01E01-E999.mkv"),
        parsed_title: "show".to_string(),
        season: Some(1),
        episode: Some(1),
        episode_end: Some(999),
        quality: None,
        extension: "mkv".to_string(),
        year: None,
    };
    // Should fall back to first episode only due to cap
    assert_eq!(expand_episode_slots(&source), vec![1]);
}

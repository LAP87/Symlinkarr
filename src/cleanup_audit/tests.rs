
use super::*;
use crate::config::Config;
use crate::db::Database;
use crate::models::{LinkRecord, LinkStatus, MediaType};
use chrono::Duration as ChronoDuration;

fn test_working_entry(
    media_id: &str,
    season: Option<u32>,
    episode: Option<u32>,
    reasons: &[FindingReason],
) -> WorkingEntry {
    let mut reason_set = BTreeSet::new();
    for reason in reasons {
        reason_set.insert(*reason);
    }

    WorkingEntry {
        symlink_path: PathBuf::from("/lib/test.mkv"),
        source_path: PathBuf::from("/src/test.mkv"),
        media_id: media_id.to_string(),
        media_type: MediaType::Tv,
        content_type: ContentType::Anime,
        parsed_title: String::new(),
        year: None,
        season,
        episode,
        library_title: String::new(),
        alternate_match: None,
        legacy_anime_root: None,
        reasons: reason_set,
    }
}

fn test_cleanup_finding(
    media_id: &str,
    season: u32,
    episode: u32,
    severity: FindingSeverity,
    reasons: Vec<FindingReason>,
    symlink_path: &str,
    source_path: &str,
) -> CleanupFinding {
    CleanupFinding {
        symlink_path: PathBuf::from(symlink_path),
        source_path: PathBuf::from(source_path),
        media_id: media_id.to_string(),
        severity,
        confidence: 0.5,
        reasons,
        parsed: ParsedContext {
            library_title: String::new(),
            parsed_title: String::new(),
            year: None,
            season: Some(season),
            episode: Some(episode),
        },
        alternate_match: None,
        legacy_anime_root: None,
        db_tracked: false,
        ownership: CleanupOwnership::Foreign,
    }
}

#[test]
fn test_token_match_rejects_group_substring_collision() {
    assert!(!tokenized_title_match("show", "showgroup fansub"));
    assert!(!tokenized_title_match("show group", "showgroup fansub"));
}

#[test]
fn test_tokenized_title_match_exact_and_contiguous_tokens() {
    assert!(tokenized_title_match("jujutsu kaisen", "jujutsu kaisen 03"));
    assert!(tokenized_title_match("jujutsu kaisen 03", "jujutsu kaisen"));
    assert!(!tokenized_title_match("one piece", "piece one"));
}

#[test]
fn test_owner_title_matches_standard_rejects_one_word_collision() {
    assert!(!owner_title_matches(
        ContentType::Tv,
        &[normalize("you")],
        &normalize("i love you man")
    ));
    assert!(!owner_title_matches(
        ContentType::Tv,
        &[normalize("chuck")],
        &normalize("chucky")
    ));
    assert!(owner_title_matches(
        ContentType::Tv,
        &[normalize("you")],
        &normalize("you")
    ));
}

#[test]
fn test_owner_title_matches_standard_allows_leading_article_drop() {
    assert!(owner_title_matches(
        ContentType::Movie,
        &[normalize("the matrix")],
        &normalize("matrix")
    ));
}

#[test]
fn test_owner_title_matches_standard_allows_trailing_year_drop() {
    assert!(owner_title_matches(
        ContentType::Movie,
        &[normalize("leon the professional 1994")],
        &normalize("leon the professional")
    ));
    assert!(owner_title_matches(
        ContentType::Movie,
        &[normalize("sam morril youve changed 2024")],
        &normalize("sam morril youve changed")
    ));
}

#[test]
fn test_owner_title_matches_tv_allows_embedded_episode_marker_after_title() {
    assert!(owner_title_matches(
        ContentType::Tv,
        &[normalize("dexter 2006")],
        &normalize("dexter 3x01 nuestro padre")
    ));
    assert!(owner_title_matches(
        ContentType::Tv,
        &[normalize("stranger things 2016")],
        &normalize("stranger things s05x07 il ponte")
    ));
}

#[test]
fn test_owner_title_matches_standard_does_not_merge_near_titles_after_year_drop() {
    assert!(!owner_title_matches(
        ContentType::Movie,
        &[normalize("freakier friday 2025")],
        &normalize("freaky friday 2003")
    ));
    assert!(!owner_title_matches(
        ContentType::Movie,
        &[normalize("hellbound hellraiser ii 1988")],
        &normalize("hellraiser 2022")
    ));
}

#[test]
fn test_find_alternate_library_match_picks_exact_other_title() {
    let library_items = vec![
        LibraryItem {
            id: MediaId::Tvdb(1),
            path: PathBuf::from("/library/Chuck (2007) {tvdb-1}"),
            title: "Chuck (2007)".to_string(),
            library_name: "Series".to_string(),
            media_type: MediaType::Tv,
            content_type: ContentType::Tv,
        },
        LibraryItem {
            id: MediaId::Tvdb(2),
            path: PathBuf::from("/library/Chucky (2021) {tvdb-2}"),
            title: "Chucky (2021)".to_string(),
            library_name: "Series".to_string(),
            media_type: MediaType::Tv,
            content_type: ContentType::Tv,
        },
    ];
    let metadata_map = HashMap::new();
    let alias_map_by_index = build_aliases_by_index(&library_items, &metadata_map);
    let alias_token_index = build_alias_token_index(&alias_map_by_index);
    let entry = WorkingEntry {
        symlink_path: PathBuf::from("/library/Chuck (2007) {tvdb-1}/Season 01/Chuck - S01E01.mkv"),
        source_path: PathBuf::from("/src/Chucky.S01E01.mkv"),
        media_id: "tvdb-1".to_string(),
        media_type: MediaType::Tv,
        content_type: ContentType::Tv,
        parsed_title: "Chucky".to_string(),
        year: Some(2021),
        season: Some(1),
        episode: Some(1),
        library_title: "Chuck (2007)".to_string(),
        alternate_match: None,
        legacy_anime_root: None,
        reasons: BTreeSet::new(),
    };

    let alt = find_alternate_library_match(
        0,
        &entry,
        &normalize("Chucky"),
        &library_items,
        &alias_map_by_index,
        &alias_token_index,
        &metadata_map,
    );

    assert_eq!(
        alt,
        Some(AlternateMatchContext {
            media_id: "tvdb-2".to_string(),
            title: "Chucky (2021)".to_string(),
            score: 1.0,
        })
    );
}

#[test]
fn test_classify_severity_treats_alternate_library_match_as_critical_with_mismatch() {
    let mut reasons = BTreeSet::new();
    reasons.insert(FindingReason::ParserTitleMismatch);
    reasons.insert(FindingReason::AlternateLibraryMatch);

    assert_eq!(classify_severity(&reasons), FindingSeverity::Critical);
    assert_eq!(classify_confidence(&reasons), 0.98);
}

#[test]
fn test_candidate_metadata_compatible_rejects_tv_year_mismatch() {
    let item = LibraryItem {
        id: MediaId::Tvdb(1),
        path: PathBuf::from("/library/Dark Matter (2015) {tvdb-1}"),
        title: "Dark Matter (2015)".to_string(),
        library_name: "Series".to_string(),
        media_type: MediaType::Tv,
        content_type: ContentType::Tv,
    };
    let entry = WorkingEntry {
        symlink_path: PathBuf::from(
            "/library/Dark Matter (2015) {tvdb-1}/Season 01/Dark Matter - S01E01.mkv",
        ),
        source_path: PathBuf::from("/src/Dark.Matter.2024.S01E01.mkv"),
        media_id: "tvdb-1".to_string(),
        media_type: MediaType::Tv,
        content_type: ContentType::Tv,
        parsed_title: "Dark Matter".to_string(),
        year: Some(2024),
        season: Some(1),
        episode: Some(1),
        library_title: "Dark Matter (2015)".to_string(),
        alternate_match: None,
        legacy_anime_root: None,
        reasons: BTreeSet::new(),
    };
    let metadata = ContentMetadata {
        title: "Dark Matter".to_string(),
        aliases: Vec::new(),
        year: Some(2015),
        seasons: vec![crate::models::SeasonInfo {
            season_number: 1,
            episodes: vec![crate::models::EpisodeInfo {
                episode_number: 1,
                title: "Episode 1".to_string(),
            }],
        }],
    };

    assert!(!candidate_metadata_compatible(
        &item,
        &entry,
        Some(&metadata)
    ));
}

#[test]
fn test_candidate_metadata_compatible_rejects_regular_tv_unknown_season() {
    let item = LibraryItem {
        id: MediaId::Tvdb(1),
        path: PathBuf::from("/library/Dark Matter (2024) {tvdb-1}"),
        title: "Dark Matter (2024)".to_string(),
        library_name: "Series".to_string(),
        media_type: MediaType::Tv,
        content_type: ContentType::Tv,
    };
    let entry = WorkingEntry {
        symlink_path: PathBuf::from(
            "/library/Dark Matter (2024) {tvdb-1}/Season 02/Dark Matter - S02E01.mkv",
        ),
        source_path: PathBuf::from("/src/Dark.Matter.2024.S02E01.mkv"),
        media_id: "tvdb-1".to_string(),
        media_type: MediaType::Tv,
        content_type: ContentType::Tv,
        parsed_title: "Dark Matter".to_string(),
        year: Some(2024),
        season: Some(2),
        episode: Some(1),
        library_title: "Dark Matter (2024)".to_string(),
        alternate_match: None,
        legacy_anime_root: None,
        reasons: BTreeSet::new(),
    };
    let metadata = ContentMetadata {
        title: "Dark Matter".to_string(),
        aliases: Vec::new(),
        year: Some(2024),
        seasons: vec![crate::models::SeasonInfo {
            season_number: 1,
            episodes: vec![crate::models::EpisodeInfo {
                episode_number: 1,
                title: "Episode 1".to_string(),
            }],
        }],
    };

    assert!(!candidate_metadata_compatible(
        &item,
        &entry,
        Some(&metadata)
    ));
}

#[test]
fn test_find_alternate_library_match_prefers_same_title_matching_year() {
    let library_items = vec![
        LibraryItem {
            id: MediaId::Tvdb(1),
            path: PathBuf::from("/library/Dark Matter (2015) {tvdb-1}"),
            title: "Dark Matter (2015)".to_string(),
            library_name: "Series".to_string(),
            media_type: MediaType::Tv,
            content_type: ContentType::Tv,
        },
        LibraryItem {
            id: MediaId::Tvdb(2),
            path: PathBuf::from("/library/Dark Matter (2024) {tvdb-2}"),
            title: "Dark Matter (2024)".to_string(),
            library_name: "Series".to_string(),
            media_type: MediaType::Tv,
            content_type: ContentType::Tv,
        },
    ];
    let metadata_map = HashMap::from([
        (
            "tvdb-1".to_string(),
            Some(ContentMetadata {
                title: "Dark Matter".to_string(),
                aliases: Vec::new(),
                year: Some(2015),
                seasons: vec![crate::models::SeasonInfo {
                    season_number: 1,
                    episodes: vec![crate::models::EpisodeInfo {
                        episode_number: 1,
                        title: "Episode 1".to_string(),
                    }],
                }],
            }),
        ),
        (
            "tvdb-2".to_string(),
            Some(ContentMetadata {
                title: "Dark Matter".to_string(),
                aliases: Vec::new(),
                year: Some(2024),
                seasons: vec![crate::models::SeasonInfo {
                    season_number: 1,
                    episodes: vec![crate::models::EpisodeInfo {
                        episode_number: 1,
                        title: "Episode 1".to_string(),
                    }],
                }],
            }),
        ),
    ]);
    let alias_map_by_index = build_aliases_by_index(&library_items, &metadata_map);
    let alias_token_index = build_alias_token_index(&alias_map_by_index);
    let entry = WorkingEntry {
        symlink_path: PathBuf::from(
            "/library/Dark Matter (2015) {tvdb-1}/Season 01/Dark Matter - S01E01.mkv",
        ),
        source_path: PathBuf::from("/src/Dark.Matter.2024.S01E01.mkv"),
        media_id: "tvdb-1".to_string(),
        media_type: MediaType::Tv,
        content_type: ContentType::Tv,
        parsed_title: "Dark Matter".to_string(),
        year: Some(2024),
        season: Some(1),
        episode: Some(1),
        library_title: "Dark Matter (2015)".to_string(),
        alternate_match: None,
        legacy_anime_root: None,
        reasons: BTreeSet::new(),
    };

    let alt = find_alternate_library_match(
        0,
        &entry,
        &normalize("Dark Matter"),
        &library_items,
        &alias_map_by_index,
        &alias_token_index,
        &metadata_map,
    );

    assert_eq!(
        alt,
        Some(AlternateMatchContext {
            media_id: "tvdb-2".to_string(),
            title: "Dark Matter (2024)".to_string(),
            score: 1.0,
        })
    );
}

#[test]
fn test_candidate_metadata_compatible_rejects_movie_year_mismatch() {
    let item = LibraryItem {
        id: MediaId::Tmdb(1),
        path: PathBuf::from("/library/The Crow (1994) {tmdb-1}"),
        title: "The Crow (1994)".to_string(),
        library_name: "Movies".to_string(),
        media_type: MediaType::Movie,
        content_type: ContentType::Movie,
    };
    let entry = WorkingEntry {
        symlink_path: PathBuf::from("/library/The Crow (1994) {tmdb-1}/The Crow (1994).mkv"),
        source_path: PathBuf::from("/src/The.Crow.2024.1080p.mkv"),
        media_id: "tmdb-1".to_string(),
        media_type: MediaType::Movie,
        content_type: ContentType::Movie,
        parsed_title: "The Crow".to_string(),
        year: Some(2024),
        season: None,
        episode: None,
        library_title: "The Crow (1994)".to_string(),
        alternate_match: None,
        legacy_anime_root: None,
        reasons: BTreeSet::new(),
    };
    let metadata = ContentMetadata {
        title: "The Crow".to_string(),
        aliases: Vec::new(),
        year: Some(1994),
        seasons: Vec::new(),
    };

    assert!(!candidate_metadata_compatible(
        &item,
        &entry,
        Some(&metadata)
    ));
}

#[test]
fn test_cleanup_scope_parse_accepts_general_scopes() {
    assert_eq!(CleanupScope::parse("tv").unwrap(), CleanupScope::Tv);
    assert_eq!(CleanupScope::parse("movies").unwrap(), CleanupScope::Movie);
    assert_eq!(CleanupScope::parse("all").unwrap(), CleanupScope::All);
}

#[test]
fn test_cleanup_scope_parse_all_variants() {
    assert_eq!(CleanupScope::parse("anime").unwrap(), CleanupScope::Anime);
    assert_eq!(CleanupScope::parse("series").unwrap(), CleanupScope::Tv);
    assert_eq!(CleanupScope::parse("shows").unwrap(), CleanupScope::Tv);
    assert_eq!(CleanupScope::parse("movie").unwrap(), CleanupScope::Movie);
    assert_eq!(CleanupScope::parse("films").unwrap(), CleanupScope::Movie);
    assert_eq!(CleanupScope::parse("film").unwrap(), CleanupScope::Movie);
    assert!(CleanupScope::parse("tv").unwrap() == CleanupScope::Tv);
}

#[test]
fn test_cleanup_scope_parse_rejects_invalid() {
    assert!(CleanupScope::parse("invalid").is_err());
    assert!(CleanupScope::parse("").is_err());
    assert_eq!(CleanupScope::parse("ANIME").unwrap(), CleanupScope::Anime); // case-insensitive
}

#[test]
fn test_finding_severity_display() {
    assert_eq!(FindingSeverity::Critical.to_string(), "critical");
    assert_eq!(FindingSeverity::High.to_string(), "high");
    assert_eq!(FindingSeverity::Warning.to_string(), "warning");
}

#[test]
fn test_finding_reason_display() {
    assert_eq!(FindingReason::BrokenSource.to_string(), "broken_source");
    assert_eq!(
        FindingReason::LegacyAnimeRootDuplicate.to_string(),
        "legacy_anime_root_duplicate"
    );
    assert_eq!(
        FindingReason::ParserTitleMismatch.to_string(),
        "parser_title_mismatch"
    );
    assert_eq!(
        FindingReason::DuplicateEpisodeSlot.to_string(),
        "duplicate_episode_slot"
    );
    assert_eq!(
        FindingReason::NonRdSourcePath.to_string(),
        "non_rd_source_path"
    );
}

#[test]
fn test_find_owner_library_item_walks_up_to_show_root() {
    let library_items = vec![LibraryItem {
        id: MediaId::Tvdb(42),
        path: PathBuf::from("/library/Show (2025) {tvdb-42}"),
        title: "Show".to_string(),
        library_name: "Series".to_string(),
        media_type: MediaType::Tv,
        content_type: ContentType::Tv,
    }];
    let by_path = build_library_indices_by_path(&library_items);
    let symlink_path = PathBuf::from("/library/Show (2025) {tvdb-42}/Season 01/Show - S01E01.mkv");

    let owner = find_owner_library_item(&symlink_path, &library_items, &by_path).unwrap();
    assert_eq!(owner.id, MediaId::Tvdb(42));
}

#[test]
fn test_classify_severity_critical_combo() {
    let mut reasons = BTreeSet::new();
    reasons.insert(FindingReason::ArrUntracked);
    reasons.insert(FindingReason::ParserTitleMismatch);
    assert_eq!(classify_severity(&reasons), FindingSeverity::Critical);
}

#[test]
fn test_classify_severity_warning() {
    let mut reasons = BTreeSet::new();
    reasons.insert(FindingReason::SeasonCountAnomaly);
    assert_eq!(classify_severity(&reasons), FindingSeverity::Warning);
}

#[test]
fn test_classify_severity_keeps_legacy_anime_root_duplicate_as_warning() {
    let mut reasons = BTreeSet::new();
    reasons.insert(FindingReason::LegacyAnimeRootDuplicate);
    assert_eq!(classify_severity(&reasons), FindingSeverity::Warning);
    assert_eq!(classify_confidence(&reasons), 0.55);
}

#[test]
fn test_classify_severity_treats_movie_episode_source_as_critical() {
    let mut reasons = BTreeSet::new();
    reasons.insert(FindingReason::MovieEpisodeSource);
    assert_eq!(classify_severity(&reasons), FindingSeverity::Critical);
}

#[test]
fn test_classify_confidence_uses_strongest_reason() {
    let mut reasons = BTreeSet::new();
    reasons.insert(FindingReason::SeasonCountAnomaly);
    reasons.insert(FindingReason::BrokenSource);
    assert_eq!(classify_confidence(&reasons), 1.0);
}

#[test]
fn test_season_count_anomaly_ignores_missing_or_equal_counts() {
    assert!(!is_season_count_anomaly(18, 20));
    assert!(!is_season_count_anomaly(20, 20));
}

#[test]
fn test_season_count_anomaly_small_season_requires_at_least_two_excess() {
    assert!(!is_season_count_anomaly(11, 10));
    assert!(is_season_count_anomaly(12, 10));
}

#[test]
fn test_season_count_anomaly_medium_season_requires_ratio_and_excess() {
    assert!(!is_season_count_anomaly(23, 20));
    assert!(is_season_count_anomaly(24, 20));
}

#[test]
fn test_season_count_anomaly_large_season_scales_with_expected_count() {
    assert!(!is_season_count_anomaly(59, 50));
    assert!(is_season_count_anomaly(60, 50));
}

#[test]
fn test_episode_out_of_range_allows_unknown_specials() {
    let meta = ContentMetadata {
        title: "Test Show".to_string(),
        aliases: vec![],
        year: None,
        seasons: vec![crate::models::SeasonInfo {
            season_number: 1,
            episodes: vec![crate::models::EpisodeInfo {
                episode_number: 1,
                title: "Ep1".to_string(),
            }],
        }],
    };

    assert!(!episode_out_of_range(&meta, 0, 1));
}

#[test]
fn test_episode_out_of_range_keeps_regular_unknown_season_strict() {
    let meta = ContentMetadata {
        title: "Test Show".to_string(),
        aliases: vec![],
        year: None,
        seasons: vec![crate::models::SeasonInfo {
            season_number: 1,
            episodes: vec![crate::models::EpisodeInfo {
                episode_number: 1,
                title: "Ep1".to_string(),
            }],
        }],
    };

    assert!(episode_out_of_range(&meta, 9, 1));
}

#[test]
fn test_suppress_redundant_season_count_warning_when_season_has_high_signal() {
    let mut entries = vec![
        test_working_entry(
            "tvdb-1",
            Some(1),
            Some(1),
            &[FindingReason::SeasonCountAnomaly],
        ),
        test_working_entry(
            "tvdb-1",
            Some(1),
            Some(2),
            &[FindingReason::ParserTitleMismatch],
        ),
        test_working_entry(
            "tvdb-1",
            Some(2),
            Some(1),
            &[FindingReason::SeasonCountAnomaly],
        ),
    ];

    let suppressed = suppress_redundant_season_count_warnings(&mut entries);
    assert_eq!(suppressed, 1);
    assert!(entries[0].reasons.is_empty());
    assert!(entries[2]
        .reasons
        .contains(&FindingReason::SeasonCountAnomaly));
}

#[test]
fn test_collect_safe_warning_duplicate_prunes_requires_tracked_anchor() {
    let findings = vec![
        test_cleanup_finding(
            "tvdb-1",
            1,
            3,
            FindingSeverity::Warning,
            vec![FindingReason::DuplicateEpisodeSlot],
            "/lib/Show - S01E03 a.mkv",
            "/src/show-s01e03.mkv",
        ),
        test_cleanup_finding(
            "tvdb-1",
            1,
            3,
            FindingSeverity::Warning,
            vec![FindingReason::DuplicateEpisodeSlot],
            "/lib/Show - S01E03 b.mkv",
            "/src/show-s01e03.mkv",
        ),
    ];

    let plan = collect_safe_duplicate_prune_plan(&findings);
    assert!(plan.prune_paths.is_empty());
    assert_eq!(
        plan.blocked_reason_counts
            .get(&PruneBlockedReasonCode::DuplicateSlotNeedsTrackedAnchor),
        Some(&2)
    );
}

#[test]
fn test_collect_safe_warning_duplicate_prunes_skips_different_sources() {
    let findings = vec![
        test_cleanup_finding(
            "tvdb-1",
            1,
            3,
            FindingSeverity::Warning,
            vec![FindingReason::DuplicateEpisodeSlot],
            "/lib/Show - S01E03 a.mkv",
            "/src/show-s01e03-source-a.mkv",
        ),
        test_cleanup_finding(
            "tvdb-1",
            1,
            3,
            FindingSeverity::Warning,
            vec![FindingReason::DuplicateEpisodeSlot],
            "/lib/Show - S01E03 b.mkv",
            "/src/show-s01e03-source-b.mkv",
        ),
    ];

    let plan = collect_safe_duplicate_prune_plan(&findings);
    assert!(plan.prune_paths.is_empty());
    assert_eq!(
        plan.blocked_reason_counts
            .get(&PruneBlockedReasonCode::DuplicateSlotSourceMismatch),
        Some(&2)
    );
}

#[test]
fn test_collect_safe_warning_duplicate_prunes_skips_tainted_slot() {
    let findings = vec![
        test_cleanup_finding(
            "tvdb-1",
            1,
            3,
            FindingSeverity::Warning,
            vec![FindingReason::DuplicateEpisodeSlot],
            "/lib/Show - S01E03 a.mkv",
            "/src/show-s01e03.mkv",
        ),
        test_cleanup_finding(
            "tvdb-1",
            1,
            3,
            FindingSeverity::Warning,
            vec![FindingReason::DuplicateEpisodeSlot],
            "/lib/Show - S01E03 b.mkv",
            "/src/show-s01e03.mkv",
        ),
        test_cleanup_finding(
            "tvdb-1",
            1,
            3,
            FindingSeverity::High,
            vec![
                FindingReason::DuplicateEpisodeSlot,
                FindingReason::ParserTitleMismatch,
            ],
            "/lib/Show - S01E03 suspicious.mkv",
            "/src/show-s01e03-alt.mkv",
        ),
    ];

    let plan = collect_safe_duplicate_prune_plan(&findings);
    assert!(plan.prune_paths.is_empty());
    assert_eq!(
        plan.blocked_reason_counts
            .get(&PruneBlockedReasonCode::DuplicateSlotTainted),
        Some(&2)
    );
}

#[test]
fn test_collect_safe_warning_duplicate_prunes_prunes_untracked_in_mixed_slot() {
    let mut tracked = test_cleanup_finding(
        "tvdb-1",
        1,
        3,
        FindingSeverity::High,
        vec![
            FindingReason::DuplicateEpisodeSlot,
            FindingReason::SeasonCountAnomaly,
        ],
        "/lib/Show - S01E03 canonical.mkv",
        "/src/show-s01e03.mkv",
    );
    tracked.db_tracked = true;

    let legacy = test_cleanup_finding(
        "tvdb-1",
        1,
        3,
        FindingSeverity::High,
        vec![
            FindingReason::DuplicateEpisodeSlot,
            FindingReason::SeasonCountAnomaly,
        ],
        "/lib/Show - S01E03 legacy.mkv",
        "/src/show-s01e03.mkv",
    );

    let plan = collect_safe_duplicate_prune_plan(&[tracked, legacy]);
    assert_eq!(
        plan.prune_paths,
        vec![PathBuf::from("/lib/Show - S01E03 legacy.mkv")]
    );
    assert!(plan
        .managed_paths
        .contains(&PathBuf::from("/lib/Show - S01E03 canonical.mkv")));
    assert!(plan
        .managed_paths
        .contains(&PathBuf::from("/lib/Show - S01E03 legacy.mkv")));
}

#[test]
fn test_collect_safe_warning_duplicate_prunes_skips_all_tracked_duplicates() {
    let mut first = test_cleanup_finding(
        "tvdb-1",
        1,
        3,
        FindingSeverity::Warning,
        vec![FindingReason::DuplicateEpisodeSlot],
        "/lib/Show - S01E03 canonical-a.mkv",
        "/src/show-s01e03.mkv",
    );
    first.db_tracked = true;

    let mut second = test_cleanup_finding(
        "tvdb-1",
        1,
        3,
        FindingSeverity::Warning,
        vec![FindingReason::DuplicateEpisodeSlot],
        "/lib/Show - S01E03 canonical-b.mkv",
        "/src/show-s01e03.mkv",
    );
    second.db_tracked = true;

    let prunes = collect_safe_duplicate_prune_plan(&[first, second]).prune_paths;
    assert!(prunes.is_empty());
}

#[test]
fn test_build_prune_plan_excludes_legacy_anime_root_warning_by_default() {
    let report = report_with_findings(
        Utc::now(),
        vec![CleanupFinding {
            symlink_path: PathBuf::from("/lib/Show/Season 01/Show - S01E01.mkv"),
            source_path: PathBuf::from("/src/Show.S01E01.mkv"),
            media_id: String::new(),
            severity: FindingSeverity::Warning,
            confidence: 0.55,
            reasons: vec![FindingReason::LegacyAnimeRootDuplicate],
            parsed: ParsedContext {
                library_title: "Show".to_string(),
                parsed_title: "Show".to_string(),
                year: None,
                season: Some(1),
                episode: Some(1),
            },
            alternate_match: None,
            legacy_anime_root: Some(LegacyAnimeRootDetails {
                normalized_title: "Show".to_string(),
                untagged_root: PathBuf::from("/lib/Show"),
                tagged_roots: vec![PathBuf::from("/lib/Show (2024) {tvdb-123}")],
            }),
            db_tracked: false,
            ownership: CleanupOwnership::Foreign,
        }],
    );

    let plan = build_prune_plan(&report, true, false);
    assert_eq!(plan.candidate_paths.len(), 0);
    assert_eq!(plan.legacy_anime_root_candidates, 0);
    assert_eq!(plan.blocked_candidates, 1);
    assert_eq!(
        plan.blocked_reason_summary[0].code,
        PruneBlockedReasonCode::LegacyAnimeRootsExcludedByDefault
    );
}

#[test]
fn test_build_prune_plan_can_include_legacy_anime_root_warning_candidates() {
    let report = report_with_findings(
        Utc::now(),
        vec![CleanupFinding {
            symlink_path: PathBuf::from("/lib/Show/Season 01/Show - S01E01.mkv"),
            source_path: PathBuf::from("/src/Show.S01E01.mkv"),
            media_id: String::new(),
            severity: FindingSeverity::Warning,
            confidence: 0.55,
            reasons: vec![FindingReason::LegacyAnimeRootDuplicate],
            parsed: ParsedContext {
                library_title: "Show".to_string(),
                parsed_title: "Show".to_string(),
                year: None,
                season: Some(1),
                episode: Some(1),
            },
            alternate_match: None,
            legacy_anime_root: Some(LegacyAnimeRootDetails {
                normalized_title: "Show".to_string(),
                untagged_root: PathBuf::from("/lib/Show"),
                tagged_roots: vec![PathBuf::from("/lib/Show (2024) {tvdb-123}")],
            }),
            db_tracked: false,
            ownership: CleanupOwnership::Foreign,
        }],
    );

    let plan = build_prune_plan(&report, true, true);
    assert_eq!(
        plan.candidate_paths,
        vec![PathBuf::from("/lib/Show/Season 01/Show - S01E01.mkv")]
    );
    assert_eq!(plan.legacy_anime_root_candidates, 1);
    assert_eq!(
        plan.legacy_anime_root_groups,
        vec![LegacyAnimeRootGroupCount {
            normalized_title: "Show".to_string(),
            total: 1,
            tagged_roots: vec![PathBuf::from("/lib/Show (2024) {tvdb-123}")],
        }]
    );
    assert_eq!(plan.foreign_candidates, 1);
    assert_eq!(plan.managed_candidates, 0);
    assert_eq!(
        plan.reason_counts,
        vec![PruneReasonCount {
            reason: FindingReason::LegacyAnimeRootDuplicate,
            total: 1,
            managed: 0,
            foreign: 1,
        }]
    );
    assert_eq!(
        plan.action_for_path(Path::new("/lib/Show/Season 01/Show - S01E01.mkv")),
        PrunePathAction::Quarantine
    );
}

#[test]
fn test_build_prune_plan_reports_foreign_quarantine_disabled() {
    let report = report_with_findings(
        Utc::now(),
        vec![CleanupFinding {
            symlink_path: PathBuf::from("/lib/Show/Season 01/Show - S01E02.mkv"),
            source_path: PathBuf::from("/src/Show.S01E02.mkv"),
            media_id: "tvdb-1".to_string(),
            severity: FindingSeverity::High,
            confidence: 1.0,
            reasons: vec![FindingReason::BrokenSource],
            parsed: ParsedContext {
                library_title: "Show".to_string(),
                parsed_title: "Show".to_string(),
                year: None,
                season: Some(1),
                episode: Some(2),
            },
            alternate_match: None,
            legacy_anime_root: None,
            db_tracked: false,
            ownership: CleanupOwnership::Foreign,
        }],
    );

    let plan = build_prune_plan(&report, false, false);
    assert!(plan.candidate_paths.is_empty());
    assert_eq!(plan.blocked_candidates, 1);
    assert_eq!(
        plan.blocked_reason_summary[0].code,
        PruneBlockedReasonCode::ForeignQuarantineDisabled
    );
}

#[test]
fn test_build_prune_plan_marks_untracked_duplicate_without_anchor_as_blocked() {
    let first = test_cleanup_finding(
        "tvdb-1",
        1,
        3,
        FindingSeverity::Warning,
        vec![FindingReason::DuplicateEpisodeSlot],
        "/lib/Show - S01E03 legacy-a.mkv",
        "/src/show-s01e03.mkv",
    );

    let second = test_cleanup_finding(
        "tvdb-1",
        1,
        3,
        FindingSeverity::Warning,
        vec![FindingReason::DuplicateEpisodeSlot],
        "/lib/Show - S01E03 legacy-b.mkv",
        "/src/show-s01e03.mkv",
    );

    let report = report_with_findings(Utc::now(), vec![first.clone(), second.clone()]);
    let plan = build_prune_plan(&report, true, false);

    assert!(plan.candidate_paths.is_empty());
    assert_eq!(plan.blocked_candidates, 2);
    assert_eq!(
        plan.action_for_path(&first.symlink_path),
        PrunePathAction::Blocked(PruneBlockedReasonCode::DuplicateSlotNeedsTrackedAnchor)
    );
    assert!(plan.blocked_reason_summary.iter().any(|entry| {
        entry.code == PruneBlockedReasonCode::DuplicateSlotNeedsTrackedAnchor
            && entry.candidates == 2
    }));
}

fn test_config(library_root: &Path, source_root: &Path) -> Config {
    let quarantine_root = library_root
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("quarantine");
    let yaml = format!(
        r#"
libraries:
  - name: Anime
    path: "{}"
    media_type: tv
    content_type: anime
sources:
  - name: RD
    path: "{}"
    media_type: auto
backup:
  enabled: false
cleanup:
  prune:
    quarantine_path: "{}"
"#,
        library_root.display(),
        source_root.display(),
        quarantine_root.display()
    );
    serde_yml::from_str(&yaml).unwrap()
}

fn test_config_multi_scope(
    anime_a_root: &Path,
    anime_b_root: &Path,
    movie_root: &Path,
    source_root: &Path,
) -> Config {
    let quarantine_root = anime_a_root
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("quarantine");
    let yaml = format!(
        r#"
libraries:
  - name: Anime A
    path: "{}"
    media_type: tv
    content_type: anime
  - name: Anime B
    path: "{}"
    media_type: tv
    content_type: anime
  - name: Movies
    path: "{}"
    media_type: movie
    content_type: movie
sources:
  - name: RD
    path: "{}"
    media_type: auto
backup:
  enabled: false
cleanup:
  prune:
    quarantine_path: "{}"
"#,
        anime_a_root.display(),
        anime_b_root.display(),
        movie_root.display(),
        source_root.display(),
        quarantine_root.display()
    );
    serde_yml::from_str(&yaml).unwrap()
}

fn high_finding(path: &Path, source: &Path) -> CleanupFinding {
    CleanupFinding {
        symlink_path: path.to_path_buf(),
        source_path: source.to_path_buf(),
        media_id: "tvdb-1".to_string(),
        severity: FindingSeverity::High,
        confidence: 1.0,
        reasons: vec![FindingReason::BrokenSource],
        parsed: ParsedContext {
            library_title: "Show".to_string(),
            parsed_title: "Show".to_string(),
            year: None,
            season: Some(1),
            episode: Some(1),
        },
        alternate_match: None,
        legacy_anime_root: None,
        db_tracked: false,
        ownership: CleanupOwnership::Foreign,
    }
}

fn report_with_findings(created_at: DateTime<Utc>, findings: Vec<CleanupFinding>) -> CleanupReport {
    let summary = CleanupSummary {
        total_findings: findings.len(),
        high: findings
            .iter()
            .filter(|f| matches!(f.severity, FindingSeverity::High))
            .count(),
        ..CleanupSummary::default()
    };
    CleanupReport {
        version: 1,
        created_at,
        scope: CleanupScope::Anime,
        findings,
        summary,
        applied_at: None,
    }
}

#[tokio::test]
async fn test_libraries_for_scope_filtered_respects_selected_library_names() {
    let dir = tempfile::TempDir::new().unwrap();
    let anime_a_root = dir.path().join("anime-a");
    let anime_b_root = dir.path().join("anime-b");
    let movie_root = dir.path().join("movies");
    let source_root = dir.path().join("rd");
    std::fs::create_dir_all(&anime_a_root).unwrap();
    std::fs::create_dir_all(&anime_b_root).unwrap();
    std::fs::create_dir_all(&movie_root).unwrap();
    std::fs::create_dir_all(&source_root).unwrap();

    let cfg = test_config_multi_scope(&anime_a_root, &anime_b_root, &movie_root, &source_root);
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let auditor = CleanupAuditor::new_with_progress(&cfg, &db, false);

    let selected = vec!["Anime B".to_string()];
    let libraries = auditor
        .libraries_for_scope_filtered(CleanupScope::Anime, Some(&selected))
        .unwrap();

    assert_eq!(libraries.len(), 1);
    assert_eq!(libraries[0].name, "Anime B");
}

#[test]
fn test_build_legacy_anime_root_lookup_indexes_untagged_roots_only() {
    let dir = tempfile::TempDir::new().unwrap();
    let anime_root = dir.path().join("anime");
    std::fs::create_dir_all(anime_root.join("Show")).unwrap();
    std::fs::create_dir_all(anime_root.join("Show (2024) {tvdb-123}")).unwrap();
    std::fs::create_dir_all(anime_root.join("Other (2024) {tvdb-456}")).unwrap();

    let library = LibraryConfig {
        name: "Anime".to_string(),
        path: anime_root.clone(),
        media_type: MediaType::Tv,
        content_type: Some(ContentType::Anime),
        depth: 1,
    };

    let lookup = build_legacy_anime_root_lookup(&[&library]);
    assert_eq!(lookup.len(), 1);
    assert_eq!(
        lookup
            .get(&anime_root.join("Show"))
            .map(|context| context.normalized_title.as_str()),
        Some("Show")
    );
    assert_eq!(
        lookup
            .get(&anime_root.join("Show"))
            .map(|context| context.tagged_roots.clone()),
        Some(vec![anime_root.join("Show (2024) {tvdb-123}")])
    );
    assert!(!lookup.contains_key(&anime_root.join("Show (2024) {tvdb-123}")));
}

#[test]
fn test_legacy_anime_root_context_for_path_uses_first_library_component() {
    let library_root = PathBuf::from("/library/anime");
    let mut lookup = HashMap::new();
    lookup.insert(
        library_root.join("Show"),
        LegacyAnimeRootContext {
            normalized_title: "Show".to_string(),
            untagged_root: library_root.join("Show"),
            tagged_roots: vec![library_root.join("Show (2024) {tvdb-123}")],
        },
    );

    let context = legacy_anime_root_context_for_path(
        &library_root.join("Show/Season 01/Show - S01E01.mkv"),
        &library_root,
        &lookup,
    )
    .unwrap();

    assert_eq!(context.normalized_title, "Show");
    assert!(legacy_anime_root_context_for_path(
        &library_root.join("Other/Season 01/Other - S01E01.mkv"),
        &library_root,
        &lookup
    )
    .is_none());
}

#[tokio::test]
async fn test_libraries_for_scope_filtered_rejects_selection_outside_scope() {
    let dir = tempfile::TempDir::new().unwrap();
    let anime_a_root = dir.path().join("anime-a");
    let anime_b_root = dir.path().join("anime-b");
    let movie_root = dir.path().join("movies");
    let source_root = dir.path().join("rd");
    std::fs::create_dir_all(&anime_a_root).unwrap();
    std::fs::create_dir_all(&anime_b_root).unwrap();
    std::fs::create_dir_all(&movie_root).unwrap();
    std::fs::create_dir_all(&source_root).unwrap();

    let cfg = test_config_multi_scope(&anime_a_root, &anime_b_root, &movie_root, &source_root);
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let auditor = CleanupAuditor::new_with_progress(&cfg, &db, false);

    let selected = vec!["Movies".to_string()];
    let err = auditor
        .libraries_for_scope_filtered(CleanupScope::Anime, Some(&selected))
        .unwrap_err();

    assert!(err.to_string().contains("No libraries matched scope"));
}

#[cfg(unix)]
#[tokio::test]
async fn test_build_report_flags_legacy_anime_root_duplicates() {
    let dir = tempfile::TempDir::new().unwrap();
    let library_root = dir.path().join("anime");
    let source_root = dir.path().join("rd");
    let tagged_root = library_root.join("Show (2024) {tvdb-123}");
    let legacy_root = library_root.join("Show");
    let season_dir = legacy_root.join("Season 01");
    std::fs::create_dir_all(&tagged_root).unwrap();
    std::fs::create_dir_all(&season_dir).unwrap();
    std::fs::create_dir_all(&source_root).unwrap();

    let source_file = source_root.join("Show.S01E01.mkv");
    std::fs::write(&source_file, "video").unwrap();
    let legacy_symlink = season_dir.join("Show - S01E01.mkv");
    std::os::unix::fs::symlink(&source_file, &legacy_symlink).unwrap();

    let cfg = test_config(&library_root, &source_root);
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let auditor = CleanupAuditor::new_with_progress(&cfg, &db, false);

    let report = auditor.build_report(CleanupScope::Anime).await.unwrap();
    assert_eq!(report.summary.total_findings, 1);
    assert_eq!(report.findings[0].symlink_path, legacy_symlink);
    assert_eq!(report.findings[0].severity, FindingSeverity::Warning);
    assert_eq!(
        report.findings[0].reasons,
        vec![FindingReason::LegacyAnimeRootDuplicate]
    );
    assert_eq!(report.findings[0].parsed.library_title, "Show");
    assert!(!report.findings[0].db_tracked);
}

#[cfg(unix)]
#[tokio::test]
async fn test_prune_apply_marks_db_removed_and_deletes_symlink() {
    let dir = tempfile::TempDir::new().unwrap();
    let library_root = dir.path().join("library");
    let source_root = dir.path().join("rd");
    std::fs::create_dir_all(&library_root).unwrap();
    std::fs::create_dir_all(&source_root).unwrap();

    let source_file = source_root.join("source.mkv");
    std::fs::write(&source_file, "video").unwrap();
    let symlink_path = library_root.join("Show - S01E01.mkv");
    std::os::unix::fs::symlink(&source_file, &symlink_path).unwrap();

    let cfg = test_config(&library_root, &source_root);
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    db.insert_link(&LinkRecord {
        id: None,
        source_path: source_file.clone(),
        target_path: symlink_path.clone(),
        media_id: "tvdb-1".to_string(),
        media_type: MediaType::Tv,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();

    let report = report_with_findings(Utc::now(), vec![high_finding(&symlink_path, &source_file)]);
    let report_path = dir.path().join("report.json");
    std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

    let preview = run_prune(&cfg, &db, &report_path, false, false, None, None)
        .await
        .unwrap();
    let outcome = run_prune(
        &cfg,
        &db,
        &report_path,
        true,
        false,
        None,
        Some(&preview.confirmation_token),
    )
    .await
    .unwrap();

    assert_eq!(outcome.removed, 1);
    assert_eq!(outcome.quarantined, 0);
    assert!(!symlink_path.exists() && !symlink_path.is_symlink());

    let updated = db
        .get_link_by_target_path(&symlink_path)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.status, LinkStatus::Removed);
}

#[cfg(unix)]
#[tokio::test]
async fn test_prune_apply_quarantines_foreign_high_finding() {
    let dir = tempfile::TempDir::new().unwrap();
    let library_root = dir.path().join("library");
    let source_root = dir.path().join("rd");
    let quarantine_root = dir.path().join("quarantine");
    std::fs::create_dir_all(&library_root).unwrap();
    std::fs::create_dir_all(&source_root).unwrap();

    let source_file = source_root.join("source.mkv");
    std::fs::write(&source_file, "video").unwrap();
    let symlink_path = library_root.join("Show - S01E02.mkv");
    std::os::unix::fs::symlink(&source_file, &symlink_path).unwrap();

    let mut cfg = test_config(&library_root, &source_root);
    cfg.cleanup.prune.quarantine_path = quarantine_root.clone();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let report = report_with_findings(Utc::now(), vec![high_finding(&symlink_path, &source_file)]);
    let report_path = dir.path().join("foreign-report.json");
    std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

    let preview = run_prune(&cfg, &db, &report_path, false, false, None, None)
        .await
        .unwrap();
    assert_eq!(preview.candidates, 1);
    assert_eq!(preview.managed_candidates, 0);
    assert_eq!(preview.foreign_candidates, 1);

    let outcome = run_prune(
        &cfg,
        &db,
        &report_path,
        true,
        false,
        None,
        Some(&preview.confirmation_token),
    )
    .await
    .unwrap();

    let quarantined_path = quarantine_root.join("library/Show - S01E02.mkv");
    assert_eq!(outcome.removed, 0);
    assert_eq!(outcome.quarantined, 1);
    assert!(!symlink_path.exists());
    assert!(quarantined_path.is_symlink());
    assert_eq!(std::fs::read_link(&quarantined_path).unwrap(), source_file);
}

#[cfg(unix)]
#[test]
fn test_quarantine_symlink_for_cleanup_leaves_original_when_destination_unwritable() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::TempDir::new().unwrap();
    let library_root = dir.path().join("library");
    let source_root = dir.path().join("rd");
    let quarantine_root = dir.path().join("quarantine");
    std::fs::create_dir_all(&library_root).unwrap();
    std::fs::create_dir_all(&source_root).unwrap();
    std::fs::create_dir_all(&quarantine_root).unwrap();

    let source_file = source_root.join("source.mkv");
    std::fs::write(&source_file, "video").unwrap();
    let symlink_path = library_root.join("Show - S01E99.mkv");
    std::os::unix::fs::symlink(&source_file, &symlink_path).unwrap();

    let mut cfg = test_config(&library_root, &source_root);
    cfg.cleanup.prune.quarantine_path = quarantine_root.clone();

    let mut perms = std::fs::metadata(&quarantine_root).unwrap().permissions();
    perms.set_mode(0o555);
    std::fs::set_permissions(&quarantine_root, perms).unwrap();

    let result = quarantine_symlink_for_cleanup(&cfg, &symlink_path);

    let mut restore = std::fs::metadata(&quarantine_root).unwrap().permissions();
    restore.set_mode(0o755);
    std::fs::set_permissions(&quarantine_root, restore).unwrap();

    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("Permission denied")
            || err.to_string().contains("permission denied")
    );
    assert!(symlink_path.is_symlink());
    assert!(!quarantine_root.join("library/Show - S01E99.mkv").exists());
}

#[tokio::test]
async fn test_prune_preview_skips_foreign_when_quarantine_disabled() {
    let dir = tempfile::TempDir::new().unwrap();
    let library_root = dir.path().join("library");
    let source_root = dir.path().join("rd");
    std::fs::create_dir_all(&library_root).unwrap();
    std::fs::create_dir_all(&source_root).unwrap();

    let mut cfg = test_config(&library_root, &source_root);
    cfg.cleanup.prune.quarantine_foreign = false;
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let source_file = source_root.join("source.mkv");
    let symlink_path = library_root.join("foreign.mkv");
    let report = report_with_findings(Utc::now(), vec![high_finding(&symlink_path, &source_file)]);
    let report_path = dir.path().join("foreign-disabled-report.json");
    std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

    let preview = run_prune(&cfg, &db, &report_path, false, false, None, None)
        .await
        .unwrap();
    assert_eq!(preview.candidates, 0);
    assert_eq!(preview.blocked_candidates, 1);
    assert_eq!(preview.managed_candidates, 0);
    assert_eq!(preview.foreign_candidates, 0);
    assert_eq!(preview.blocked_reason_summary.len(), 1);
    assert_eq!(
        preview.blocked_reason_summary[0].code,
        PruneBlockedReasonCode::ForeignQuarantineDisabled
    );
}

#[tokio::test]
async fn test_prune_apply_rejects_when_all_candidates_are_blocked_by_policy() {
    let dir = tempfile::TempDir::new().unwrap();
    let library_root = dir.path().join("library");
    let source_root = dir.path().join("rd");
    std::fs::create_dir_all(&library_root).unwrap();
    std::fs::create_dir_all(&source_root).unwrap();

    let mut cfg = test_config(&library_root, &source_root);
    cfg.cleanup.prune.quarantine_foreign = false;
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let source_file = source_root.join("source.mkv");
    let symlink_path = library_root.join("foreign.mkv");
    let report = report_with_findings(Utc::now(), vec![high_finding(&symlink_path, &source_file)]);
    let report_path = dir.path().join("foreign-disabled-report.json");
    std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

    let preview = run_prune(&cfg, &db, &report_path, false, false, None, None)
        .await
        .unwrap();
    let err = run_prune(
        &cfg,
        &db,
        &report_path,
        true,
        false,
        None,
        Some(&preview.confirmation_token),
    )
    .await
    .unwrap_err();

    assert!(err.to_string().contains("no actionable candidates remain"));
    assert!(err.to_string().contains("foreign_quarantine_disabled"));
}

#[cfg(unix)]
#[tokio::test]
async fn test_prune_apply_protects_safe_duplicate_candidate_in_tainted_slot() {
    let dir = tempfile::TempDir::new().unwrap();
    let library_root = dir.path().join("library");
    let source_root = dir.path().join("rd");
    std::fs::create_dir_all(&library_root).unwrap();
    std::fs::create_dir_all(&source_root).unwrap();

    let source_file = source_root.join("source.mkv");
    std::fs::write(&source_file, "video").unwrap();
    let canonical_symlink = library_root.join("Show - S01E01 canonical.mkv");
    let suspicious_symlink = library_root.join("Show - S01E01 suspicious.mkv");
    std::os::unix::fs::symlink(&source_file, &canonical_symlink).unwrap();
    std::os::unix::fs::symlink(&source_file, &suspicious_symlink).unwrap();

    let cfg = test_config(&library_root, &source_root);
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    db.insert_link(&LinkRecord {
        id: None,
        source_path: source_file.clone(),
        target_path: canonical_symlink.clone(),
        media_id: "tvdb-1".to_string(),
        media_type: MediaType::Tv,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();

    let report = report_with_findings(
        Utc::now(),
        vec![
            test_cleanup_finding(
                "tvdb-1",
                1,
                1,
                FindingSeverity::High,
                vec![
                    FindingReason::DuplicateEpisodeSlot,
                    FindingReason::SeasonCountAnomaly,
                ],
                canonical_symlink.to_str().unwrap(),
                source_file.to_str().unwrap(),
            ),
            test_cleanup_finding(
                "tvdb-1",
                1,
                1,
                FindingSeverity::High,
                vec![
                    FindingReason::DuplicateEpisodeSlot,
                    FindingReason::ParserTitleMismatch,
                    FindingReason::SeasonCountAnomaly,
                ],
                suspicious_symlink.to_str().unwrap(),
                source_file.to_str().unwrap(),
            ),
        ],
    );
    let report_path = dir.path().join("duplicates-report.json");
    std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

    let preview = run_prune(&cfg, &db, &report_path, false, false, None, None)
        .await
        .unwrap();
    assert_eq!(preview.candidates, 1);
    assert_eq!(preview.safe_warning_duplicate_candidates, 0);
    assert_eq!(preview.managed_candidates, 0);
    assert_eq!(preview.foreign_candidates, 1);

    let outcome = run_prune(
        &cfg,
        &db,
        &report_path,
        true,
        false,
        None,
        Some(&preview.confirmation_token),
    )
    .await
    .unwrap();

    assert_eq!(outcome.removed, 0);
    assert_eq!(outcome.quarantined, 1);
    assert!(canonical_symlink.is_symlink());
    assert!(!suspicious_symlink.exists() && !suspicious_symlink.is_symlink());
    let updated = db
        .get_link_by_target_path(&canonical_symlink)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.status, LinkStatus::Active);
    assert!(db
        .get_link_by_target_path(&suspicious_symlink)
        .await
        .unwrap()
        .is_none());

    let quarantine_path = cfg
        .cleanup
        .prune
        .quarantine_path
        .join("library/Show - S01E01 suspicious.mkv");
    assert!(quarantine_path.is_symlink());
}

#[cfg(unix)]
#[tokio::test]
async fn test_prune_apply_quarantines_untracked_foreign_symlink() {
    let dir = tempfile::TempDir::new().unwrap();
    let library_root = dir.path().join("library");
    let source_root = dir.path().join("rd");
    std::fs::create_dir_all(&library_root).unwrap();
    std::fs::create_dir_all(&source_root).unwrap();

    let source_file = source_root.join("foreign-source.mkv");
    std::fs::write(&source_file, "video").unwrap();
    let foreign_symlink = library_root.join("Foreign - S01E01.mkv");
    std::os::unix::fs::symlink(&source_file, &foreign_symlink).unwrap();

    let cfg = test_config(&library_root, &source_root);
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let report = report_with_findings(
        Utc::now(),
        vec![high_finding(&foreign_symlink, &source_file)],
    );
    let report_path = dir.path().join("foreign-report.json");
    std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

    let preview = run_prune(&cfg, &db, &report_path, false, false, None, None)
        .await
        .unwrap();
    assert_eq!(preview.managed_candidates, 0);
    assert_eq!(preview.foreign_candidates, 1);

    let outcome = run_prune(
        &cfg,
        &db,
        &report_path,
        true,
        false,
        None,
        Some(&preview.confirmation_token),
    )
    .await
    .unwrap();

    assert_eq!(outcome.removed, 0);
    assert_eq!(outcome.quarantined, 1);
    assert!(!foreign_symlink.exists());

    let quarantine_path = cfg
        .cleanup
        .prune
        .quarantine_path
        .join("library/Foreign - S01E01.mkv");
    assert!(quarantine_path.is_symlink());
    assert!(db
        .get_link_by_target_path(&foreign_symlink)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn test_prune_apply_rejects_stale_report() {
    let dir = tempfile::TempDir::new().unwrap();
    let library_root = dir.path().join("library");
    let source_root = dir.path().join("rd");
    std::fs::create_dir_all(&library_root).unwrap();
    std::fs::create_dir_all(&source_root).unwrap();

    let cfg = test_config(&library_root, &source_root);
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let symlink_path = library_root.join("stale.mkv");
    let source_file = source_root.join("source.mkv");
    let report = report_with_findings(
        Utc::now() - ChronoDuration::hours(cfg.cleanup.prune.max_report_age_hours as i64 + 1),
        vec![high_finding(&symlink_path, &source_file)],
    );
    let report_path = dir.path().join("stale-report.json");
    std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

    let preview = run_prune(&cfg, &db, &report_path, false, false, None, None)
        .await
        .unwrap();
    let err = run_prune(
        &cfg,
        &db,
        &report_path,
        true,
        false,
        None,
        Some(&preview.confirmation_token),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("too old"));
}

#[tokio::test]
async fn test_prune_apply_rejects_tampered_report_token() {
    let dir = tempfile::TempDir::new().unwrap();
    let library_root = dir.path().join("library");
    let source_root = dir.path().join("rd");
    std::fs::create_dir_all(&library_root).unwrap();
    std::fs::create_dir_all(&source_root).unwrap();

    let cfg = test_config(&library_root, &source_root);
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let source_file = source_root.join("source.mkv");
    let report_path = dir.path().join("tampered-report.json");
    let mut report = report_with_findings(
        Utc::now(),
        vec![high_finding(&library_root.join("a.mkv"), &source_file)],
    );
    std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

    let preview = run_prune(&cfg, &db, &report_path, false, false, None, None)
        .await
        .unwrap();

    report
        .findings
        .push(high_finding(&library_root.join("b.mkv"), &source_file));
    report.summary.total_findings = report.findings.len();
    report.summary.high = report.findings.len();
    std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

    let err = run_prune(
        &cfg,
        &db,
        &report_path,
        true,
        false,
        None,
        Some(&preview.confirmation_token),
    )
    .await
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("invalid or missing confirmation token"));
}

#[cfg(unix)]
#[tokio::test]
async fn test_prune_apply_blocks_path_escape() {
    let dir = tempfile::TempDir::new().unwrap();
    let library_root = dir.path().join("library");
    let source_root = dir.path().join("rd");
    let outside_root = dir.path().join("outside");
    std::fs::create_dir_all(&library_root).unwrap();
    std::fs::create_dir_all(&source_root).unwrap();
    std::fs::create_dir_all(&outside_root).unwrap();

    let source_file = source_root.join("source.mkv");
    std::fs::write(&source_file, "video").unwrap();
    let escaped_symlink = outside_root.join("escaped.mkv");
    std::os::unix::fs::symlink(&source_file, &escaped_symlink).unwrap();

    let cfg = test_config(&library_root, &source_root);
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let report = report_with_findings(
        Utc::now(),
        vec![high_finding(&escaped_symlink, &source_file)],
    );
    let report_path = dir.path().join("escaped-report.json");
    std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

    let preview = run_prune(&cfg, &db, &report_path, false, false, None, None)
        .await
        .unwrap();
    let outcome = run_prune(
        &cfg,
        &db,
        &report_path,
        true,
        false,
        None,
        Some(&preview.confirmation_token),
    )
    .await
    .unwrap();

    assert_eq!(outcome.removed, 0);
    assert_eq!(outcome.quarantined, 0);
    assert_eq!(outcome.skipped, 1);
    assert!(escaped_symlink.is_symlink());
}

#[test]
fn test_strip_leading_article() {
    // Case-sensitive: only lowercase article prefix is stripped
    assert_eq!(strip_leading_article("the Matrix"), "Matrix");
    assert_eq!(strip_leading_article("a Beautiful Mind"), "Beautiful Mind");
    assert_eq!(
        strip_leading_article("an Affair to Remember"),
        "Affair to Remember"
    );
    assert_eq!(strip_leading_article("The Matrix"), "The Matrix"); // case-sensitive, no match
    assert_eq!(strip_leading_article("Matrix"), "Matrix"); // no article
}

#[test]
fn test_strip_trailing_year() {
    // Only strips whitespace-delimited year tokens (1900-2099)
    assert_eq!(strip_trailing_year("Breaking Bad 2008"), "Breaking Bad");
    assert_eq!(strip_trailing_year("Movie 2024"), "Movie");
    assert_eq!(
        strip_trailing_year("Breaking Bad (2008)"),
        "Breaking Bad (2008)"
    ); // parens not stripped
    assert_eq!(strip_trailing_year("No Year Here"), "No Year Here");
    assert_eq!(strip_trailing_year("Show Season 1"), "Show Season 1"); // "1" not a valid year
}

#[test]
fn test_is_season_count_anomaly() {
    // Anomaly: excess links compared to expected count
    // Anomaly: 20 links when expected 5 (ratio=4.0, well above 2.0 threshold)
    assert!(is_season_count_anomaly(20, 5));
    // Not anomaly: 5 links when expected 20 (deficit, not excess)
    assert!(!is_season_count_anomaly(5, 20));
    // Not anomaly: actual equals expected
    assert!(!is_season_count_anomaly(5, 5));
    // Not anomaly: 18 links when expected 20 (18 < 20, deficit)
    assert!(!is_season_count_anomaly(18, 20));
}

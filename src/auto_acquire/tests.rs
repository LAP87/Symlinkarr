use super::*;
use chrono::TimeZone;
use std::collections::HashSet;

#[test]
fn failed_retry_backoff_values() {
    assert_eq!(failed_retry_minutes(1), 30);
    assert_eq!(failed_retry_minutes(2), 90);
    assert_eq!(failed_retry_minutes(3), 180);
    assert_eq!(failed_retry_minutes(4), 180); // capped
    assert_eq!(failed_retry_minutes(5), 180); // capped
                                              // Edge: 0 or negative treated as attempt 1
    assert_eq!(failed_retry_minutes(0), 30);
}

#[test]
fn completed_unlinked_retry_backoff_values() {
    assert_eq!(completed_unlinked_retry_minutes(1), 5);
    assert_eq!(completed_unlinked_retry_minutes(2), 15);
    assert_eq!(completed_unlinked_retry_minutes(3), 45);
    assert_eq!(completed_unlinked_retry_minutes(4), 120);
    assert_eq!(completed_unlinked_retry_minutes(5), 120); // capped
                                                          // Edge: 0 or negative treated as attempt 1
    assert_eq!(completed_unlinked_retry_minutes(0), 5);
}

#[test]
fn request_error_outcome_maps_submit_errors_to_failed_terminal_outcomes() {
    let outcome =
        request_error_outcome(anyhow::anyhow!("DMM tv lookup error 429 Too Many Requests"));
    assert_eq!(outcome.status, AutoAcquireStatus::Failed);
    assert_eq!(outcome.reason_code, "auto_acquire_internal_error");
    assert!(outcome.release_title.is_none());
    assert!(outcome.message.contains("429 Too Many Requests"));
}

#[test]
fn record_terminal_outcome_tracks_reason_counts_for_non_success_states() {
    let mut summary = AutoAcquireBatchSummary::default();

    record_terminal_outcome(
        &mut summary,
        &AutoAcquireOutcome {
            status: AutoAcquireStatus::NoResult,
            reason_code: "auto_acquire_no_result_prowlarr_empty",
            release_title: None,
            message: "no result".to_string(),
        },
    );
    record_terminal_outcome(
        &mut summary,
        &AutoAcquireOutcome {
            status: AutoAcquireStatus::Blocked,
            reason_code: "auto_acquire_queue_failing",
            release_title: None,
            message: "blocked".to_string(),
        },
    );
    record_terminal_outcome(
        &mut summary,
        &AutoAcquireOutcome {
            status: AutoAcquireStatus::CompletedLinked,
            reason_code: "auto_acquire_completed_linked",
            release_title: Some("Release".to_string()),
            message: "linked".to_string(),
        },
    );

    assert_eq!(
        summary
            .reason_counts
            .get("auto_acquire_no_result_prowlarr_empty"),
        Some(&1)
    );
    assert_eq!(
        summary.reason_counts.get("auto_acquire_queue_failing"),
        Some(&1)
    );
    assert!(!summary
        .reason_counts
        .contains_key("auto_acquire_completed_linked"));
}

#[test]
fn extract_btih_reads_hash_from_magnet() {
    let magnet = "magnet:?xt=urn:btih:ABC123DEF456&dn=Example";
    assert_eq!(extract_btih(magnet), Some("ABC123DEF456".to_string()));
}

#[test]
fn queue_block_reason_prefers_failed_torrents() {
    let torrent = DecypharrTorrent {
        info_hash: "abc".to_string(),
        name: "Broken".to_string(),
        state: "error".to_string(),
        status: "error".to_string(),
        progress: 0.0,
        is_complete: false,
        bad: true,
        category: "sonarr".to_string(),
        mount_path: String::new(),
        save_path: String::new(),
        content_path: String::new(),
        last_error: "slot full".to_string(),
        added_on: None,
        completed_at: None,
    };

    let (_, reason) = queue_block_reason(&[torrent], 3).unwrap();
    assert!(reason.contains("Broken"));
    assert!(reason.contains("DMM/Decypharr"));
}

#[test]
fn queue_block_reason_flags_capacity_separately() {
    let torrent = DecypharrTorrent {
        info_hash: "abc".to_string(),
        name: "Busy".to_string(),
        state: "downloading".to_string(),
        status: "downloading".to_string(),
        progress: 0.2,
        is_complete: false,
        bad: false,
        category: "radarr".to_string(),
        mount_path: String::new(),
        save_path: String::new(),
        content_path: String::new(),
        last_error: String::new(),
        added_on: None,
        completed_at: None,
    };

    let (guard, _) = queue_block_reason(&[torrent], 1).unwrap();
    assert!(matches!(guard, QueueGuard::Capacity));
}

#[test]
fn find_matching_torrent_falls_back_to_recent_token_match() {
    let tracker = TorrentTracker {
        category: "sonarr".to_string(),
        info_hash: None,
        query_tokens: vec![
            "breaking".to_string(),
            "bad".to_string(),
            "s01e01".to_string(),
        ],
        added_after: Utc.with_ymd_and_hms(2026, 3, 11, 0, 0, 0).unwrap(),
    };
    let torrents = vec![DecypharrTorrent {
        info_hash: "abc".to_string(),
        name: "Breaking.Bad.S01E01.1080p".to_string(),
        state: "downloading".to_string(),
        status: "downloading".to_string(),
        progress: 42.0,
        is_complete: false,
        bad: false,
        category: "sonarr".to_string(),
        mount_path: String::new(),
        save_path: String::new(),
        content_path: String::new(),
        last_error: String::new(),
        added_on: Some(Utc.with_ymd_and_hms(2026, 3, 11, 0, 0, 5).unwrap()),
        completed_at: None,
    }];

    let matched = find_matching_torrent(&torrents, &tracker).unwrap();
    assert_eq!(matched.name, "Breaking.Bad.S01E01.1080p");
}

#[test]
fn normalize_arr_name_ignores_separators() {
    assert_eq!(normalize_arr_name("sonarr_anime"), "sonarranime");
    assert_eq!(normalize_arr_name("sonarr-anime"), "sonarranime");
}

#[test]
fn parse_media_episode_value_roundtrips() {
    assert_eq!(
        parse_media_episode_value("tvdb-12345|1|9").unwrap(),
        ("tvdb-12345".to_string(), 1, 9)
    );
}

#[test]
fn candidate_queries_include_label_and_yearless_fallbacks() {
    let request = AutoAcquireRequest {
        label: "The Darwin Incident (2026) S01E10 upgrade".to_string(),
        query: "Darwins Incident 10".to_string(),
        query_hints: vec!["The Darwin Incident 10".to_string()],
        imdb_id: None,
        categories: vec![5070],
        arr: "sonarr-anime".to_string(),
        library_filter: Some("Anime".to_string()),
        relink_check: RelinkCheck::MediaEpisode {
            media_id: "tvdb-123".to_string(),
            season: 1,
            episode: 10,
        },
    };

    let queries = build_candidate_queries(&request);
    assert_eq!(queries[0], "Darwins Incident 10");
    assert!(queries.contains(&"The Darwin Incident 10".to_string()));
    assert!(queries.contains(&"The Darwin Incident (2026) S01E10".to_string()));
    assert!(queries.contains(&"The Darwin Incident S01E10".to_string()));
    assert!(queries.contains(&"The Darwin Incident S01".to_string()));
    assert!(queries.contains(&"The Darwin Incident".to_string()));
    assert!(queries.contains(&"Darwins Incident".to_string()));
}

#[test]
fn exact_imdb_movie_hits_survive_zero_token_overlap() {
    let request = AutoAcquireRequest {
        label: "1917".to_string(),
        query: "1917".to_string(),
        query_hints: Vec::new(),
        imdb_id: Some("tt8579674".to_string()),
        categories: vec![2000],
        arr: "radarr".to_string(),
        library_filter: Some("Movies".to_string()),
        relink_check: RelinkCheck::MediaId("tmdb-530915".to_string()),
    };
    let title_hit = DmmTitleCandidate {
        title: "1917".to_string(),
        imdb_id: "tt8579674".to_string(),
        year: Some(2019),
    };
    let ranked = rank_dmm_movie_results(
        &request,
        &title_hit,
        vec![DmmTorrentResult {
            title: "Sam.Mendes.War.Film.2019.2160p".to_string(),
            hash: "abc123".to_string(),
            file_size: 42,
        }],
    );

    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].hash, "abc123");
}

#[test]
fn strip_year_tokens_removes_parenthesized_years_only() {
    assert_eq!(
        strip_year_tokens("The Darwin Incident (2026) S01E10"),
        "The Darwin Incident S01E10"
    );
    assert_eq!(strip_year_tokens("Jujutsu Kaisen 3"), "Jujutsu Kaisen 3");
}

#[test]
fn season_token_matches_common_anime_second_season_forms() {
    let normalized = "frieren 2nd season 01 12 complete";
    let tokens: HashSet<_> = normalized.split_whitespace().collect();
    assert!(season_token_matches(&tokens, normalized, 2));

    let normalized = "frieren second season batch";
    let tokens: HashSet<_> = normalized.split_whitespace().collect();
    assert!(season_token_matches(&tokens, normalized, 2));
}

#[test]
fn conflicting_explicit_season_checks_beyond_tenth_season() {
    let normalized = "naruto season 15 complete";
    let tokens: HashSet<_> = normalized.split_whitespace().collect();

    assert!(has_conflicting_explicit_season(&tokens, normalized, 3));
    assert!(!has_conflicting_explicit_season(&tokens, normalized, 15));
}

#[test]
fn anime_batch_fallbacks_reduce_episode_queries() {
    assert_eq!(
        anime_batch_fallbacks("Frieren S01E15"),
        vec!["Frieren S01".to_string(), "Frieren".to_string()]
    );
    assert_eq!(
        anime_batch_fallbacks("Jujutsu Kaisen 3"),
        vec!["Jujutsu Kaisen".to_string()]
    );
}

fn fake_hit(title: &str, seeders: i32, size_gb: i64) -> ProwlarrResult {
    ProwlarrResult {
        guid: format!("guid-{title}"),
        title: title.to_string(),
        indexer_id: 1,
        indexer: "test".to_string(),
        size: size_gb * 1024 * 1024 * 1024,
        seeders: Some(seeders),
        leechers: Some(0),
        download_url: Some("http://example.invalid/download".to_string()),
        magnet_url: Some("magnet:?xt=urn:btih:ABCDEF0123456789".to_string()),
        categories: Vec::new(),
        protocol: "torrent".to_string(),
    }
}

#[test]
fn anime_ranking_prefers_exact_episode_over_packs() {
    let request = AutoAcquireRequest {
        label: "Frieren S01E15".to_string(),
        query: "Frieren S01E15".to_string(),
        query_hints: vec!["Sousou no Frieren 15".to_string()],
        imdb_id: None,
        categories: vec![5070],
        arr: "sonarr-anime".to_string(),
        library_filter: Some("Anime".to_string()),
        relink_check: RelinkCheck::MediaEpisode {
            media_id: "tvdb-123".to_string(),
            season: 1,
            episode: 15,
        },
    };

    let ranked = rank_candidate_hits(
        &request,
        "Frieren S01E15",
        vec![
            fake_hit("[SubsPlease] Sousou no Frieren - 15", 22, 2),
            fake_hit("[SubsPlease] Sousou no Frieren S01 01-28 Complete", 120, 28),
        ],
    );

    assert_eq!(ranked[0].title, "[SubsPlease] Sousou no Frieren - 15");
}

#[test]
fn anime_ranking_filters_wrong_absolute_episode() {
    let request = AutoAcquireRequest {
        label: "Frieren S01E15".to_string(),
        query: "Sousou no Frieren 15".to_string(),
        query_hints: Vec::new(),
        imdb_id: None,
        categories: vec![5070],
        arr: "sonarr-anime".to_string(),
        library_filter: Some("Anime".to_string()),
        relink_check: RelinkCheck::MediaEpisode {
            media_id: "tvdb-123".to_string(),
            season: 1,
            episode: 15,
        },
    };

    let ranked = rank_candidate_hits(
        &request,
        "Sousou no Frieren",
        vec![
            fake_hit("[SubsPlease] Sousou no Frieren - 14", 300, 2),
            fake_hit("[SubsPlease] Sousou no Frieren - 15", 25, 2),
        ],
    );

    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].title, "[SubsPlease] Sousou no Frieren - 15");
}

#[test]
fn anime_ranking_keeps_season_pack_when_exact_missing() {
    let request = AutoAcquireRequest {
        label: "Tales of Wedding Rings S02E13 upgrade".to_string(),
        query: "Tales of Wedding Rings S02E13".to_string(),
        query_hints: Vec::new(),
        imdb_id: None,
        categories: vec![5070],
        arr: "sonarr-anime".to_string(),
        library_filter: Some("Anime".to_string()),
        relink_check: RelinkCheck::MediaEpisode {
            media_id: "tvdb-999".to_string(),
            season: 2,
            episode: 13,
        },
    };

    let ranked = rank_candidate_hits(
        &request,
        "Tales of Wedding Rings S02",
        vec![
            fake_hit("Tales of Wedding Rings S02 01-13 Complete 1080p", 18, 20),
            fake_hit("Tales of Wedding Rings S01 01-12 Complete 1080p", 200, 20),
        ],
    );

    assert_eq!(ranked.len(), 1);
    assert_eq!(
        ranked[0].title,
        "Tales of Wedding Rings S02 01-13 Complete 1080p"
    );
}

#[test]
fn anime_ranking_keeps_absolute_pack_for_later_tvdb_season() {
    let request = AutoAcquireRequest {
        label: "Example Show S02E03".to_string(),
        query: "Example Show S02E03".to_string(),
        query_hints: vec![
            "Example Show S01E15".to_string(),
            "Example Show 15".to_string(),
        ],
        imdb_id: None,
        categories: vec![5070],
        arr: "sonarr-anime".to_string(),
        library_filter: Some("Anime".to_string()),
        relink_check: RelinkCheck::MediaEpisode {
            media_id: "tvdb-555".to_string(),
            season: 2,
            episode: 3,
        },
    };
    let context = build_anime_request_context(&request).unwrap();
    assert_eq!(context.absolute_query_episode, Some(15));
    assert!(context.acceptable_episode_slots.contains(&(2, 3)));
    assert!(context.acceptable_episode_slots.contains(&(1, 15)));

    let hit_tokens_vec = normalized_tokens("Example Show 13-24 Complete 1080p");
    let hit_tokens: HashSet<_> = hit_tokens_vec.iter().map(String::as_str).collect();
    let title_matches = context
        .title_tokens
        .iter()
        .filter(|token| hit_tokens.contains(token.as_str()))
        .count() as i64;
    assert!(title_matches > 0);
    assert!(
        anime_pack_score(&context, "Example Show 13-24 Complete 1080p", title_matches).is_some()
    );

    let ranked = rank_candidate_hits(
        &request,
        "Example Show",
        vec![
            fake_hit("Example Show 13-24 Complete 1080p", 80, 20),
            fake_hit("Example Show S01 01-12 Complete 1080p", 200, 20),
        ],
    );

    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].title, "Example Show 13-24 Complete 1080p");
}

#[test]
fn anime_ranking_accepts_scene_numbered_episode_from_query_hints() {
    let request = AutoAcquireRequest {
        label: "Example Show S02E03".to_string(),
        query: "Example Show S02E03".to_string(),
        query_hints: vec![
            "Example Show S01E15".to_string(),
            "Example Show 15".to_string(),
        ],
        imdb_id: None,
        categories: vec![5070],
        arr: "sonarr-anime".to_string(),
        library_filter: Some("Anime".to_string()),
        relink_check: RelinkCheck::MediaEpisode {
            media_id: "tvdb-555".to_string(),
            season: 2,
            episode: 3,
        },
    };

    let ranked = rank_candidate_hits(
        &request,
        "Example Show",
        vec![
            fake_hit("Example Show S01E15 1080p", 40, 2),
            fake_hit("Example Show S01E14 1080p", 200, 2),
        ],
    );

    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].title, "Example Show S01E15 1080p");
}

#[test]
fn anime_ranking_rejects_seasonless_absolute_pack_without_title_overlap() {
    let request = AutoAcquireRequest {
        label: "Example Show S02E03".to_string(),
        query: "Example Show S02E03".to_string(),
        query_hints: vec![
            "Example Show S01E15".to_string(),
            "Example Show 15".to_string(),
        ],
        imdb_id: None,
        categories: vec![5070],
        arr: "sonarr-anime".to_string(),
        library_filter: Some("Anime".to_string()),
        relink_check: RelinkCheck::MediaEpisode {
            media_id: "tvdb-555".to_string(),
            season: 2,
            episode: 3,
        },
    };

    let ranked = rank_candidate_hits(
        &request,
        "Example Show",
        vec![fake_hit("Different Series 13-24 Complete 1080p", 300, 20)],
    );

    assert!(ranked.is_empty());
}

#[test]
fn anime_context_records_episode_only_hint() {
    // When a hint like "My Hero Academia S05E21" provides episode number
    // but no season, the query_episode should still be recorded so that
    // acceptable_episode_slots includes it.
    let request = AutoAcquireRequest {
        label: "My Hero Academia S05E21".to_string(),
        query: "My Hero Academia S05E21".to_string(),
        query_hints: vec!["Boku no Hero Academia 21".to_string()],
        imdb_id: None,
        categories: vec![5070],
        arr: "sonarr-anime".to_string(),
        library_filter: Some("Anime".to_string()),
        relink_check: RelinkCheck::MediaEpisode {
            media_id: "tvdb-777".to_string(),
            season: 5,
            episode: 21,
        },
    };

    let context = build_anime_request_context(&request).unwrap();
    assert_eq!(context.query_episode, Some(21));
    assert_eq!(context.absolute_query_episode, Some(21));
    assert!(
        context.acceptable_episode_slots.contains(&(5, 21)),
        "acceptable slots should include (5, 21): {:?}",
        context.acceptable_episode_slots
    );
}

#[test]
fn anime_batch_fallbacks_strips_episode_and_returns_title() {
    // "My Hero Academia 21" → ["My Hero Academia"]
    let fallbacks = anime_batch_fallbacks("My Hero Academia 21");
    assert_eq!(fallbacks, vec!["My Hero Academia".to_string()]);
}

#[test]
fn anime_batch_fallbacks_handles_season_token() {
    // "Frieren S01E05" → ["Frieren S01", "Frieren"]
    // parse_season_token reformats as "S{:02}" (drops episode part)
    let fallbacks = anime_batch_fallbacks("Frieren S01E05");
    assert_eq!(
        fallbacks,
        vec!["Frieren S01".to_string(), "Frieren".to_string()]
    );
}

#[test]
fn anime_batch_fallbacks_ignores_single_token() {
    // Single token → no fallbacks
    let fallbacks = anime_batch_fallbacks("Frieren");
    assert!(fallbacks.is_empty());
}

#[test]
fn anime_batch_fallbacks_ignores_year_token() {
    // "Frieren 2023" → standalone year is not a batch marker, so no fallback
    let fallbacks = anime_batch_fallbacks("Frieren 2023");
    assert!(
        fallbacks.is_empty(),
        "standalone year is not a batch marker: {:?}",
        fallbacks
    );
}

#[test]
fn anime_batch_fallbacks_preserve_titles_that_end_in_zero() {
    let fallbacks = anime_batch_fallbacks("Steins Gate 0");
    assert!(
        fallbacks.is_empty(),
        "zero-ending title should not be treated as an episode token: {:?}",
        fallbacks
    );
}

#[test]
fn dmm_search_session_cache_key_uses_season_for_shows() {
    assert_eq!(
        DmmSearchSession::cache_key(DmmMediaKind::Show, "tt123", Some(2)),
        Some(DmmLookupCacheKey {
            kind: DmmMediaKind::Show,
            imdb_id: "tt123".to_string(),
            season: Some(2),
        })
    );
    assert_eq!(
        DmmSearchSession::cache_key(DmmMediaKind::Show, "tt123", None),
        None
    );
}

#[test]
fn is_numbering_token_rejects_release_format_tokens() {
    // BD, BDRip, HDRip, DVDR, BRRip are format tokens treated as noise in query tokens
    // They are filtered out when building title tokens for cleaner DMM queries
    assert!(
        is_numbering_token("BD"),
        "BD should be treated as a noise token"
    );
    assert!(
        is_numbering_token("BDRip"),
        "BDRip should be treated as a noise token"
    );
    assert!(
        is_numbering_token("HDRip"),
        "HDRip should be treated as a noise token"
    );
    assert!(
        is_numbering_token("DVDR"),
        "DVDR should be treated as a noise token"
    );
    assert!(
        is_numbering_token("BRRip"),
        "BRRip should be treated as a noise token"
    );
    assert!(
        is_numbering_token("HDRip"),
        "HDRip should be treated as a noise token"
    );
    // Year and season/episode tokens are also noise
    assert!(is_numbering_token("2020"), "2020 is a year/numbering token");
    assert!(
        is_numbering_token("s01e05"),
        "s01e05 is a season+episode token"
    );
    assert!(is_numbering_token("05"), "05 is a standalone episode token");
    assert!(
        is_numbering_token("upgrade"),
        "upgrade is a numbering token"
    );
}

#[test]
fn clean_request_label_strips_suffixes() {
    assert_eq!(clean_request_label("Show S01E05"), "Show S01E05");
    assert_eq!(clean_request_label("Show S01E05 (unlinked)"), "Show S01E05");
    assert_eq!(clean_request_label("Show S01E05 upgrade"), "Show S01E05");
    assert_eq!(
        clean_request_label("Show S01E05 upgrade (unlinked)"),
        "Show S01E05"
    );
    assert_eq!(clean_request_label("Show S01E05 (new)"), "Show S01E05");
    assert_eq!(clean_request_label("  Show S01E05  "), "Show S01E05");
}

#[test]
fn anime_quality_bonus_for_upgrade() {
    // Upgrade: prefer higher quality
    assert!(anime_quality_bonus(Some("2160p"), true) > anime_quality_bonus(Some("1080p"), true));
    assert!(anime_quality_bonus(Some("1080p"), true) > anime_quality_bonus(Some("720p"), true));
    // Non-upgrade: prefer higher quality (get the best available)
    assert!(anime_quality_bonus(Some("1080p"), false) > anime_quality_bonus(Some("720p"), false));
}

#[test]
fn anime_quality_bonus_unknown_quality() {
    assert_eq!(anime_quality_bonus(None, false), 0);
    assert_eq!(anime_quality_bonus(Some("480p"), false), 0);
}

#[test]
fn dmm_search_session_reuses_cached_lookup() {
    let mut session = DmmSearchSession::default();
    session.cache_lookup(
        DmmMediaKind::Show,
        "tt123",
        Some(1),
        DmmTorrentLookup::Results(vec![DmmTorrentResult {
            title: "Example".to_string(),
            hash: "abc123".to_string(),
            file_size: 42,
        }]),
    );

    let cached = session.get_cached_lookup(DmmMediaKind::Show, "tt123", Some(1));
    let other_season = session.get_cached_lookup(DmmMediaKind::Show, "tt123", Some(2));

    assert!(matches!(cached, Some(DmmTorrentLookup::Results(results)) if results.len() == 1));
    assert!(other_season.is_none());
}

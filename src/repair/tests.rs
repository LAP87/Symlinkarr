use super::*;

// ── TRaSH parser tests ──

#[test]
fn test_parse_trash_sonarr_format() {
    let meta = parse_trash_filename(
            "Breaking Bad (2008) - S01E03 - ...And the Bag's in the River [WEBDL-1080p][x264]-GROUP.mkv",
        );
    assert_eq!(meta.title, "Breaking Bad");
    assert_eq!(meta.year, Some(2008));
    assert_eq!(meta.season, Some(1));
    assert_eq!(meta.episode, Some(3));
    assert_eq!(meta.quality, Some("1080p".to_string()));
    assert!(meta.imdb_id.is_none());
}

#[test]
fn test_parse_trash_radarr_format() {
    let meta = parse_trash_filename(
            "The Matrix (1999) {imdb-tt0133093} [Bluray-2160p][DV HDR10][DTS-HD MA 5.1][x265]-GROUP.mkv",
        );
    assert_eq!(meta.title, "The Matrix");
    assert_eq!(meta.year, Some(1999));
    assert_eq!(meta.imdb_id, Some("tt0133093".to_string()));
    assert_eq!(meta.quality, Some("2160p".to_string()));
    assert!(meta.season.is_none());
}

#[test]
fn test_parse_trash_minimal() {
    let meta = parse_trash_filename("Some Movie (2020).mkv");
    assert_eq!(meta.title, "Some Movie");
    assert_eq!(meta.year, Some(2020));
}

#[test]
fn test_parse_trash_episode_only() {
    let meta = parse_trash_filename("My Show - S02E15 - Episode Title.mkv");
    assert_eq!(meta.title, "My Show");
    assert_eq!(meta.season, Some(2));
    assert_eq!(meta.episode, Some(15));
}

// ── Title normalization ──

#[test]
fn test_normalize_title() {
    assert_eq!(normalize_title("Breaking.Bad"), "breaking bad");
    assert_eq!(
        normalize_title("The_Big_Bang_Theory"),
        "the big bang theory"
    );
    assert_eq!(normalize_title("Game-of-Thrones"), "game of thrones");
}

// ── Scoring tests ──

#[test]
fn test_score_perfect_tv_match() {
    let score = calculate_match_score(MatchScoreInput {
        search_title: "breaking bad",
        candidate_title: "breaking bad",
        search_season: Some(1),
        search_episode: Some(3),
        candidate_season: Some(1),
        candidate_episode: Some(3),
        search_quality: &Some("1080p".to_string()),
        candidate_quality: &Some("1080p".to_string()),
        search_size: Some(4_000_000_000),
        candidate_size: Some(4_200_000_000),
        media_type: MediaType::Tv,
        search_year: None,
        candidate_year: None,
    });
    assert_eq!(score, 1.0); // 0.35 + 0.25 + 0.25 + 0.10 + 0.05
}

#[test]
fn test_score_tv_wrong_episode() {
    let score = calculate_match_score(MatchScoreInput {
        search_title: "breaking bad",
        candidate_title: "breaking bad",
        search_season: Some(1),
        search_episode: Some(3),
        candidate_season: Some(1),
        candidate_episode: Some(5),
        search_quality: &None,
        candidate_quality: &None,
        search_size: None,
        candidate_size: None,
        media_type: MediaType::Tv,
        search_year: None,
        candidate_year: None,
    });
    assert_eq!(score, 0.0); // Wrong episode = instant discard
}

#[test]
fn test_score_tv_missing_season_info() {
    let score = calculate_match_score(MatchScoreInput {
        search_title: "breaking bad",
        candidate_title: "breaking bad",
        search_season: Some(1),
        search_episode: Some(3),
        candidate_season: None,
        candidate_episode: None,
        search_quality: &None,
        candidate_quality: &None,
        search_size: None,
        candidate_size: None,
        media_type: MediaType::Tv,
        search_year: None,
        candidate_year: None,
    });
    assert_eq!(score, 0.0); // TV without S/E info → discarded
}

#[test]
fn test_score_movie_title_only() {
    let score = calculate_match_score(MatchScoreInput {
        search_title: "the matrix",
        candidate_title: "the matrix",
        search_season: None,
        search_episode: None,
        candidate_season: None,
        candidate_episode: None,
        search_quality: &None,
        candidate_quality: &None,
        search_size: None,
        candidate_size: None,
        media_type: MediaType::Movie,
        search_year: None,
        candidate_year: None,
    });
    assert_eq!(score, 0.35); // Title match only — below movie threshold (0.55)
}

#[test]
fn test_score_movie_with_quality() {
    let score = calculate_match_score(MatchScoreInput {
        search_title: "the matrix",
        candidate_title: "the matrix",
        search_season: None,
        search_episode: None,
        candidate_season: None,
        candidate_episode: None,
        search_quality: &Some("2160p".to_string()),
        candidate_quality: &Some("2160p".to_string()),
        search_size: Some(50_000_000_000),
        candidate_size: Some(48_000_000_000),
        media_type: MediaType::Movie,
        search_year: Some(1999),
        candidate_year: Some(1999),
    });
    // 0.35 + 0.15 + 0.10 + 0.05 = 0.65, now above movie threshold with year match
    assert!(score >= MOVIE_THRESHOLD);
}

#[test]
fn test_score_movie_exact_title_and_year_meets_threshold_without_quality_bonus() {
    let score = calculate_match_score(MatchScoreInput {
        search_title: "send help",
        candidate_title: "send help",
        search_season: None,
        search_episode: None,
        candidate_season: None,
        candidate_episode: None,
        search_quality: &Some("2160p".to_string()),
        candidate_quality: &Some("1080p".to_string()),
        search_size: None,
        candidate_size: None,
        media_type: MediaType::Movie,
        search_year: Some(2026),
        candidate_year: Some(2026),
    });
    assert_eq!(score, 0.50);
    assert!(score >= MOVIE_THRESHOLD);
}

#[test]
fn test_score_no_title_match() {
    let score = calculate_match_score(MatchScoreInput {
        search_title: "breaking bad",
        candidate_title: "game of thrones",
        search_season: Some(1),
        search_episode: Some(1),
        candidate_season: Some(1),
        candidate_episode: Some(1),
        search_quality: &None,
        candidate_quality: &None,
        search_size: None,
        candidate_size: None,
        media_type: MediaType::Tv,
        search_year: None,
        candidate_year: None,
    });
    assert_eq!(score, 0.0);
}

// ── Filesystem tests ──

#[test]
fn test_scan_empty_dir_for_dead_symlinks() {
    let repairer = Repairer::new();
    let dir = tempfile::TempDir::new().unwrap();
    let dead = repairer.scan_for_dead_symlinks(&[dir.path().to_path_buf()]);
    assert!(dead.is_empty());
}

#[test]
fn test_scan_finds_dead_symlink_with_trash_name() {
    let repairer = Repairer::new();
    let dir = tempfile::TempDir::new().unwrap();

    // Create a dead symlink with TRaSH-format name
    let link_path = dir
        .path()
        .join("Breaking Bad (2008) - S01E03 - And the Bags in the River [WEBDL-1080p][x264].mkv");
    std::os::unix::fs::symlink("/nonexistent/file.mkv", &link_path).unwrap();

    let dead = repairer.scan_for_dead_symlinks(&[dir.path().to_path_buf()]);
    assert_eq!(dead.len(), 1);

    // Verify TRaSH parsing enriched the dead link
    assert_eq!(dead[0].meta.title, "Breaking Bad");
    assert_eq!(dead[0].meta.year, Some(2008));
    assert_eq!(dead[0].meta.season, Some(1));
    assert_eq!(dead[0].meta.episode, Some(3));
    assert_eq!(dead[0].meta.quality, Some("1080p".to_string()));
    assert_eq!(dead[0].media_type, MediaType::Tv);
}

#[test]
fn test_build_source_catalog_preserves_parsed_anime_quality() {
    let repairer = Repairer::new();
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir
        .path()
        .join("[Okay-Subs] Dan Da Dan - 01 (BD 1080p) [3F475D62].mkv");
    std::fs::write(&file, b"demo").unwrap();

    let catalog = repairer.build_source_catalog(&[dir.path().to_path_buf()], ContentType::Anime);
    assert_eq!(catalog.entries.len(), 1);
    assert_eq!(catalog.entries[0].quality.as_deref(), Some("1080p"));
}

#[test]
fn test_streaming_guard_exact_path_match() {
    let path = PathBuf::from("/mnt/plex/anime/Show/Season 01/Show - S01E01.mkv");
    assert!(is_streaming_symlink_match(
        &path,
        "/mnt/plex/anime/Show/Season 01/Show - S01E01.mkv"
    ));
}

#[test]
fn test_streaming_guard_does_not_match_substring() {
    let path = PathBuf::from("/mnt/plex/anime/Show/Season 01/Show - S01E01.mkv");
    assert!(!is_streaming_symlink_match(
        &path,
        "/mnt/plex/anime/Show/Season 01/Show - S01E0"
    ));
}

#[test]
fn test_rollback_repair_restores_original_symlink() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repairer = Repairer::new();
    let symlink_path = tmp.path().join("Show - S01E01.mkv");
    let original_source = tmp.path().join("old-source.mkv");
    let replacement_source = tmp.path().join("new-source.mkv");
    std::fs::write(&original_source, "old").unwrap();
    std::fs::write(&replacement_source, "new").unwrap();

    std::os::unix::fs::symlink(&replacement_source, &symlink_path).unwrap();

    let dead_link = DeadLink {
        symlink_path: symlink_path.clone(),
        original_source: original_source.clone(),
        media_id: "tvdb-123".to_string(),
        media_type: MediaType::Tv,
        content_type: ContentType::Tv,
        meta: parse_trash_filename("Show - S01E01.mkv"),
        original_size: None,
    };
    let replacement = ReplacementCandidate {
        path: replacement_source.clone(),
        parsed_title: "show".to_string(),
        season: Some(1),
        episode: Some(1),
        quality: None,
        file_size: 0,
        score: 1.0,
    };

    repairer
        .rollback_repair_link(&dead_link, &replacement)
        .unwrap();

    assert_eq!(std::fs::read_link(&symlink_path).unwrap(), original_source);
}

#[test]
fn test_repair_link_replaces_symlink_via_temp_swap() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repairer = Repairer::new();
    let symlink_path = tmp.path().join("Show - S01E01.mkv");
    let original_source = tmp.path().join("old-source.mkv");
    let replacement_source = tmp.path().join("new-source.mkv");
    std::fs::write(&original_source, "old").unwrap();
    std::fs::write(&replacement_source, "new").unwrap();
    std::os::unix::fs::symlink(&original_source, &symlink_path).unwrap();

    let dead_link = DeadLink {
        symlink_path: symlink_path.clone(),
        original_source: original_source.clone(),
        media_id: "tvdb-123".to_string(),
        media_type: MediaType::Tv,
        content_type: ContentType::Tv,
        meta: parse_trash_filename("Show - S01E01.mkv"),
        original_size: None,
    };
    let replacement = ReplacementCandidate {
        path: replacement_source.clone(),
        parsed_title: "show".to_string(),
        season: Some(1),
        episode: Some(1),
        quality: None,
        file_size: 0,
        score: 1.0,
    };

    repairer
        .repair_link(&dead_link, &replacement, false)
        .unwrap();

    assert_eq!(
        std::fs::read_link(&symlink_path).unwrap(),
        replacement_source
    );
    assert!(!symlink_path.with_extension("grt").exists());
}

#[test]
fn test_rollback_repair_removes_repaired_symlink_when_original_source_is_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repairer = Repairer::new();
    let symlink_path = tmp.path().join("Show - S01E01.mkv");
    let original_source = tmp.path().join("old-source.mkv");
    let replacement_source = tmp.path().join("new-source.mkv");
    std::fs::write(&replacement_source, "new").unwrap();

    std::os::unix::fs::symlink(&replacement_source, &symlink_path).unwrap();

    let dead_link = DeadLink {
        symlink_path: symlink_path.clone(),
        original_source: original_source.clone(),
        media_id: "tvdb-123".to_string(),
        media_type: MediaType::Tv,
        content_type: ContentType::Tv,
        meta: parse_trash_filename("Show - S01E01.mkv"),
        original_size: None,
    };
    let replacement = ReplacementCandidate {
        path: replacement_source.clone(),
        parsed_title: "show".to_string(),
        season: Some(1),
        episode: Some(1),
        quality: None,
        file_size: 0,
        score: 1.0,
    };

    let note = repairer
        .rollback_repair_link(&dead_link, &replacement)
        .unwrap();

    assert!(note.contains("removing repaired symlink"));
    assert!(!symlink_path.exists());
    assert!(std::fs::symlink_metadata(&symlink_path).is_err());
}

#[tokio::test]
async fn test_repair_all_detects_fresh_dead_links_without_prior_scan() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    let library_root = tmp.path().join("library");
    let target = library_root.join("Show/Season 01/Show - S01E01.mkv");
    let missing_source = tmp.path().join("rd/missing-source.mkv");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&missing_source, &target).unwrap();

    let record = LinkRecord {
        id: None,
        source_path: missing_source,
        target_path: target.clone(),
        media_id: "tvdb-999".to_string(),
        media_type: MediaType::Tv,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    };
    db.insert_link(&record).await.unwrap();

    let repairer = Repairer::new();
    let results = repairer
        .repair_all(&db, &[], false, &[], Some(&[library_root]), None, None)
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert!(matches!(results[0], RepairResult::Unrepairable { .. }));

    let dead_after = db.get_links_by_status(LinkStatus::Dead).await.unwrap();
    assert_eq!(dead_after.len(), 1);
    assert_eq!(dead_after[0].target_path, target);
}

#[tokio::test]
async fn test_repair_all_detects_orphan_dead_symlink_not_tracked_in_db() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    let library_root = tmp.path().join("library");
    let tagged_dir = library_root.join("Movie Title (2024) {tmdb-42}");
    let target = tagged_dir.join("Movie Title (2024) {tmdb-42} [WEBDL-1080p].mkv");
    let missing_source = tmp.path().join("rd/missing-source.mkv");
    let source_root = tmp.path().join("rd");
    let replacement = source_root.join("Movie.Title.2024.1080p.WEB-DL.x265-GROUP.mkv");

    std::fs::create_dir_all(&tagged_dir).unwrap();
    std::fs::create_dir_all(&source_root).unwrap();
    std::fs::write(&replacement, b"replacement").unwrap();
    std::os::unix::fs::symlink(&missing_source, &target).unwrap();

    let repairer = Repairer::new();
    let selected = vec![LibraryConfig {
        name: "Movies".to_string(),
        path: library_root.clone(),
        media_type: MediaType::Movie,
        content_type: Some(ContentType::Movie),
        depth: 2,
    }];

    let results = repairer
        .repair_all(
            &db,
            &[source_root],
            true,
            &[],
            Some(&[library_root]),
            Some(&selected),
            None,
        )
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    match &results[0] {
        RepairResult::Repaired {
            dead_link,
            replacement,
        } => {
            assert_eq!(dead_link.media_id, "tmdb-42");
            assert_eq!(dead_link.media_type, MediaType::Movie);
            assert_eq!(dead_link.content_type, ContentType::Movie);
            assert_eq!(
                replacement,
                &tmp.path()
                    .join("rd/Movie.Title.2024.1080p.WEB-DL.x265-GROUP.mkv")
            );
        }
        other => panic!("expected repaired orphan dead symlink, got {:?}", other),
    }

    let dead_after = db.get_links_by_status(LinkStatus::Dead).await.unwrap();
    assert!(dead_after.is_empty());
}

#[tokio::test]
async fn test_repair_all_persists_orphan_dead_symlink_when_unrepairable() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    let library_root = tmp.path().join("library");
    let tagged_dir = library_root.join("Movie Title (2024) {tmdb-42}");
    let target = tagged_dir.join("Movie Title (2024) {tmdb-42} [WEBDL-1080p].mkv");
    let missing_source = tmp.path().join("rd/missing-source.mkv");

    std::fs::create_dir_all(&tagged_dir).unwrap();
    std::os::unix::fs::symlink(&missing_source, &target).unwrap();

    let repairer = Repairer::new();
    let selected = vec![LibraryConfig {
        name: "Movies".to_string(),
        path: library_root.clone(),
        media_type: MediaType::Movie,
        content_type: Some(ContentType::Movie),
        depth: 2,
    }];

    let results = repairer
        .repair_all(
            &db,
            &[],
            false,
            &[],
            Some(&[library_root]),
            Some(&selected),
            None,
        )
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert!(matches!(results[0], RepairResult::Unrepairable { .. }));

    let dead_after = db.get_links_by_status(LinkStatus::Dead).await.unwrap();
    assert_eq!(dead_after.len(), 1);
    assert_eq!(dead_after[0].target_path, target);
}

#[tokio::test]
async fn test_repair_all_classifies_missing_symlink_as_stale_in_dry_run() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    let target = tmp.path().join("library/Show/Season 01/S01E01.mkv");
    let source = tmp.path().join("rd/source.mkv");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();

    let record = LinkRecord {
        id: None,
        source_path: source.clone(),
        target_path: target.clone(),
        media_id: "tvdb-123".to_string(),
        media_type: MediaType::Tv,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    };
    db.insert_link(&record).await.unwrap();
    db.mark_dead_path(&target).await.unwrap();

    let repairer = Repairer::new();
    let library_root = tmp.path().join("library");
    let results = repairer
        .repair_all(&db, &[], true, &[], Some(&[library_root]), None, None)
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert!(matches!(results[0], RepairResult::Stale { .. }));
    let dead_after = db.get_links_by_status(LinkStatus::Dead).await.unwrap();
    assert_eq!(dead_after.len(), 1);
}

#[tokio::test]
async fn test_repair_all_marks_stale_dead_links_as_removed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    let target = tmp.path().join("library/Show/Season 01/S01E02.mkv");
    let source = tmp.path().join("rd/source2.mkv");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();

    let record = LinkRecord {
        id: None,
        source_path: source.clone(),
        target_path: target.clone(),
        media_id: "tvdb-456".to_string(),
        media_type: MediaType::Tv,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    };
    db.insert_link(&record).await.unwrap();
    db.mark_dead_path(&target).await.unwrap();

    let repairer = Repairer::new();
    let library_root = tmp.path().join("library");
    let results = repairer
        .repair_all(&db, &[], false, &[], Some(&[library_root]), None, None)
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert!(matches!(results[0], RepairResult::Stale { .. }));
    let removed = db.get_links_by_status(LinkStatus::Removed).await.unwrap();
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].target_path, target);
}

#[tokio::test]
async fn test_repair_all_rejects_unhealthy_source_root() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    let repairer = Repairer::new();
    let missing_source_root = tmp.path().join("missing-rd");
    let err = repairer
        .repair_all(&db, &[missing_source_root], false, &[], None, None, None)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("Refusing repair auto"));
}

#[test]
fn test_destructive_source_exists_rejects_unhealthy_parent() {
    let path = PathBuf::from("/mnt/rd/file.mkv");
    let parent = path.parent().unwrap().to_path_buf();
    let mut source_cache = HashMap::new();
    let mut parent_cache = HashMap::new();
    parent_cache.insert(parent, PathHealth::TransportDisconnected);

    let err = destructive_source_exists(
        "repair dead-link detection",
        &path,
        &mut source_cache,
        &mut parent_cache,
    )
    .unwrap_err();

    assert!(err
        .to_string()
        .contains("Aborting repair dead-link detection"));
}

// ── Pure function unit tests ──────────────────────────────────────────────

#[test]
fn test_normalize_title_removes_punctuation() {
    assert_eq!(
        normalize_title("Breaking.Bad.S01E01"),
        "breaking bad s01e01"
    );
    assert_eq!(normalize_title("Movie_Name_2024"), "movie name 2024");
}

#[test]
fn test_normalize_title_filters_non_alphanumeric() {
    assert_eq!(normalize_title("The@#$%Matrix"), "thematrix");
}

#[test]
fn test_normalize_title_trims_whitespace() {
    assert_eq!(normalize_title("  Hello   World  "), "hello world");
}

#[test]
fn test_extract_quality_handles_common_formats_and_missing_values() {
    assert_eq!(
        extract_quality("Movie.2160p.WEB-DL"),
        Some("2160p".to_string())
    );
    assert_eq!(
        extract_quality("Show.1080p.BluRay"),
        Some("1080p".to_string())
    );
    assert_eq!(extract_quality("Video.720p.x264"), Some("720p".to_string()));
    assert_eq!(extract_quality("Video.480p.XviD"), Some("480p".to_string()));
    assert_eq!(
        extract_quality("Show.S01E01.720p.HDTV"),
        Some("720p".to_string())
    );
    assert_eq!(extract_quality("Movie.NoQualityTag"), None);
}

#[test]
fn test_extract_year() {
    assert_eq!(extract_year("Movie (2008).mkv"), Some(2008));
    assert_eq!(extract_year("Film (1999).mkv"), Some(1999));
}

#[test]
fn test_extract_year_none_for_invalid() {
    assert_eq!(extract_year("No Year Here.mkv"), None);
}

#[test]
fn test_title_tokens_filters_all_noise_tokens() {
    // More comprehensive noise token filtering
    let tokens = title_tokens("breaking bad s01 x264 webrip bluray bdrip hdrip hdtv 720p");
    assert!(tokens.contains(&"breaking".to_string()));
    assert!(tokens.contains(&"bad".to_string()));
    assert!(!tokens.contains(&"s01".to_string()));
    assert!(!tokens.contains(&"x264".to_string()));
    assert!(!tokens.contains(&"webrip".to_string()));
    assert!(!tokens.contains(&"bluray".to_string()));
    assert!(!tokens.contains(&"720p".to_string()));
}

#[test]
fn test_title_tokens_minimum_length_enforced() {
    // Tokens < 2 chars should be filtered; tokens >= 2 chars are kept
    let tokens = title_tokens("a xb ccc dddd");
    assert!(
        !tokens.contains(&"a".to_string()),
        "single char should be filtered"
    );
    assert!(
        tokens.contains(&"xb".to_string()),
        "two char token should be kept"
    );
    assert!(tokens.contains(&"ccc".to_string()));
    assert!(tokens.contains(&"dddd".to_string()));
}

#[test]
fn test_token_is_lookup_noise_recognizes_common_noise() {
    assert!(token_is_lookup_noise("x264"));
    assert!(token_is_lookup_noise("x265"));
    assert!(token_is_lookup_noise("hevc"));
    assert!(token_is_lookup_noise("webrip"));
    assert!(token_is_lookup_noise("webdl"));
    assert!(token_is_lookup_noise("bluray"));
    assert!(token_is_lookup_noise("bdrip"));
    assert!(token_is_lookup_noise("hdtv"));
}

#[test]
fn test_token_is_lookup_noise_strips_pound_prefix_suffix() {
    // "720p" -> strip "p" -> "720" -> all digits -> is_year_token true -> noise
    assert!(token_is_lookup_noise("720p"));
    // "s01" -> strip "s" -> "01" -> all digits -> is_year_token true -> noise
    assert!(token_is_lookup_noise("s01"));
}

#[test]
fn test_trash_season_episode_regex_parses_formats() {
    let re = trash_season_episode_regex();
    assert_eq!(
        re.captures("S01E05")
            .map(|c| (c[1].to_string(), c[2].to_string())),
        Some(("01".to_string(), "05".to_string()))
    );
    assert_eq!(
        re.captures("s2e10")
            .map(|c| (c[1].to_string(), c[2].to_string())),
        Some(("2".to_string(), "10".to_string()))
    );
}

#[test]
fn test_trash_quality_regex_parses_formats() {
    let re = trash_quality_regex();
    assert_eq!(
        re.captures("[1080p]").map(|c| c[1].to_string()),
        Some("1080".to_string())
    );
    assert_eq!(
        re.captures("[2160p HEVC]").map(|c| c[1].to_string()),
        Some("2160".to_string())
    );
}

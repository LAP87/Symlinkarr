use super::*;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::api::test_helpers::spawn_sequence_http_server;
use crate::config::{
    ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, ContentType, DaemonConfig,
    DecypharrConfig, DmmConfig, FeaturesConfig, MatchingConfig, MediaBrowserConfig, PlexConfig,
    ProwlarrConfig, RadarrConfig, RealDebridConfig, SecurityConfig, SonarrConfig, SourceConfig,
    SymlinkConfig, TautulliConfig, WebConfig,
};
use crate::db::Database;
use crate::models::{
    LibraryItem, LinkRecord, LinkStatus, MatchResult, MediaId, MediaType, SourceItem,
};

#[test]
fn test_truncate_str_bytes_ascii() {
    assert_eq!(truncate_str_bytes("hello", 3), "hel");
    assert_eq!(truncate_str_bytes("hello", 10), "hello");
    assert_eq!(truncate_str_bytes("hello", 0), "");
}

#[test]
fn test_truncate_str_bytes_unicode() {
    // "é" is 2 bytes (0xC3 0xA9); cutting at byte 1 must back up to 0.
    let s = "aé";
    assert_eq!(truncate_str_bytes(s, 2), "a");
    assert_eq!(truncate_str_bytes(s, 3), "aé");
}

const DEFAULT_TEMPLATE: &str = "{title} - S{season:02}E{episode:02} - {episode_title}";

#[test]
fn test_format_episode_name_long_ep_title_truncated() {
    let linker = Linker::new(false, true, DEFAULT_TEMPLATE);
    // Build an episode title that will push the filename over 250 bytes.
    let long_ep_title = "A".repeat(240);
    let name = linker.format_episode_name("Show", 1, 1, &long_ep_title, "mkv");
    assert!(
        name.len() <= 250,
        "filename is {} bytes, expected ≤ 250",
        name.len()
    );
    assert!(name.ends_with(".mkv"));
    assert!(name.contains("S01E01"));
}

#[test]
fn test_format_episode_name_long_title_truncated() {
    let linker = Linker::new(false, true, DEFAULT_TEMPLATE);
    // Both title and episode title extremely long.
    let long_title = "B".repeat(200);
    let long_ep = "C".repeat(200);
    let name = linker.format_episode_name(&long_title, 1, 1, &long_ep, "mkv");
    assert!(
        name.len() <= 250,
        "filename is {} bytes, expected ≤ 250",
        name.len()
    );
    assert!(name.ends_with(".mkv"));
}

#[test]
fn test_truncated_filename_safe_for_temp_extension() {
    // Regression: the atomic-swap temp path replaces the extension with ".glt".
    // Verify the worst case (short original ext → longer temp ext) stays under NAME_MAX=255.
    let linker = Linker::new(false, true, DEFAULT_TEMPLATE);
    let long_ep_title = "あ".repeat(100); // 3 bytes each → 300 bytes of episode title
    let name = linker.format_episode_name("Show", 1, 1, &long_ep_title, "mkv");
    assert!(name.len() <= 250, "filename is {} bytes", name.len());

    // Simulate what the symlink code does: swap extension to ".glt"
    let p = std::path::Path::new(&name);
    let temp_name = p.with_extension("glt");
    let temp_len = temp_name.to_str().unwrap().len();
    assert!(
        temp_len <= 255,
        "temp filename is {} bytes, exceeds NAME_MAX",
        temp_len
    );
}

#[test]
fn test_sanitize_filename() {
    assert_eq!(sanitize_filename("Normal Title"), "Normal Title");
    assert_eq!(sanitize_filename("Title: Subtitle"), "Title_ Subtitle");
    assert_eq!(
        sanitize_filename("Who Wants to be a Millionaire?"),
        "Who Wants to be a Millionaire_"
    );
}

#[test]
fn test_sanitize_filename_all_special_chars() {
    // All Windows-incompatible filename characters replaced with underscore
    assert_eq!(
        sanitize_filename("a/b\\c:d*e?f\"g<h>i|j"),
        "a_b_c_d_e_f_g_h_i_j"
    );
    // Unicode characters preserved
    assert_eq!(sanitize_filename("日本語タイトル"), "日本語タイトル");
}

#[test]
fn test_sanitize_filename_trims_whitespace() {
    assert_eq!(sanitize_filename("  Title  "), "Title");
    assert_eq!(sanitize_filename("  Movie (2024)  "), "Movie (2024)");
}

#[test]
fn test_format_episode_name() {
    let linker = Linker::new(false, true, DEFAULT_TEMPLATE);
    let name = linker.format_episode_name("Breaking Bad", 1, 1, "Pilot", "mkv");
    assert_eq!(name, "Breaking Bad - S01E01 - Pilot.mkv");
}

#[test]
fn test_format_episode_name_no_title() {
    let linker = Linker::new(false, true, DEFAULT_TEMPLATE);
    let name = linker.format_episode_name("Breaking Bad", 2, 5, "", "mp4");
    assert_eq!(name, "Breaking Bad - S02E05.mp4");
}

#[test]
fn test_custom_naming_template() {
    let linker = Linker::new_with_options(false, true, "{episode:02}x{season:02} - {title}", true);
    let name = linker.format_episode_name("My Show", 1, 5, "Episode Title", "mkv");
    assert_eq!(name, "05x01 - My Show.mkv");
}

#[test]
fn test_cached_source_exists_short_circuits_missing_parent() {
    let root = tempfile::TempDir::new().unwrap();
    let missing = root.path().join("missing-parent").join("missing-file.mkv");
    let mut source_cache = HashMap::new();
    let mut parent_cache = HashMap::new();

    let exists = cached_source_exists(&missing, &mut source_cache, &mut parent_cache);
    assert!(!exists);

    let parent = missing.parent().unwrap().to_path_buf();
    assert_eq!(parent_cache.get(&parent), Some(&false));
    assert_eq!(source_cache.get(&missing), Some(&false));
}

#[test]
fn test_cached_source_exists_true_for_existing_file() {
    let root = tempfile::TempDir::new().unwrap();
    let file = root.path().join("source.mkv");
    fs::write(&file, "data").unwrap();
    let mut source_cache = HashMap::new();
    let mut parent_cache = HashMap::new();

    let exists = cached_source_exists(&file, &mut source_cache, &mut parent_cache);
    assert!(exists);
    assert_eq!(source_cache.get(&file), Some(&true));
}

#[test]
fn test_destructive_source_exists_rejects_unhealthy_parent() {
    let path = PathBuf::from("/mnt/rd/file.mkv");
    let parent = path.parent().unwrap().to_path_buf();
    let mut source_cache = HashMap::new();
    let mut parent_cache = HashMap::new();
    parent_cache.insert(parent, PathHealth::TransportDisconnected);

    let err = destructive_source_exists(
        "dead-link sweep",
        &path,
        &mut source_cache,
        &mut parent_cache,
    )
    .unwrap_err();

    assert!(err.to_string().contains("Aborting dead-link sweep"));
}

fn sample_movie_match(lib_path: &std::path::Path, source_path: &std::path::Path) -> MatchResult {
    MatchResult {
        library_item: LibraryItem {
            id: MediaId::Tmdb(550),
            path: lib_path.to_path_buf(),
            title: "Sample Movie".to_string(),
            library_name: "Movies".to_string(),
            media_type: MediaType::Movie,
            content_type: ContentType::Movie,
        },
        source_item: SourceItem {
            path: source_path.to_path_buf(),
            parsed_title: "Sample Movie".to_string(),
            season: None,
            episode: None,
            episode_end: None,
            quality: None,
            extension: "mkv".to_string(),
            year: None,
        },
        confidence: 1.0,
        matched_alias: "sample movie".to_string(),
        episode_title: None,
    }
}

fn sample_tv_match(
    lib_path: &std::path::Path,
    source_path: &std::path::Path,
    season: Option<u32>,
    episode: Option<u32>,
) -> MatchResult {
    MatchResult {
        library_item: LibraryItem {
            id: MediaId::Tvdb(81189),
            path: lib_path.to_path_buf(),
            title: "Sample Show".to_string(),
            library_name: "Series".to_string(),
            media_type: MediaType::Tv,
            content_type: ContentType::Tv,
        },
        source_item: SourceItem {
            path: source_path.to_path_buf(),
            parsed_title: "Sample Show".to_string(),
            season,
            episode,
            episode_end: None,
            quality: None,
            extension: "mkv".to_string(),
            year: None,
        },
        confidence: 1.0,
        matched_alias: "sample show".to_string(),
        episode_title: Some("Pilot".to_string()),
    }
}

fn test_config_with_decypharr(base_url: &str, source_root: PathBuf) -> Config {
    Config {
        libraries: vec![],
        sources: vec![SourceConfig {
            name: "RD".to_string(),
            path: source_root,
            media_type: "auto".to_string(),
        }],
        api: ApiConfig::default(),
        realdebrid: RealDebridConfig::default(),
        decypharr: DecypharrConfig {
            url: base_url.to_string(),
            ..DecypharrConfig::default()
        },
        dmm: DmmConfig::default(),
        backup: BackupConfig::default(),
        db_path: ":memory:".to_string(),
        log_level: "info".to_string(),
        daemon: DaemonConfig::default(),
        symlink: SymlinkConfig::default(),
        matching: MatchingConfig::default(),
        prowlarr: ProwlarrConfig::default(),
        bazarr: BazarrConfig::default(),
        tautulli: TautulliConfig::default(),
        plex: PlexConfig::default(),
        emby: MediaBrowserConfig::default(),
        jellyfin: MediaBrowserConfig::default(),
        radarr: RadarrConfig::default(),
        sonarr: SonarrConfig::default(),
        sonarr_anime: SonarrConfig::default(),
        features: FeaturesConfig::default(),
        security: SecurityConfig::default(),
        cleanup: CleanupPolicyConfig::default(),
        web: WebConfig::default(),
        loaded_from: None,
        secret_files: Vec::new(),
    }
}

#[tokio::test]
async fn test_strict_mode_skips_regular_file_overwrite() {
    let dir = tempfile::TempDir::new().unwrap();
    let lib_path = dir.path().join("Sample Movie {tmdb-550}");
    fs::create_dir_all(&lib_path).unwrap();

    let source_path = dir.path().join("rd").join("sample_movie.mkv");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, "video").unwrap();

    let target = lib_path.join("Sample Movie.mkv");
    fs::write(&target, "real-file").unwrap();

    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let linker = Linker::new(false, true, "");
    let m = sample_movie_match(&lib_path, &source_path);
    let target_path = linker.build_target_path(&m).unwrap();
    let mut existing_links = linker
        .preload_existing_links_for_matches(&db, std::slice::from_ref(&m))
        .await
        .unwrap();
    let mut readiness_cache = HashMap::new();

    let outcome = linker
        .create_link(
            &m,
            &target_path,
            &db,
            &mut existing_links,
            None,
            &mut readiness_cache,
        )
        .await
        .unwrap();

    assert_eq!(outcome.outcome, LinkWriteOutcome::Skipped);
    let meta = fs::symlink_metadata(&target).unwrap();
    assert!(meta.file_type().is_file());
    assert_eq!(fs::read_to_string(&target).unwrap(), "real-file");
}

#[tokio::test]
async fn test_directory_target_bails() {
    let dir = tempfile::TempDir::new().unwrap();
    let lib_path = dir.path().join("Sample Movie {tmdb-550}");
    fs::create_dir_all(&lib_path).unwrap();

    let source_path = dir.path().join("rd").join("sample_movie.mkv");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, "video").unwrap();

    let target = lib_path.join("Sample Movie.mkv");
    fs::create_dir_all(&target).unwrap();

    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let linker = Linker::new(false, true, "");
    let m = sample_movie_match(&lib_path, &source_path);
    let target_path = linker.build_target_path(&m).unwrap();
    let mut existing_links = linker
        .preload_existing_links_for_matches(&db, std::slice::from_ref(&m))
        .await
        .unwrap();
    let mut readiness_cache = HashMap::new();

    let err = linker
        .create_link(
            &m,
            &target_path,
            &db,
            &mut existing_links,
            None,
            &mut readiness_cache,
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("directory"));
}

#[tokio::test]
async fn test_process_matches_skips_missing_source_before_write() {
    let dir = tempfile::TempDir::new().unwrap();
    let lib_path = dir.path().join("Sample Movie {tmdb-550}");
    fs::create_dir_all(&lib_path).unwrap();

    let source_path = dir.path().join("rd").join("missing_source.mkv");

    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let linker = Linker::new(false, true, "");

    let summary = linker
        .process_matches(&[sample_movie_match(&lib_path, &source_path)], &db, None)
        .await
        .unwrap();

    assert_eq!(summary.created, 0);
    assert_eq!(summary.updated, 0);
    assert_eq!(summary.skipped, 1);

    let target = lib_path.join("Sample Movie.mkv");
    assert!(db.get_link_by_target_path(&target).await.unwrap().is_none());
    assert!(!target.exists());
}

#[tokio::test]
async fn test_create_link_skips_unreadable_source_before_live_write() {
    let dir = tempfile::TempDir::new().unwrap();
    let lib_path = dir.path().join("Sample Movie {tmdb-550}");
    fs::create_dir_all(&lib_path).unwrap();

    let source_root = dir.path().join("mnt").join("realdebrid").join("__all__");
    let source_path = source_root.join("broken-release").join("sample_movie.mkv");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, "video").unwrap();

    let (base_url, _) =
        spawn_sequence_http_server(&[("HTTP/1.1 503 Service Unavailable", "bad object")]).unwrap();
    let cfg = test_config_with_decypharr(&base_url, source_root.clone());

    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let linker = Linker::new(false, true, "").with_source_readiness_from_config(&cfg);
    let m = sample_movie_match(&lib_path, &source_path);
    let target_path = linker.build_target_path(&m).unwrap();
    let mut existing_links = linker
        .preload_existing_links_for_matches(&db, std::slice::from_ref(&m))
        .await
        .unwrap();
    let mut readiness_cache = HashMap::new();

    let outcome = linker
        .create_link(
            &m,
            &target_path,
            &db,
            &mut existing_links,
            None,
            &mut readiness_cache,
        )
        .await
        .unwrap();

    assert_eq!(outcome.outcome, LinkWriteOutcome::Skipped);
    assert_eq!(outcome.skip_reason, Some("source_unreadable_before_link"));
    assert!(!target_path.exists());
    assert!(db
        .get_link_by_target_path(&target_path)
        .await
        .unwrap()
        .is_none());
}

#[test]
fn test_build_target_path_errors_when_tv_match_lacks_season_or_episode() {
    let dir = tempfile::TempDir::new().unwrap();
    let lib_path = dir.path().join("Sample Show {tvdb-81189}");
    let source_path = dir.path().join("rd").join("sample_show.mkv");
    let linker = Linker::new(false, true, "");

    let missing_season = sample_tv_match(&lib_path, &source_path, None, Some(1));
    let err = linker.build_target_path(&missing_season).unwrap_err();
    assert!(err.to_string().contains("missing season"));

    let missing_episode = sample_tv_match(&lib_path, &source_path, Some(1), None);
    let err = linker.build_target_path(&missing_episode).unwrap_err();
    assert!(err.to_string().contains("missing episode"));
}

#[cfg(unix)]
#[tokio::test]
async fn test_symlink_target_can_be_replaced() {
    let dir = tempfile::TempDir::new().unwrap();
    let lib_path = dir.path().join("Sample Movie {tmdb-550}");
    fs::create_dir_all(&lib_path).unwrap();

    let rd_dir = dir.path().join("rd");
    fs::create_dir_all(&rd_dir).unwrap();
    let old_source = rd_dir.join("old.mkv");
    let new_source = rd_dir.join("new.mkv");
    fs::write(&old_source, "old").unwrap();
    fs::write(&new_source, "new").unwrap();

    let target = lib_path.join("Sample Movie.mkv");
    std::os::unix::fs::symlink(&old_source, &target).unwrap();

    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let linker = Linker::new(false, true, "");
    let m = sample_movie_match(&lib_path, &new_source);
    let target_path = linker.build_target_path(&m).unwrap();
    let mut existing_links = linker
        .preload_existing_links_for_matches(&db, std::slice::from_ref(&m))
        .await
        .unwrap();
    let mut readiness_cache = HashMap::new();

    let outcome = linker
        .create_link(
            &m,
            &target_path,
            &db,
            &mut existing_links,
            None,
            &mut readiness_cache,
        )
        .await
        .unwrap();

    assert_eq!(outcome.outcome, LinkWriteOutcome::Created);
    assert_eq!(outcome.refresh_path, Some(lib_path.clone()));
    assert_eq!(fs::read_link(&target).unwrap(), PathBuf::from(&new_source));
}

#[cfg(unix)]
#[tokio::test]
async fn test_correct_on_disk_symlink_backfills_missing_db_record() {
    let dir = tempfile::TempDir::new().unwrap();
    let lib_path = dir.path().join("Sample Movie {tmdb-550}");
    fs::create_dir_all(&lib_path).unwrap();

    let rd_dir = dir.path().join("rd");
    fs::create_dir_all(&rd_dir).unwrap();
    let source = rd_dir.join("sample.mkv");
    fs::write(&source, "video").unwrap();

    let target = lib_path.join("Sample Movie.mkv");
    std::os::unix::fs::symlink(&source, &target).unwrap();

    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let linker = Linker::new(false, true, "");
    let m = sample_movie_match(&lib_path, &source);
    let target_path = linker.build_target_path(&m).unwrap();
    let mut existing_links = linker
        .preload_existing_links_for_matches(&db, std::slice::from_ref(&m))
        .await
        .unwrap();
    let mut readiness_cache = HashMap::new();

    let outcome = linker
        .create_link(
            &m,
            &target_path,
            &db,
            &mut existing_links,
            None,
            &mut readiness_cache,
        )
        .await
        .unwrap();

    assert_eq!(outcome.outcome, LinkWriteOutcome::Skipped);
    let record = db.get_link_by_target_path(&target).await.unwrap().unwrap();
    assert_eq!(record.source_path, source);
    assert_eq!(record.status, LinkStatus::Active);
}

#[cfg(unix)]
#[tokio::test]
async fn test_same_destination_new_source_is_classified_as_updated() {
    let dir = tempfile::TempDir::new().unwrap();
    let lib_path = dir.path().join("Sample Movie {tmdb-550}");
    fs::create_dir_all(&lib_path).unwrap();

    let rd_dir = dir.path().join("rd");
    fs::create_dir_all(&rd_dir).unwrap();
    let old_source = rd_dir.join("old.mkv");
    let new_source = rd_dir.join("new.mkv");
    fs::write(&old_source, "old").unwrap();
    fs::write(&new_source, "new").unwrap();

    let target = lib_path.join("Sample Movie.mkv");
    std::os::unix::fs::symlink(&old_source, &target).unwrap();

    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    db.insert_link(&LinkRecord {
        id: None,
        source_path: old_source.clone(),
        target_path: target.clone(),
        media_id: "tmdb-550".to_string(),
        media_type: MediaType::Movie,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();

    let linker = Linker::new_with_options(false, true, "", true);
    let m = sample_movie_match(&lib_path, &new_source);
    let target_path = linker.build_target_path(&m).unwrap();
    let mut existing_links = linker
        .preload_existing_links_for_matches(&db, std::slice::from_ref(&m))
        .await
        .unwrap();
    let mut readiness_cache = HashMap::new();

    let outcome = linker
        .create_link(
            &m,
            &target_path,
            &db,
            &mut existing_links,
            None,
            &mut readiness_cache,
        )
        .await
        .unwrap();

    assert_eq!(outcome.outcome, LinkWriteOutcome::Updated);
    assert_eq!(outcome.refresh_path, Some(lib_path.clone()));
    assert_eq!(fs::read_link(&target).unwrap(), PathBuf::from(&new_source));
}

#[cfg(unix)]
#[test]
fn test_verify_link_target_accepts_matching_symlink() {
    let dir = tempfile::TempDir::new().unwrap();
    let source = dir.path().join("rd").join("video.mkv");
    let target = dir.path().join("library").join("video.mkv");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&source, "video").unwrap();
    std::os::unix::fs::symlink(&source, &target).unwrap();

    verify_link_target(&target, &source).unwrap();
}

#[cfg(unix)]
#[test]
fn test_verify_link_target_rejects_wrong_destination() {
    let dir = tempfile::TempDir::new().unwrap();
    let source = dir.path().join("rd").join("video.mkv");
    let other_source = dir.path().join("rd").join("other.mkv");
    let target = dir.path().join("library").join("video.mkv");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&source, "video").unwrap();
    fs::write(&other_source, "video").unwrap();
    std::os::unix::fs::symlink(&other_source, &target).unwrap();

    let err = verify_link_target(&target, &source).unwrap_err();
    assert!(err.to_string().contains("post-write verification failed"));
}

#[test]
fn truncate_filename_to_limit_under_limit() {
    let filename = "Show - S01E01 - Episode Title.mkv";
    let result =
        truncate_filename_to_limit(filename.to_string(), "Show", "Episode Title", 1, 1, "mkv");
    assert_eq!(result, filename);
}

#[test]
fn truncate_filename_to_limit_truncates_episode_title() {
    // Long episode title should be truncated first
    let long_title = "A".repeat(300);
    let filename = format!("Show - S01E01 - {}.mkv", long_title);
    let result = truncate_filename_to_limit(filename, "Show", &long_title, 1, 1, "mkv");
    assert!(
        result.len() <= 250,
        "result len {} should be <= 250",
        result.len()
    );
    assert!(result.contains("Show"));
    assert!(result.contains("S01E01"));
}

#[test]
fn truncate_filename_to_limit_handles_empty_episode_title() {
    // Long filename with empty episode title — should use title-only format
    let long_title = "A".repeat(230);
    let filename = format!("Show - S01E01 - {}.mkv", long_title);
    let result = truncate_filename_to_limit(filename, "Show", "", 1, 1, "mkv");
    assert!(
        result.len() <= 250,
        "result len {} should be <= 250",
        result.len()
    );
    // Should not have double dash before extension
    assert!(!result.contains(" - .mkv"));
}

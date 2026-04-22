use super::*;
use crate::models::{LinkRecord, LinkStatus, MediaType};
use chrono::Utc;
use sqlx::Row;

fn sample_link(source: &str, target: &str) -> LinkRecord {
    LinkRecord {
        id: None,
        source_path: PathBuf::from(source),
        target_path: PathBuf::from(target),
        media_id: "tvdb-12345".to_string(),
        media_type: MediaType::Tv,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    }
}

#[tokio::test]
async fn test_insert_and_get_active_links() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let record = sample_link("/mnt/rd/show/ep01.mkv", "/plex/show/S01E01.mkv");
    let row_id = db.insert_link(&record).await.unwrap();
    assert!(row_id > 0);

    let active = db.get_active_links().await.unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].media_id, "tvdb-12345");
}

#[tokio::test]
async fn test_get_active_links_limited_applies_limit_in_sql() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    db.insert_link(&sample_link(
        "/mnt/rd/show/ep01.mkv",
        "/plex/show/S01E01.mkv",
    ))
    .await
    .unwrap();
    db.insert_link(&sample_link(
        "/mnt/rd/show/ep02.mkv",
        "/plex/show/S01E02.mkv",
    ))
    .await
    .unwrap();
    db.insert_link(&sample_link(
        "/mnt/rd/show/ep03.mkv",
        "/plex/show/S01E03.mkv",
    ))
    .await
    .unwrap();

    let active = db.get_active_links_limited(2).await.unwrap();
    assert_eq!(active.len(), 2);
    assert_eq!(
        active[0].target_path,
        PathBuf::from("/plex/show/S01E03.mkv")
    );
    assert_eq!(
        active[1].target_path,
        PathBuf::from("/plex/show/S01E02.mkv")
    );
}

#[tokio::test]
async fn test_mark_dead() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let record = sample_link("/mnt/rd/show/ep01.mkv", "/plex/show/S01E01.mkv");
    db.insert_link(&record).await.unwrap();

    db.mark_dead("/plex/show/S01E01.mkv").await.unwrap();

    let active = db.get_active_links().await.unwrap();
    assert_eq!(active.len(), 0);

    let dead = db.get_links_by_status(LinkStatus::Dead).await.unwrap();
    assert_eq!(dead.len(), 1);
}

#[tokio::test]
async fn test_get_dead_link_seeds_scoped_filters_by_target_root() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let mut series = sample_link("/mnt/rd/series/ep01.mkv", "/plex/Series/Show/S01E01.mkv");
    series.media_id = "tvdb-series".to_string();
    let mut movies = sample_link("/mnt/rd/movies/m1.mkv", "/plex/Movies/Movie (2020).mkv");
    movies.media_id = "tmdb-movie".to_string();

    db.insert_link(&series).await.unwrap();
    db.insert_link(&movies).await.unwrap();
    db.mark_dead_path(&series.target_path).await.unwrap();
    db.mark_dead_path(&movies.target_path).await.unwrap();

    let roots = vec![PathBuf::from("/plex/Series")];
    let scoped = db.get_dead_link_seeds_scoped(Some(&roots)).await.unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(
        scoped[0].target_path,
        PathBuf::from("/plex/Series/Show/S01E01.mkv")
    );
}

#[tokio::test]
async fn test_get_active_links_scoped_filters_by_target_root() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let mut anime = sample_link("/mnt/rd/anime/ep01.mkv", "/plex/Anime/Show/S01E01.mkv");
    anime.media_id = "tvdb-anime".to_string();
    let mut series = sample_link("/mnt/rd/series/ep01.mkv", "/plex/Series/Show/S01E01.mkv");
    series.media_id = "tvdb-series".to_string();

    db.insert_link(&anime).await.unwrap();
    db.insert_link(&series).await.unwrap();

    let roots = vec![PathBuf::from("/plex/Anime")];
    let scoped = db.get_active_links_scoped(Some(&roots)).await.unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(
        scoped[0].target_path,
        PathBuf::from("/plex/Anime/Show/S01E01.mkv")
    );
}

#[tokio::test]
async fn test_get_active_links_scoped_batches_large_root_lists() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let anime = sample_link("/mnt/rd/anime/ep01.mkv", "/plex/Anime/Show/S01E01.mkv");
    db.insert_link(&anime).await.unwrap();

    let mut roots = (0..1200)
        .map(|i| PathBuf::from(format!("/plex/Noise/{i}")))
        .collect::<Vec<_>>();
    roots.push(PathBuf::from("/plex/Anime"));

    let scoped = db.get_active_links_scoped(Some(&roots)).await.unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(
        scoped[0].target_path,
        PathBuf::from("/plex/Anime/Show/S01E01.mkv")
    );
}

#[tokio::test]
async fn test_get_dead_link_seeds_scoped_batches_large_root_lists() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let anime = sample_link("/mnt/rd/anime/ep01.mkv", "/plex/Anime/Show/S01E01.mkv");
    db.insert_link(&anime).await.unwrap();
    db.mark_dead_path(&anime.target_path).await.unwrap();

    let mut roots = (0..1200)
        .map(|i| PathBuf::from(format!("/plex/Noise/{i}")))
        .collect::<Vec<_>>();
    roots.push(PathBuf::from("/plex/Anime"));

    let scoped = db.get_dead_link_seeds_scoped(Some(&roots)).await.unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(
        scoped[0].target_path,
        PathBuf::from("/plex/Anime/Show/S01E01.mkv")
    );
}

#[tokio::test]
async fn test_get_links_by_targets_returns_only_exact_matches() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let anime = sample_link("/mnt/rd/anime/ep01.mkv", "/plex/Anime/Show/S01E01.mkv");
    let series = sample_link("/mnt/rd/series/ep01.mkv", "/plex/Series/Show/S01E01.mkv");

    db.insert_link(&anime).await.unwrap();
    db.insert_link(&series).await.unwrap();

    let paths = vec![
        PathBuf::from("/plex/Anime/Show/S01E01.mkv"),
        PathBuf::from("/plex/Anime/Show/S01E01.mkv"),
        PathBuf::from("/plex/Missing/Show/S01E01.mkv"),
    ];
    let scoped = db.get_links_by_targets(&paths).await.unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(
        scoped[0].target_path,
        PathBuf::from("/plex/Anime/Show/S01E01.mkv")
    );
}

#[tokio::test]
async fn test_link_exists() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    assert!(!db.link_exists("/plex/show/S01E01.mkv").await.unwrap());

    let record = sample_link("/mnt/rd/show/ep01.mkv", "/plex/show/S01E01.mkv");
    db.insert_link(&record).await.unwrap();
    assert!(db.link_exists("/plex/show/S01E01.mkv").await.unwrap());
}

#[tokio::test]
async fn test_get_stats() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let (active, dead, total) = db.get_stats().await.unwrap();
    assert_eq!((active, dead, total), (0, 0, 0));

    db.insert_link(&sample_link("/a", "/b")).await.unwrap();
    db.insert_link(&sample_link("/c", "/d")).await.unwrap();
    db.mark_dead("/d").await.unwrap();

    let (active, dead, total) = db.get_stats().await.unwrap();
    assert_eq!(active, 1);
    assert_eq!(dead, 1);
    assert_eq!(total, 2);
}

#[tokio::test]
async fn test_cache_set_and_get() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    // Miss
    assert!(db.get_cached("tmdb:12345").await.unwrap().is_none());

    // Set
    db.set_cached("tmdb:12345", r#"{"title":"Test"}"#, 168)
        .await
        .unwrap();

    // Hit
    let cached = db.get_cached("tmdb:12345").await.unwrap();
    assert!(cached.is_some());
    assert!(cached.unwrap().contains("Test"));
}

#[tokio::test]
async fn test_cache_invalidation_removes_entry() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    db.set_cached("tmdb:invalidate-me", r#"{"title":"Test"}"#, 168)
        .await
        .unwrap();
    assert!(db.invalidate_cached("tmdb:invalidate-me").await.unwrap());
    assert!(db.get_cached("tmdb:invalidate-me").await.unwrap().is_none());
    assert!(!db.invalidate_cached("tmdb:invalidate-me").await.unwrap());
}

#[tokio::test]
async fn test_cache_prefix_invalidation_removes_matching_entries_only() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    db.set_cached("tmdb:tv:1", r#"{"title":"One"}"#, 168)
        .await
        .unwrap();
    db.set_cached("tmdb:tv:external_ids:1", r#"{"imdb_id":"tt1"}"#, 168)
        .await
        .unwrap();
    db.set_cached("tmdb:movie:1", r#"{"title":"Movie"}"#, 168)
        .await
        .unwrap();

    let deleted = db.invalidate_cached_prefix("tmdb:tv:").await.unwrap();
    assert_eq!(deleted, 2);
    assert!(db.get_cached("tmdb:tv:1").await.unwrap().is_none());
    assert!(db
        .get_cached("tmdb:tv:external_ids:1")
        .await
        .unwrap()
        .is_none());
    assert!(db.get_cached("tmdb:movie:1").await.unwrap().is_some());
}

#[tokio::test]
async fn test_record_scan() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    // Should not panic
    db.record_scan(100, 500, 42, 10).await.unwrap();
}

#[tokio::test]
async fn test_upsert_on_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();
    let r1 = sample_link("/mnt/rd/old.mkv", "/plex/show/ep.mkv");
    db.insert_link(&r1).await.unwrap();

    // Upsert with same target_path but different source
    let mut r2 = sample_link("/mnt/rd/new.mkv", "/plex/show/ep.mkv");
    r2.media_id = "tmdb-99999".to_string();
    db.insert_link(&r2).await.unwrap();

    let active = db.get_active_links().await.unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].source_path, PathBuf::from("/mnt/rd/new.mkv"));
}

#[cfg(unix)]
#[tokio::test]
async fn test_insert_link_non_utf8_path_fails() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let invalid = PathBuf::from(OsString::from_vec(vec![0xf0, 0x28, 0x8c, 0xbc]));
    let record = LinkRecord {
        id: None,
        source_path: invalid,
        target_path: PathBuf::from("/plex/show/S01E01.mkv"),
        media_id: "tvdb-12345".to_string(),
        media_type: MediaType::Tv,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    };

    let result = db.insert_link(&record).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_migrations_can_move_down_and_up() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    assert_eq!(
        db.current_schema_version().await.unwrap(),
        LATEST_SCHEMA_VERSION
    );
    assert!(db.table_exists("scan_runs").await.unwrap());
    assert!(db.table_exists("link_events").await.unwrap());
    assert!(db.table_exists("acquisition_jobs").await.unwrap());
    assert!(db.table_exists("anime_search_overrides").await.unwrap());

    db.migrate_to_for_tests(2).await.unwrap();
    assert_eq!(db.current_schema_version().await.unwrap(), 2);
    assert!(!db.table_exists("scan_runs").await.unwrap());
    assert!(!db.table_exists("link_events").await.unwrap());
    assert!(!db.table_exists("acquisition_jobs").await.unwrap());
    assert!(!db.table_exists("anime_search_overrides").await.unwrap());

    db.migrate_to_for_tests(LATEST_SCHEMA_VERSION)
        .await
        .unwrap();
    assert_eq!(
        db.current_schema_version().await.unwrap(),
        LATEST_SCHEMA_VERSION
    );
    assert!(db.table_exists("scan_runs").await.unwrap());
    assert!(db.table_exists("link_events").await.unwrap());
    assert!(db.table_exists("acquisition_jobs").await.unwrap());
    assert!(db.table_exists("anime_search_overrides").await.unwrap());
}

#[tokio::test]
async fn test_latest_migration_creates_links_status_target_index() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let index_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_links_status_target'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();

    assert_eq!(index_count, 1);
}

#[tokio::test]
async fn test_latest_migration_creates_plex_refresh_abort_column() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    assert!(db
        .column_exists("scan_runs", "plex_refresh_aborted_due_to_cap")
        .await
        .unwrap());
}

#[tokio::test]
async fn test_latest_migration_creates_media_server_refresh_json_column() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    assert!(db
        .column_exists("scan_runs", "media_server_refresh_json")
        .await
        .unwrap());
}

#[tokio::test]
async fn test_latest_migration_creates_skip_reason_json_column() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    assert!(db
        .column_exists("scan_runs", "skip_reason_json")
        .await
        .unwrap());
}

#[tokio::test]
async fn test_latest_migration_creates_scan_run_origin_column() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    assert!(db.column_exists("scan_runs", "origin").await.unwrap());
}

#[tokio::test]
async fn test_latest_migration_creates_daemon_heartbeat_table() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    assert!(db.table_exists("daemon_heartbeat").await.unwrap());
}

#[tokio::test]
async fn test_latest_migration_creates_anime_search_overrides_table() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    assert!(db.table_exists("anime_search_overrides").await.unwrap());
}

#[tokio::test]
async fn test_anime_search_override_roundtrip_and_update() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    db.upsert_anime_search_override(&AnimeSearchOverrideSeed {
        media_id: "tvdb-12345".to_string(),
        preferred_title: Some("Yofukashi no Uta".to_string()),
        extra_hints: vec![
            "Call of the Night".to_string(),
            "Yofukashi no Uta 2".to_string(),
        ],
        note: Some("Prefer JP scene title for anime batches".to_string()),
    })
    .await
    .unwrap();

    let stored = db
        .get_anime_search_override("tvdb-12345")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.media_id, "tvdb-12345");
    assert_eq!(stored.preferred_title.as_deref(), Some("Yofukashi no Uta"));
    assert_eq!(stored.extra_hints.len(), 2);
    assert_eq!(
        stored.note.as_deref(),
        Some("Prefer JP scene title for anime batches")
    );

    db.upsert_anime_search_override(&AnimeSearchOverrideSeed {
        media_id: "tvdb-12345".to_string(),
        preferred_title: None,
        extra_hints: vec!["Call of the Night".to_string()],
        note: None,
    })
    .await
    .unwrap();

    let updated = db
        .get_anime_search_override("tvdb-12345")
        .await
        .unwrap()
        .unwrap();
    assert!(updated.preferred_title.is_none());
    assert_eq!(updated.extra_hints, vec!["Call of the Night".to_string()]);
    assert!(updated.note.is_none());

    let listed = db.list_anime_search_overrides().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].media_id, "tvdb-12345");
}

#[tokio::test]
async fn test_delete_anime_search_override_reports_presence() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    db.upsert_anime_search_override(&AnimeSearchOverrideSeed {
        media_id: "tvdb-999".to_string(),
        preferred_title: Some("Example".to_string()),
        extra_hints: vec![],
        note: None,
    })
    .await
    .unwrap();

    assert!(db.delete_anime_search_override("tvdb-999").await.unwrap());
    assert!(!db.delete_anime_search_override("tvdb-999").await.unwrap());
    assert!(db
        .get_anime_search_override("tvdb-999")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn test_record_link_event_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    db.record_link_event_fields(
        "created",
        Path::new("/plex/show/S01E01.mkv"),
        Some(Path::new("/mnt/rd/show/ep01.mkv")),
        Some("tvdb-12345"),
        Some("test-event"),
    )
    .await
    .unwrap();

    let row = sqlx::query(
            "SELECT action, target_path, source_path, media_id, note FROM link_events ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let action: String = row.get("action");
    let target_path: String = row.get("target_path");
    let source_path: Option<String> = row.get("source_path");
    let media_id: Option<String> = row.get("media_id");
    let note: Option<String> = row.get("note");

    assert_eq!(action, "created");
    assert_eq!(target_path, "/plex/show/S01E01.mkv");
    assert_eq!(source_path.as_deref(), Some("/mnt/rd/show/ep01.mkv"));
    assert_eq!(media_id.as_deref(), Some("tvdb-12345"));
    assert_eq!(note.as_deref(), Some("test-event"));
}

#[tokio::test]
async fn test_has_active_link_for_episode_matches_slot_pattern() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    db.insert_link(&LinkRecord {
        id: None,
        source_path: PathBuf::from("/mnt/rd/show/ep09.mkv"),
        target_path: PathBuf::from("/plex/Show/Season 01/Show - S01E09.mkv"),
        media_id: "tvdb-12345".to_string(),
        media_type: MediaType::Tv,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();

    assert!(db
        .has_active_link_for_episode("tvdb-12345", 1, 9)
        .await
        .unwrap());
    assert!(!db
        .has_active_link_for_episode("tvdb-12345", 1, 10)
        .await
        .unwrap());
}

#[tokio::test]
async fn test_acquisition_jobs_deduplicate_and_resume_when_due() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let seed = AcquisitionJobSeed {
        request_key: "media:tvdb-12345".to_string(),
        label: "Test Show".to_string(),
        query: "Test Show S01E01".to_string(),
        query_hints: vec!["Example Alt 1".to_string()],
        imdb_id: None,
        categories: vec![5000],
        arr: "sonarr".to_string(),
        library_filter: Some("TV".to_string()),
        relink_kind: AcquisitionRelinkKind::MediaId,
        relink_value: "tvdb-12345".to_string(),
    };

    db.enqueue_acquisition_jobs(std::slice::from_ref(&seed))
        .await
        .unwrap();
    db.enqueue_acquisition_jobs(std::slice::from_ref(&seed))
        .await
        .unwrap();

    let active = db.get_manageable_acquisition_jobs().await.unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].status, AcquisitionJobStatus::Queued);
    assert_eq!(active[0].categories, vec![5000]);
    assert_eq!(active[0].query_hints, vec!["Example Alt 1".to_string()]);
    let counts = db.get_acquisition_job_counts().await.unwrap();
    assert_eq!(counts.queued, 1);
    assert_eq!(counts.active_total(), 1);

    let future_retry = Utc::now() + chrono::Duration::minutes(10);
    db.update_acquisition_job_state(
        active[0].id,
        &AcquisitionJobUpdate {
            status: AcquisitionJobStatus::Failed,
            release_title: None,
            info_hash: None,
            error: Some("rate limited".to_string()),
            next_retry_at: Some(future_retry),
            submitted_at: None,
            completed_at: None,
            increment_attempts: true,
        },
    )
    .await
    .unwrap();

    assert!(db
        .get_manageable_acquisition_jobs()
        .await
        .unwrap()
        .is_empty());

    db.enqueue_acquisition_jobs(std::slice::from_ref(&seed))
        .await
        .unwrap();
    assert!(db
        .get_manageable_acquisition_jobs()
        .await
        .unwrap()
        .is_empty());

    db.update_acquisition_job_state(
        active[0].id,
        &AcquisitionJobUpdate {
            status: AcquisitionJobStatus::Failed,
            release_title: None,
            info_hash: None,
            error: Some("retry now".to_string()),
            next_retry_at: Some(Utc::now() - chrono::Duration::minutes(1)),
            submitted_at: None,
            completed_at: None,
            increment_attempts: false,
        },
    )
    .await
    .unwrap();

    db.enqueue_acquisition_jobs(&[AcquisitionJobSeed {
        query: "Test Show S01E01 1080p".to_string(),
        ..seed
    }])
    .await
    .unwrap();

    let retried = db.get_manageable_acquisition_jobs().await.unwrap();
    assert_eq!(retried.len(), 1);
    assert_eq!(retried[0].status, AcquisitionJobStatus::Queued);
    assert_eq!(retried[0].query, "Test Show S01E01 1080p");
    assert_eq!(retried[0].attempts, 1);
    assert!(retried[0].error.is_none());
}

#[tokio::test]
async fn test_completed_linked_jobs_do_not_reset_on_reenqueue() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let seed = AcquisitionJobSeed {
        request_key: "episode:tvdb-12345:1:1".to_string(),
        label: "Test Show S01E01".to_string(),
        query: "Test Show S01E01".to_string(),
        query_hints: vec!["Alt Title 1".to_string()],
        imdb_id: None,
        categories: vec![5070],
        arr: "sonarr-anime".to_string(),
        library_filter: Some("Anime".to_string()),
        relink_kind: AcquisitionRelinkKind::MediaEpisode,
        relink_value: "tvdb-12345|1|1".to_string(),
    };

    db.enqueue_acquisition_jobs(std::slice::from_ref(&seed))
        .await
        .unwrap();
    let job = db
        .get_manageable_acquisition_jobs()
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

    db.update_acquisition_job_state(
        job.id,
        &AcquisitionJobUpdate {
            status: AcquisitionJobStatus::CompletedLinked,
            release_title: Some("[SubsPlease] Test Show - 01".to_string()),
            info_hash: Some("abc123".to_string()),
            error: None,
            next_retry_at: None,
            submitted_at: Some(Utc::now()),
            completed_at: Some(Utc::now()),
            increment_attempts: false,
        },
    )
    .await
    .unwrap();

    db.enqueue_acquisition_jobs(&[AcquisitionJobSeed {
        query: "Test Show S01E01 upgrade".to_string(),
        ..seed
    }])
    .await
    .unwrap();

    assert!(db
        .get_manageable_acquisition_jobs()
        .await
        .unwrap()
        .is_empty());

    let stored = db
        .list_acquisition_jobs(None, 10)
        .await
        .unwrap()
        .into_iter()
        .find(|item| item.request_key == "episode:tvdb-12345:1:1")
        .unwrap();
    assert_eq!(stored.status, AcquisitionJobStatus::CompletedLinked);
    assert_eq!(
        stored.release_title.as_deref(),
        Some("[SubsPlease] Test Show - 01")
    );
}

// ── Test 1: housekeeping retention boundaries ──────────────────────────────

#[tokio::test]
async fn test_housekeeping_retention_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    // scan_runs: one old (>90 days), one recent
    sqlx::query(
        "INSERT INTO scan_runs (run_at, dry_run, library_items_found, source_items_found, \
             matches_found, links_created, links_updated, dead_marked, links_removed, \
             links_skipped, ambiguous_skipped) \
             VALUES (datetime('now', '-100 days'), 0, 10, 20, 5, 3, 1, 0, 0, 0, 0)",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO scan_runs (run_at, dry_run, library_items_found, source_items_found, \
             matches_found, links_created, links_updated, dead_marked, links_removed, \
             links_skipped, ambiguous_skipped) \
             VALUES (datetime('now', '-10 days'), 0, 5, 10, 3, 2, 0, 0, 0, 0, 0)",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    // link_events: one old (>30 days), one recent
    sqlx::query(
        "INSERT INTO link_events (event_at, action, target_path) \
             VALUES (datetime('now', '-40 days'), 'created', '/plex/old.mkv')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO link_events (event_at, action, target_path) \
             VALUES (datetime('now', '-5 days'), 'created', '/plex/recent.mkv')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    // acquisition_jobs: old completed_linked (>30 days), recent completed_linked,
    // active queued job (should NEVER be deleted regardless of age)
    sqlx::query(
        "INSERT INTO acquisition_jobs \
             (request_key, label, query, categories_json, arr, relink_kind, relink_value, \
              status, updated_at, created_at) \
             VALUES ('key-old-done', 'Old Done', 'q', '[]', 'sonarr', 'media_id', 'tvdb-1', \
                     'completed_linked', datetime('now', '-40 days'), datetime('now', '-40 days'))",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO acquisition_jobs \
             (request_key, label, query, categories_json, arr, relink_kind, relink_value, \
              status, updated_at, created_at) \
             VALUES ('key-recent-done', 'Recent Done', 'q', '[]', 'sonarr', 'media_id', 'tvdb-2', \
                     'completed_linked', datetime('now', '-5 days'), datetime('now', '-5 days'))",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO acquisition_jobs \
             (request_key, label, query, categories_json, arr, relink_kind, relink_value, \
              status, updated_at, created_at) \
             VALUES ('key-active', 'Active Job', 'q', '[]', 'sonarr', 'media_id', 'tvdb-3', \
                     'queued', datetime('now', '-100 days'), datetime('now', '-100 days'))",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    let stats = db.housekeeping_with_vacuum(false).await.unwrap();

    assert_eq!(stats.scan_runs_deleted, 1, "only old scan_run deleted");
    assert_eq!(stats.link_events_deleted, 1, "only old link_event deleted");
    assert_eq!(stats.old_jobs_deleted, 1, "only old completed job deleted");
    assert_eq!(
        stats.expired_api_cache_deleted, 0,
        "no expired API cache rows in this fixture"
    );

    // Verify recent scan_run survives
    let remaining_runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scan_runs")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(remaining_runs, 1);

    // Verify recent link_event survives
    let remaining_events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM link_events")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(remaining_events, 1);

    // Verify active (queued) job is never deleted, recent completed_linked survives
    let remaining_jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM acquisition_jobs")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(remaining_jobs, 2, "active + recent completed both survive");
}

#[tokio::test]
async fn test_housekeeping_with_vacuum_uses_maintenance_connection() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("vacuum.db");
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    db.set_cached("tmdb:tv:expired", "{\"title\":\"expired\"}", 0)
        .await
        .unwrap();

    let stats = db.housekeeping_with_vacuum(true).await.unwrap();

    assert_eq!(stats.expired_api_cache_deleted, 1);
    assert!(db.get_cached("tmdb:tv:expired").await.unwrap().is_none());
    assert!(
        db_path.exists(),
        "database file should still exist after VACUUM"
    );
}

// ── Test 2: recover_stale_downloading_jobs ─────────────────────────────────

fn make_seed(request_key: &str, label: &str, relink_value: &str) -> AcquisitionJobSeed {
    AcquisitionJobSeed {
        request_key: request_key.to_string(),
        label: label.to_string(),
        query: "some query".to_string(),
        query_hints: Vec::new(),
        imdb_id: None,
        categories: vec![5000],
        arr: "sonarr".to_string(),
        library_filter: None,
        relink_kind: AcquisitionRelinkKind::MediaId,
        relink_value: relink_value.to_string(),
    }
}

#[tokio::test]
async fn test_recover_stale_downloading_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let seed_stale = make_seed("key1", "Stale Job", "tvdb-1");
    let seed_recent = make_seed("key2", "Recent Job", "tvdb-2");
    let seed_queued = make_seed("key3", "Queued Job", "tvdb-3");

    db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_stale))
        .await
        .unwrap();
    db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_recent))
        .await
        .unwrap();
    db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_queued))
        .await
        .unwrap();

    let jobs = db.get_manageable_acquisition_jobs().await.unwrap();
    let stale_job = jobs.iter().find(|j| j.request_key == "key1").unwrap();
    let recent_job = jobs.iter().find(|j| j.request_key == "key2").unwrap();

    // Set stale job to downloading with old submitted_at (>60 min ago)
    let old_submitted = Utc::now() - chrono::Duration::hours(3);
    db.update_acquisition_job_state(
        stale_job.id,
        &AcquisitionJobUpdate {
            status: AcquisitionJobStatus::Downloading,
            release_title: Some("Some.Release".to_string()),
            info_hash: Some("abc".to_string()),
            error: None,
            next_retry_at: None,
            submitted_at: Some(old_submitted),
            completed_at: None,
            increment_attempts: true,
        },
    )
    .await
    .unwrap();

    // Set recent job to downloading with submitted_at < 60 min ago
    let recent_submitted = Utc::now() - chrono::Duration::minutes(10);
    db.update_acquisition_job_state(
        recent_job.id,
        &AcquisitionJobUpdate {
            status: AcquisitionJobStatus::Downloading,
            release_title: Some("Other.Release".to_string()),
            info_hash: Some("def".to_string()),
            error: None,
            next_retry_at: None,
            submitted_at: Some(recent_submitted),
            completed_at: None,
            increment_attempts: true,
        },
    )
    .await
    .unwrap();

    let recovered = db.recover_stale_downloading_jobs(60).await.unwrap();
    assert_eq!(recovered, 1, "only one stale job recovered");

    // Stale job is now failed with "stale" in error message
    let stale_stored = db
        .list_acquisition_jobs(None, 10)
        .await
        .unwrap()
        .into_iter()
        .find(|j| j.request_key == "key1")
        .unwrap();
    assert_eq!(stale_stored.status, AcquisitionJobStatus::Failed);
    assert!(
        stale_stored
            .error
            .as_deref()
            .unwrap_or("")
            .to_lowercase()
            .contains("stale"),
        "error should mention stale"
    );

    // Recent downloading job is still downloading
    let recent_stored = db
        .list_acquisition_jobs(None, 10)
        .await
        .unwrap()
        .into_iter()
        .find(|j| j.request_key == "key2")
        .unwrap();
    assert_eq!(recent_stored.status, AcquisitionJobStatus::Downloading);

    // Queued job is unchanged
    let queued_stored = db
        .list_acquisition_jobs(None, 10)
        .await
        .unwrap()
        .into_iter()
        .find(|j| j.request_key == "key3")
        .unwrap();
    assert_eq!(queued_stored.status, AcquisitionJobStatus::Queued);
}

// ── Test 3: MAX_JOB_ATTEMPTS gate ─────────────────────────────────────────

#[tokio::test]
async fn test_max_job_attempts_gate() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let seed = make_seed("key-maxed", "Maxed Job", "tvdb-99");
    db.enqueue_acquisition_jobs(std::slice::from_ref(&seed))
        .await
        .unwrap();

    // Set attempts = 5 via raw SQL to hit the MAX_JOB_ATTEMPTS boundary
    sqlx::query("UPDATE acquisition_jobs SET attempts = 5 WHERE request_key = 'key-maxed'")
        .execute(&db.pool)
        .await
        .unwrap();

    let manageable = db.get_manageable_acquisition_jobs().await.unwrap();
    assert!(
        manageable.is_empty(),
        "job with 5 attempts should be excluded"
    );
}

// ── Test 4: retry_acquisition_jobs by status ───────────────────────────────

#[tokio::test]
async fn test_retry_acquisition_jobs_by_status() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let seed_failed = make_seed("key-failed", "Failed Job", "tvdb-1");
    let seed_blocked = make_seed("key-blocked", "Blocked Job", "tvdb-2");
    let seed_no_result = make_seed("key-no-result", "No Result Job", "tvdb-3");

    db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_failed))
        .await
        .unwrap();
    db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_blocked))
        .await
        .unwrap();
    db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_no_result))
        .await
        .unwrap();

    let jobs = db.get_manageable_acquisition_jobs().await.unwrap();
    let failed_id = jobs
        .iter()
        .find(|j| j.request_key == "key-failed")
        .unwrap()
        .id;
    let blocked_id = jobs
        .iter()
        .find(|j| j.request_key == "key-blocked")
        .unwrap()
        .id;
    let no_result_id = jobs
        .iter()
        .find(|j| j.request_key == "key-no-result")
        .unwrap()
        .id;

    // Set statuses directly
    db.update_acquisition_job_state(
        failed_id,
        &AcquisitionJobUpdate {
            status: AcquisitionJobStatus::Failed,
            release_title: None,
            info_hash: None,
            error: Some("failed".to_string()),
            next_retry_at: None,
            submitted_at: None,
            completed_at: None,
            increment_attempts: false,
        },
    )
    .await
    .unwrap();
    db.update_acquisition_job_state(
        blocked_id,
        &AcquisitionJobUpdate {
            status: AcquisitionJobStatus::Blocked,
            release_title: None,
            info_hash: None,
            error: Some("blocked".to_string()),
            next_retry_at: None,
            submitted_at: None,
            completed_at: None,
            increment_attempts: false,
        },
    )
    .await
    .unwrap();
    db.update_acquisition_job_state(
        no_result_id,
        &AcquisitionJobUpdate {
            status: AcquisitionJobStatus::NoResult,
            release_title: None,
            info_hash: None,
            error: None,
            next_retry_at: None,
            submitted_at: None,
            completed_at: None,
            increment_attempts: false,
        },
    )
    .await
    .unwrap();

    // Retry only failed
    let retried = db
        .retry_acquisition_jobs(&[AcquisitionJobStatus::Failed])
        .await
        .unwrap();
    assert_eq!(retried, 1);

    let failed_now = db
        .list_acquisition_jobs(None, 10)
        .await
        .unwrap()
        .into_iter()
        .find(|j| j.id == failed_id)
        .unwrap();
    assert_eq!(failed_now.status, AcquisitionJobStatus::Queued);

    let blocked_now = db
        .list_acquisition_jobs(None, 10)
        .await
        .unwrap()
        .into_iter()
        .find(|j| j.id == blocked_id)
        .unwrap();
    assert_eq!(blocked_now.status, AcquisitionJobStatus::Blocked);

    // Retry blocked + no_result
    let retried2 = db
        .retry_acquisition_jobs(&[
            AcquisitionJobStatus::Blocked,
            AcquisitionJobStatus::NoResult,
        ])
        .await
        .unwrap();
    assert_eq!(retried2, 2);
}

// ── Test 5: get_manageable_acquisition_jobs ordering ──────────────────────

#[tokio::test]
async fn test_manageable_jobs_priority_ordering() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let seed_queued = make_seed("key-queued", "Queued", "tvdb-1");
    let seed_dl = make_seed("key-dl", "Downloading", "tvdb-2");
    let seed_rl = make_seed("key-rl", "Relinking", "tvdb-3");

    db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_queued))
        .await
        .unwrap();
    db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_dl))
        .await
        .unwrap();
    db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_rl))
        .await
        .unwrap();

    let jobs = db.get_manageable_acquisition_jobs().await.unwrap();
    let dl_id = jobs.iter().find(|j| j.request_key == "key-dl").unwrap().id;
    let rl_id = jobs.iter().find(|j| j.request_key == "key-rl").unwrap().id;

    db.update_acquisition_job_state(
        dl_id,
        &AcquisitionJobUpdate {
            status: AcquisitionJobStatus::Downloading,
            release_title: None,
            info_hash: None,
            error: None,
            next_retry_at: None,
            submitted_at: Some(Utc::now()),
            completed_at: None,
            increment_attempts: false,
        },
    )
    .await
    .unwrap();

    db.update_acquisition_job_state(
        rl_id,
        &AcquisitionJobUpdate {
            status: AcquisitionJobStatus::Relinking,
            release_title: None,
            info_hash: None,
            error: None,
            next_retry_at: None,
            submitted_at: None,
            completed_at: None,
            increment_attempts: false,
        },
    )
    .await
    .unwrap();

    let ordered = db.get_manageable_acquisition_jobs().await.unwrap();
    assert_eq!(ordered.len(), 3);
    assert_eq!(ordered[0].status, AcquisitionJobStatus::Downloading);
    assert_eq!(ordered[1].status, AcquisitionJobStatus::Relinking);
    assert_eq!(ordered[2].status, AcquisitionJobStatus::Queued);
}

// ── Test 6: list_acquisition_jobs with status filter ──────────────────────

#[tokio::test]
async fn test_list_acquisition_jobs_status_filter() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let seed_q = make_seed("key-q", "Queued", "tvdb-1");
    let seed_f = make_seed("key-f", "Failed", "tvdb-2");
    let seed_b = make_seed("key-b", "Blocked", "tvdb-3");

    db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_q))
        .await
        .unwrap();
    db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_f))
        .await
        .unwrap();
    db.enqueue_acquisition_jobs(std::slice::from_ref(&seed_b))
        .await
        .unwrap();

    let all_jobs = db.get_manageable_acquisition_jobs().await.unwrap();
    let f_id = all_jobs
        .iter()
        .find(|j| j.request_key == "key-f")
        .unwrap()
        .id;
    let b_id = all_jobs
        .iter()
        .find(|j| j.request_key == "key-b")
        .unwrap()
        .id;

    db.update_acquisition_job_state(
        f_id,
        &AcquisitionJobUpdate {
            status: AcquisitionJobStatus::Failed,
            release_title: None,
            info_hash: None,
            error: Some("err".to_string()),
            next_retry_at: None,
            submitted_at: None,
            completed_at: None,
            increment_attempts: false,
        },
    )
    .await
    .unwrap();
    db.update_acquisition_job_state(
        b_id,
        &AcquisitionJobUpdate {
            status: AcquisitionJobStatus::Blocked,
            release_title: None,
            info_hash: None,
            error: Some("blocked".to_string()),
            next_retry_at: None,
            submitted_at: None,
            completed_at: None,
            increment_attempts: false,
        },
    )
    .await
    .unwrap();

    let failed_only = db
        .list_acquisition_jobs(Some(&[AcquisitionJobStatus::Failed]), 10)
        .await
        .unwrap();
    assert_eq!(failed_only.len(), 1);
    assert_eq!(failed_only[0].status, AcquisitionJobStatus::Failed);

    let all_listed = db.list_acquisition_jobs(None, 10).await.unwrap();
    assert_eq!(all_listed.len(), 3);
}

// ── Test 7: RD torrent operations ─────────────────────────────────────────

#[tokio::test]
async fn test_rd_torrent_operations() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    // Insert two torrents: one downloaded (with files), one waiting_files
    db.upsert_rd_torrent(
        "id1",
        "HASH1",
        "Show S01 1080p",
        "downloaded",
        r#"{"files":[{"path":"/ep01.mkv","bytes":1000000000,"id":1}]}"#,
    )
    .await
    .unwrap();

    db.upsert_rd_torrent(
        "id2",
        "HASH2",
        "Movie 2020",
        "waiting_files",
        r#"{"files":[]}"#,
    )
    .await
    .unwrap();

    // get_rd_torrents returns both
    let all = db.get_rd_torrents().await.unwrap();
    assert_eq!(all.len(), 2);

    // rd_torrent_downloaded_by_hash: case-insensitive
    assert!(db.rd_torrent_downloaded_by_hash("HASH1").await.unwrap());
    assert!(db.rd_torrent_downloaded_by_hash("hash1").await.unwrap());
    assert!(!db.rd_torrent_downloaded_by_hash("HASH2").await.unwrap());
    assert!(!db
        .rd_torrent_downloaded_by_hash("HASH_UNKNOWN")
        .await
        .unwrap());

    // get_rd_torrent_counts: (cached_with_files, total_downloaded)
    // id1 is downloaded with non-empty files -> cached=1, total=1
    let (cached, total) = db.get_rd_torrent_counts().await.unwrap();
    assert_eq!(total, 1, "only downloaded torrents counted");
    assert_eq!(cached, 1, "id1 has non-empty files_json");

    // delete_rd_torrent removes id1
    db.delete_rd_torrent("id1").await.unwrap();
    let remaining = db.get_rd_torrents().await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].0, "id2");
}

// ── Test 8: record_scan_run roundtrip ─────────────────────────────────────

#[tokio::test]
async fn test_record_scan_run_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let run = ScanRunRecord {
            origin: crate::db::ScanRunOrigin::Cli,
            dry_run: true,
            run_token: Some("scan-run-db".to_string()),
            library_items_found: 42,
            source_items_found: 100,
            matches_found: 38,
            links_created: 10,
            links_updated: 5,
            dead_marked: 2,
            links_removed: 1,
            links_skipped: 3,
            ambiguous_skipped: 7,
            skip_reason_json: Some(
                r#"{"already_correct":2,"source_missing_before_link":1,"ambiguous_match":7}"#
                    .to_string(),
            ),
            library_filter: Some("Anime".to_string()),
            search_missing: true,
            runtime_checks_ms: 11,
            library_scan_ms: 22,
            source_inventory_ms: 33,
            matching_ms: 44,
            title_enrichment_ms: 55,
            linking_ms: 66,
            plex_refresh_ms: 77,
            plex_refresh_requested_paths: 12,
            plex_refresh_unique_paths: 10,
            plex_refresh_planned_batches: 5,
            plex_refresh_coalesced_batches: 2,
            plex_refresh_coalesced_paths: 7,
            plex_refresh_refreshed_batches: 4,
            plex_refresh_refreshed_paths_covered: 11,
            plex_refresh_skipped_batches: 1,
            plex_refresh_unresolved_paths: 0,
            plex_refresh_capped_batches: 1,
            plex_refresh_aborted_due_to_cap: true,
            plex_refresh_failed_batches: 0,
            media_server_refresh_json: Some(
                r#"[{"server":"plex","requested_targets":2,"refresh":{"requested_paths":2,"unique_paths":2,"planned_batches":1,"coalesced_batches":0,"coalesced_paths":0,"refreshed_batches":1,"refreshed_paths_covered":2,"skipped_batches":0,"unresolved_paths":0,"capped_batches":0,"aborted_due_to_cap":false,"failed_batches":0}}]"#.to_string(),
            ),
            dead_link_sweep_ms: 88,
            cache_hit_ratio: Some(1.0),
            candidate_slots: 1234,
            scored_candidates: 56,
            exact_id_hits: 7,
            auto_acquire_requests: 10,
            auto_acquire_missing_requests: 5,
            auto_acquire_cutoff_requests: 5,
            auto_acquire_dry_run_hits: 8,
            auto_acquire_submitted: 0,
            auto_acquire_no_result: 2,
            auto_acquire_blocked: 0,
            auto_acquire_failed: 0,
            auto_acquire_completed_linked: 0,
            auto_acquire_completed_unlinked: 0,
        };

    db.record_scan_run(&run).await.unwrap();

    let row = sqlx::query(
            "SELECT origin, dry_run, library_filter, search_missing, library_items_found, source_items_found, matches_found, \
             links_created, links_updated, dead_marked, links_removed, links_skipped, \
             ambiguous_skipped, runtime_checks_ms, plex_refresh_planned_batches, plex_refresh_capped_batches, \
             plex_refresh_aborted_due_to_cap, media_server_refresh_json, \
             cache_hit_ratio, candidate_slots, auto_acquire_requests, auto_acquire_dry_run_hits, auto_acquire_no_result \
             FROM scan_runs ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();

    let origin: String = row.get("origin");
    assert_eq!(origin, "cli");
    let dry_run: i64 = row.get("dry_run");
    assert_eq!(dry_run, 1, "dry_run stored as 1");
    let library_filter: Option<String> = row.get("library_filter");
    assert_eq!(library_filter.as_deref(), Some("Anime"));
    let search_missing: i64 = row.get("search_missing");
    assert_eq!(search_missing, 1);
    let lib: i64 = row.get("library_items_found");
    assert_eq!(lib, 42);
    let src: i64 = row.get("source_items_found");
    assert_eq!(src, 100);
    let matches: i64 = row.get("matches_found");
    assert_eq!(matches, 38);
    let created: i64 = row.get("links_created");
    assert_eq!(created, 10);
    let updated: i64 = row.get("links_updated");
    assert_eq!(updated, 5);
    let dead: i64 = row.get("dead_marked");
    assert_eq!(dead, 2);
    let removed: i64 = row.get("links_removed");
    assert_eq!(removed, 1);
    let skipped: i64 = row.get("links_skipped");
    assert_eq!(skipped, 3);
    let ambiguous: i64 = row.get("ambiguous_skipped");
    assert_eq!(ambiguous, 7);
    let runtime_checks_ms: i64 = row.get("runtime_checks_ms");
    assert_eq!(runtime_checks_ms, 11);
    let planned_batches: i64 = row.get("plex_refresh_planned_batches");
    assert_eq!(planned_batches, 5);
    let capped_batches: i64 = row.get("plex_refresh_capped_batches");
    assert_eq!(capped_batches, 1);
    let aborted_due_to_cap: i64 = row.get("plex_refresh_aborted_due_to_cap");
    assert_eq!(aborted_due_to_cap, 1);
    let media_server_refresh_json: Option<String> = row.get("media_server_refresh_json");
    assert!(media_server_refresh_json
        .unwrap()
        .contains("\"server\":\"plex\""));
    let cache_hit_ratio: f64 = row.get("cache_hit_ratio");
    assert_eq!(cache_hit_ratio, 1.0);
    let candidate_slots: i64 = row.get("candidate_slots");
    assert_eq!(candidate_slots, 1234);
    let auto_acquire_requests: i64 = row.get("auto_acquire_requests");
    assert_eq!(auto_acquire_requests, 10);
    let auto_acquire_dry_run_hits: i64 = row.get("auto_acquire_dry_run_hits");
    assert_eq!(auto_acquire_dry_run_hits, 8);
    let auto_acquire_no_result: i64 = row.get("auto_acquire_no_result");
    assert_eq!(auto_acquire_no_result, 2);
}

// ── Test 9: mark_removed and get_link_by_target ────────────────────────────

#[tokio::test]
async fn test_mark_removed_and_get_link_by_target() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let record = sample_link("/mnt/rd/show/ep01.mkv", "/plex/show/S01E01.mkv");
    db.insert_link(&record).await.unwrap();

    // get_link_by_target_path returns it
    let found = db
        .get_link_by_target_path(Path::new("/plex/show/S01E01.mkv"))
        .await
        .unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().status, LinkStatus::Active);

    // mark_removed_path transitions to Removed
    db.mark_removed_path(Path::new("/plex/show/S01E01.mkv"))
        .await
        .unwrap();

    let removed_links = db.get_links_by_status(LinkStatus::Removed).await.unwrap();
    assert_eq!(removed_links.len(), 1);
    assert_eq!(
        removed_links[0].target_path,
        PathBuf::from("/plex/show/S01E01.mkv")
    );

    // get_active_links does not include it
    let active = db.get_active_links().await.unwrap();
    assert!(active.is_empty());
}

// ── Test 10: insert_link_in_tx – commit and rollback ──────────────────────

#[tokio::test]
async fn test_insert_link_in_tx_commit() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let record = sample_link("/mnt/rd/show/ep01.mkv", "/plex/show/S01E01.mkv");

    let mut tx = db.begin().await.unwrap();
    let id = db.insert_link_in_tx(&record, &mut tx).await.unwrap();
    assert!(id > 0);
    tx.commit().await.unwrap();

    let active = db.get_active_links().await.unwrap();
    assert_eq!(active.len(), 1);
}

#[tokio::test]
async fn test_insert_link_in_tx_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let record = sample_link("/mnt/rd/show/ep01.mkv", "/plex/show/S01E01.mkv");

    {
        let mut tx = db.begin().await.unwrap();
        db.insert_link_in_tx(&record, &mut tx).await.unwrap();
        // Drop tx without committing — implicit rollback
    }

    let active = db.get_active_links().await.unwrap();
    assert!(active.is_empty(), "rolled-back insert should not persist");
}

// ── Test 11: escape_sql_like_pattern ──────────────────────────────────────

#[tokio::test]
async fn test_escape_sql_like_pattern_special_chars() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    // Insert a link whose target path contains % and _
    let record = sample_link(
        "/mnt/rd/show/ep01.mkv",
        "/plex/100%_Show/Season 01/S01E01.mkv",
    );
    db.insert_link(&record).await.unwrap();

    // Also insert a link that should NOT match
    let other = sample_link("/mnt/rd/other/ep01.mkv", "/plex/Other/S01E01.mkv");
    db.insert_link(&other).await.unwrap();

    // Scope to the exact directory containing % and _
    let roots = vec![PathBuf::from("/plex/100%_Show")];
    let scoped = db.get_links_scoped(Some(&roots)).await.unwrap();

    // Only the link under the special-character path should match
    assert_eq!(scoped.len(), 1);
    assert_eq!(
        scoped[0].target_path,
        PathBuf::from("/plex/100%_Show/Season 01/S01E01.mkv")
    );
}

// ── Test 12: get_links_scoped (all statuses) ──────────────────────────────

#[tokio::test]
async fn test_get_links_scoped_all_statuses() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    // Three links under /plex/TV, one under /plex/Movies
    let active = sample_link("/mnt/rd/a.mkv", "/plex/TV/Show/S01E01.mkv");
    let mut dead = sample_link("/mnt/rd/b.mkv", "/plex/TV/Show/S01E02.mkv");
    dead.media_id = "tvdb-dead".to_string();
    let mut removed = sample_link("/mnt/rd/c.mkv", "/plex/TV/Show/S01E03.mkv");
    removed.media_id = "tvdb-removed".to_string();
    let other = sample_link("/mnt/rd/d.mkv", "/plex/Movies/Movie.mkv");

    db.insert_link(&active).await.unwrap();
    db.insert_link(&dead).await.unwrap();
    db.insert_link(&removed).await.unwrap();
    db.insert_link(&other).await.unwrap();

    db.mark_dead_path(Path::new("/plex/TV/Show/S01E02.mkv"))
        .await
        .unwrap();
    db.mark_removed_path(Path::new("/plex/TV/Show/S01E03.mkv"))
        .await
        .unwrap();

    let roots = vec![PathBuf::from("/plex/TV")];
    let scoped = db.get_links_scoped(Some(&roots)).await.unwrap();

    assert_eq!(scoped.len(), 3, "active + dead + removed under /plex/TV");
    let statuses: Vec<LinkStatus> = scoped.iter().map(|l| l.status).collect();
    assert!(statuses.contains(&LinkStatus::Active));
    assert!(statuses.contains(&LinkStatus::Dead));
    assert!(statuses.contains(&LinkStatus::Removed));

    // Movies link not included
    assert!(!scoped
        .iter()
        .any(|l| l.target_path == Path::new("/plex/Movies/Movie.mkv")));
}

// ── Test 13: empty DB edge cases ──────────────────────────────────────────

#[tokio::test]
async fn test_empty_db_edge_cases() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let (cached, total) = db.get_rd_torrent_counts().await.unwrap();
    assert_eq!((cached, total), (0, 0));

    let counts = db.get_acquisition_job_counts().await.unwrap();
    assert_eq!(counts.queued, 0);
    assert_eq!(counts.downloading, 0);
    assert_eq!(counts.relinking, 0);
    assert_eq!(counts.blocked, 0);
    assert_eq!(counts.no_result, 0);
    assert_eq!(counts.failed, 0);
    assert_eq!(counts.completed_unlinked, 0);
    assert_eq!(counts.active_total(), 0);

    let stats = db.housekeeping_with_vacuum(false).await.unwrap();
    assert_eq!(stats.scan_runs_deleted, 0);
    assert_eq!(stats.link_events_deleted, 0);
    assert_eq!(stats.old_jobs_deleted, 0);
    assert_eq!(stats.expired_api_cache_deleted, 0);
}

#[tokio::test]
async fn test_get_dead_links() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let r = sample_link("/mnt/rd/show/ep01.mkv", "/plex/show/S01E01.mkv");
    db.insert_link(&r).await.unwrap();
    db.mark_dead("/plex/show/S01E01.mkv").await.unwrap();

    let dead = db.get_dead_links().await.unwrap();
    assert_eq!(dead.len(), 1);
    assert_eq!(dead[0].status, LinkStatus::Dead);
    assert_eq!(dead[0].target_path, PathBuf::from("/plex/show/S01E01.mkv"));
}

#[tokio::test]
async fn test_get_web_stats() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    // Initially all zeros, no last_scan
    let stats = db.get_web_stats().await.unwrap();
    assert_eq!(stats.active_links, 0);
    assert_eq!(stats.dead_links, 0);
    assert_eq!(stats.total_scans, 0);
    assert!(stats.last_scan.is_none());

    // Insert one active, one dead link
    db.insert_link(&sample_link("/mnt/a", "/plex/a"))
        .await
        .unwrap();
    db.insert_link(&sample_link("/mnt/b", "/plex/b"))
        .await
        .unwrap();
    db.mark_dead("/plex/b").await.unwrap();

    // Insert a scan_run
    db.record_scan_run(&ScanRunRecord {
        dry_run: false,
        library_items_found: 10,
        source_items_found: 20,
        matches_found: 5,
        links_created: 2,
        skip_reason_json: None,
        ..Default::default()
    })
    .await
    .unwrap();

    let stats = db.get_web_stats().await.unwrap();
    assert_eq!(stats.active_links, 1);
    assert_eq!(stats.dead_links, 1);
    assert_eq!(stats.total_scans, 1);
    assert!(stats.last_scan.is_some());
}

#[tokio::test]
async fn test_get_scan_history() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    // Insert 3 scan runs
    for i in 0..3i64 {
        db.record_scan_run(&ScanRunRecord {
            dry_run: i == 0,
            library_items_found: i * 10,
            source_items_found: i * 20,
            matches_found: i * 5,
            links_created: i,
            links_updated: i,
            dead_marked: 0,
            links_removed: 0,
            links_skipped: 0,
            ambiguous_skipped: 0,
            skip_reason_json: None,
            ..Default::default()
        })
        .await
        .unwrap();
    }

    // get_scan_history(2) returns 2 in reverse chronological order (latest first)
    let history = db.get_scan_history(2).await.unwrap();
    assert_eq!(history.len(), 2);
    // Most recent has library_items_found=20 (i=2), next has 10 (i=1)
    assert_eq!(history[0].library_items_found, 20);
    assert_eq!(history[1].library_items_found, 10);

    // Full history returns all 3
    let all = db.get_scan_history(10).await.unwrap();
    assert_eq!(all.len(), 3);
}

#[tokio::test]
async fn test_get_latest_scan_run_for_origin() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    db.record_scan_run(&ScanRunRecord {
        origin: crate::db::ScanRunOrigin::Daemon,
        run_token: Some("daemon-run".to_string()),
        matches_found: 5,
        ..Default::default()
    })
    .await
    .unwrap();
    db.record_scan_run(&ScanRunRecord {
        origin: crate::db::ScanRunOrigin::Web,
        run_token: Some("web-run".to_string()),
        matches_found: 8,
        ..Default::default()
    })
    .await
    .unwrap();

    let daemon_run = db
        .get_latest_scan_run_for_origin(crate::db::ScanRunOrigin::Daemon)
        .await
        .unwrap()
        .unwrap();
    let web_run = db
        .get_latest_scan_run_for_origin(crate::db::ScanRunOrigin::Web)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(daemon_run.origin, crate::db::ScanRunOrigin::Daemon);
    assert_eq!(daemon_run.run_token.as_deref(), Some("daemon-run"));
    assert_eq!(web_run.origin, crate::db::ScanRunOrigin::Web);
    assert_eq!(web_run.run_token.as_deref(), Some("web-run"));
}

#[tokio::test]
async fn test_daemon_heartbeat_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    db.record_daemon_heartbeat("scan", Some("Running daemon-origin scan"))
        .await
        .unwrap();

    let heartbeat = db.get_daemon_heartbeat().await.unwrap().unwrap();
    assert_eq!(heartbeat.phase, "scan");
    assert_eq!(heartbeat.detail.as_deref(), Some("Running daemon-origin scan"));
    assert!(!heartbeat.last_seen_at.is_empty());
}

#[tokio::test]
async fn test_get_scan_run_returns_specific_row() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    db.record_scan_run(&ScanRunRecord {
        dry_run: true,
        library_filter: Some("Anime".to_string()),
        search_missing: true,
        library_items_found: 10,
        source_items_found: 20,
        matches_found: 5,
        links_created: 1,
        links_updated: 2,
        skip_reason_json: None,
        ..Default::default()
    })
    .await
    .unwrap();
    db.record_scan_run(&ScanRunRecord {
        dry_run: false,
        library_filter: Some("Movies".to_string()),
        search_missing: false,
        library_items_found: 30,
        source_items_found: 40,
        matches_found: 15,
        links_created: 3,
        links_updated: 4,
        skip_reason_json: None,
        ..Default::default()
    })
    .await
    .unwrap();

    let latest = db.get_scan_history(1).await.unwrap();
    let run = db.get_scan_run(latest[0].id).await.unwrap().unwrap();

    assert_eq!(run.id, latest[0].id);
    assert_eq!(run.library_filter.as_deref(), Some("Movies"));
    assert_eq!(run.matches_found, 15);
    assert_eq!(run.links_created, 3);
}

#[tokio::test]
async fn database_enables_foreign_keys() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("test.db").to_str().unwrap())
        .await
        .unwrap();

    let enabled: i64 = sqlx::query("PRAGMA foreign_keys")
        .fetch_one(&db.pool)
        .await
        .unwrap()
        .get(0);
    assert_eq!(enabled, 1);
}

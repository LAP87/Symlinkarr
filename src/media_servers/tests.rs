use super::deferred::{
    load_deferred_refresh_queue, media_refresh_lock_path, media_refresh_queue_path,
    store_deferred_refresh_queue, DeferredRefreshQueue,
};
use super::*;
use crate::config::{
    ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, ContentType, DaemonConfig,
    DecypharrConfig, DmmConfig, FeaturesConfig, LibraryConfig, MatchingConfig, MediaBrowserConfig,
    PlexConfig, ProwlarrConfig, RadarrConfig, RealDebridConfig, SecurityConfig, SonarrConfig,
    SourceConfig, SymlinkConfig, TautulliConfig, WebConfig,
};
use crate::models::MediaType;
use axum::{extract::State, routing::get, Router};
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::time::{sleep, timeout, Duration};

fn test_config() -> Config {
    Config {
        libraries: vec![LibraryConfig {
            name: "Anime".to_string(),
            path: PathBuf::from("/mnt/storage/plex/anime"),
            media_type: MediaType::Tv,
            content_type: Some(ContentType::Anime),
            depth: 1,
        }],
        sources: vec![SourceConfig {
            name: "RD".to_string(),
            path: PathBuf::from("/mnt/zurg/__all__"),
            media_type: "auto".to_string(),
        }],
        api: ApiConfig::default(),
        realdebrid: RealDebridConfig::default(),
        decypharr: DecypharrConfig::default(),
        dmm: DmmConfig::default(),
        backup: BackupConfig::default(),
        db_path: "/tmp/test.sqlite".to_string(),
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

#[derive(Clone)]
struct MockPlexServerState {
    refresh_delay: Duration,
    refresh_calls: Arc<AtomicUsize>,
}

async fn mock_plex_sections() -> &'static str {
    r#"
<MediaContainer size="1">
  <Directory key="7" title="Anime">
<Location id="11" path="/mnt/storage/plex/anime" />
  </Directory>
</MediaContainer>
"#
}

async fn mock_plex_refresh(State(state): State<MockPlexServerState>) -> &'static str {
    state.refresh_calls.fetch_add(1, Ordering::SeqCst);
    sleep(state.refresh_delay).await;
    ""
}

async fn spawn_mock_plex_server(
    refresh_delay: Duration,
) -> std::io::Result<(String, Arc<AtomicUsize>)> {
    let refresh_calls = Arc::new(AtomicUsize::new(0));
    let state = MockPlexServerState {
        refresh_delay,
        refresh_calls: Arc::clone(&refresh_calls),
    };
    let app = Router::new()
        .route("/library/sections", get(mock_plex_sections))
        .route("/library/sections/{key}/refresh", get(mock_plex_refresh))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Ok((format!("http://{addr}"), refresh_calls))
}

#[test]
fn selected_library_root_paths_dedupes_and_sorts() {
    let movie = LibraryConfig {
        name: "Movies".to_string(),
        path: PathBuf::from("/mnt/storage/plex/movies"),
        media_type: MediaType::Movie,
        content_type: Some(ContentType::Movie),
        depth: 1,
    };
    let anime = LibraryConfig {
        name: "Anime".to_string(),
        path: PathBuf::from("/mnt/storage/plex/anime"),
        media_type: MediaType::Tv,
        content_type: Some(ContentType::Anime),
        depth: 1,
    };

    let roots = selected_library_root_paths(&[&movie, &anime, &movie]);
    assert_eq!(
        roots,
        vec![
            PathBuf::from("/mnt/storage/plex/anime"),
            PathBuf::from("/mnt/storage/plex/movies"),
        ]
    );
}

#[test]
fn refresh_root_paths_for_affected_paths_prefers_longest_matching_root() {
    let root = LibraryConfig {
        name: "Root".to_string(),
        path: PathBuf::from("/mnt/storage/plex"),
        media_type: MediaType::Tv,
        content_type: Some(ContentType::Tv),
        depth: 1,
    };
    let anime = LibraryConfig {
        name: "Anime".to_string(),
        path: PathBuf::from("/mnt/storage/plex/anime"),
        media_type: MediaType::Tv,
        content_type: Some(ContentType::Anime),
        depth: 1,
    };

    let roots = refresh_root_paths_for_affected_paths(
        &[&root, &anime],
        &[
            PathBuf::from("/mnt/storage/plex/anime/Show/Season 01/E01.mkv"),
            PathBuf::from("/mnt/storage/plex/serier/Show/Season 01/E01.mkv"),
        ],
    );

    assert_eq!(
        roots,
        vec![
            PathBuf::from("/mnt/storage/plex"),
            PathBuf::from("/mnt/storage/plex/anime"),
        ]
    );
}

#[test]
fn configured_refresh_backends_returns_empty_without_backend() {
    let cfg = test_config();
    assert!(configured_refresh_backends(&cfg).is_empty());
    assert!(!has_configured_invalidation_server(&cfg));
}

#[test]
fn configured_refresh_backends_supports_multiple_backends() {
    let mut cfg = test_config();
    cfg.plex.url = "http://localhost:32400".to_string();
    cfg.plex.token = "plex-token".to_string();
    cfg.emby.url = "http://localhost:8096".to_string();
    cfg.emby.api_key = "emby-key".to_string();

    assert_eq!(
        configured_refresh_backends(&cfg),
        vec![MediaServerKind::Plex, MediaServerKind::Emby]
    );
    assert!(has_configured_invalidation_server(&cfg));
}

#[test]
fn media_browser_target_plan_falls_back_to_library_roots_when_cap_would_abort() {
    let mut cfg = test_config();
    cfg.emby.url = "http://localhost:8096".to_string();
    cfg.emby.api_key = "emby-key".to_string();
    cfg.emby.refresh_batch_size = 2;
    cfg.emby.max_refresh_batches_per_run = 1;
    cfg.emby.abort_refresh_when_capped = true;
    cfg.emby.fallback_to_library_roots_when_capped = true;

    let plan = refresh_targets_for_server(
        &cfg,
        MediaServerKind::Emby,
        &[PathBuf::from("/mnt/storage/plex/anime")],
        &[
            PathBuf::from("/mnt/storage/plex/anime/Show 1/Season 01/E01.mkv"),
            PathBuf::from("/mnt/storage/plex/anime/Show 2/Season 01/E01.mkv"),
            PathBuf::from("/mnt/storage/plex/anime/Show 3/Season 01/E01.mkv"),
        ],
    );

    assert_eq!(plan.targets, vec![PathBuf::from("/mnt/storage/plex/anime")]);
    assert!(plan.root_fallback_applied);
    assert_eq!(plan.coalesced_paths, 2);
    assert_eq!(plan.coalesced_batches, 1);
}

#[test]
fn media_browser_target_plan_keeps_targeted_paths_when_fallback_disabled() {
    let mut cfg = test_config();
    cfg.jellyfin.url = "http://localhost:8097".to_string();
    cfg.jellyfin.api_key = "jellyfin-key".to_string();
    cfg.jellyfin.refresh_batch_size = 2;
    cfg.jellyfin.max_refresh_batches_per_run = 1;
    cfg.jellyfin.abort_refresh_when_capped = true;
    cfg.jellyfin.fallback_to_library_roots_when_capped = false;

    let plan = refresh_targets_for_server(
        &cfg,
        MediaServerKind::Jellyfin,
        &[PathBuf::from("/mnt/storage/plex/anime")],
        &[
            PathBuf::from("/mnt/storage/plex/anime/Show 1/Season 01/E01.mkv"),
            PathBuf::from("/mnt/storage/plex/anime/Show 2/Season 01/E01.mkv"),
            PathBuf::from("/mnt/storage/plex/anime/Show 3/Season 01/E01.mkv"),
        ],
    );

    assert_eq!(plan.targets.len(), 3);
    assert!(!plan.root_fallback_applied);
    assert_eq!(plan.coalesced_paths, 0);
    assert_eq!(plan.coalesced_batches, 0);
}

#[test]
fn media_browser_target_plan_skips_fallback_when_roots_still_exceed_cap() {
    let mut cfg = test_config();
    cfg.emby.url = "http://localhost:8096".to_string();
    cfg.emby.api_key = "emby-key".to_string();
    cfg.emby.refresh_batch_size = 1;
    cfg.emby.max_refresh_batches_per_run = 1;
    cfg.emby.abort_refresh_when_capped = true;
    cfg.emby.fallback_to_library_roots_when_capped = true;

    let plan = refresh_targets_for_server(
        &cfg,
        MediaServerKind::Emby,
        &[
            PathBuf::from("/mnt/storage/plex/anime"),
            PathBuf::from("/mnt/storage/plex/series"),
        ],
        &[
            PathBuf::from("/mnt/storage/plex/anime/Show 1/Season 01/E01.mkv"),
            PathBuf::from("/mnt/storage/plex/series/Show 2/Season 01/E01.mkv"),
            PathBuf::from("/mnt/storage/plex/series/Show 3/Season 01/E01.mkv"),
        ],
    );

    assert_eq!(plan.targets.len(), 3);
    assert!(!plan.root_fallback_applied);
}

#[tokio::test]
async fn invalidate_after_mutation_reports_unconfigured_when_no_backend_exists() {
    let cfg = test_config();
    let library = &cfg.libraries[0];
    let outcome = invalidate_after_mutation(
        &cfg,
        &[library],
        &[PathBuf::from(
            "/mnt/storage/plex/anime/Show/Season 01/E01.mkv",
        )],
        false,
    )
    .await
    .unwrap();

    assert_eq!(outcome.server, None);
    assert!(!outcome.configured);
    assert_eq!(outcome.requested_library_roots, 1);
    assert!(outcome.refresh.is_none());
    assert!(outcome.servers.is_empty());
}

#[test]
fn summary_suffix_mentions_multiple_backends_when_present() {
    let outcome = LibraryInvalidationOutcome {
        server: None,
        requested_library_roots: 2,
        configured: true,
        refresh: Some(LibraryRefreshTelemetry {
            refreshed_batches: 3,
            ..LibraryRefreshTelemetry::default()
        }),
        servers: vec![
            LibraryInvalidationServerOutcome {
                server: MediaServerKind::Plex,
                requested_targets: 2,
                refresh: LibraryRefreshTelemetry {
                    refreshed_batches: 1,
                    ..LibraryRefreshTelemetry::default()
                },
            },
            LibraryInvalidationServerOutcome {
                server: MediaServerKind::Emby,
                requested_targets: 4,
                refresh: LibraryRefreshTelemetry {
                    refreshed_batches: 2,
                    ..LibraryRefreshTelemetry::default()
                },
            },
        ],
    };

    let summary = outcome.summary_suffix().unwrap();
    assert!(summary.contains("Plex, Emby"));
    assert!(summary.contains("across 2 server(s)"));
}

#[test]
fn summary_suffix_mentions_lock_deferral_when_present() {
    let outcome = LibraryInvalidationOutcome {
        server: None,
        requested_library_roots: 2,
        configured: true,
        refresh: Some(LibraryRefreshTelemetry {
            deferred_due_to_lock: true,
            ..LibraryRefreshTelemetry::default()
        }),
        servers: vec![LibraryInvalidationServerOutcome {
            server: MediaServerKind::Plex,
            requested_targets: 2,
            refresh: LibraryRefreshTelemetry {
                requested_paths: 2,
                deferred_due_to_lock: true,
                ..LibraryRefreshTelemetry::default()
            },
        }],
    };

    let summary = outcome.summary_suffix().unwrap();
    assert!(summary.contains("deferred"));
    assert!(summary.contains("already refreshing"));
}

#[tokio::test]
async fn refresh_library_paths_detailed_defers_when_lock_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.plex.url = "http://localhost:32400".to_string();
    cfg.plex.token = "plex-token".to_string();
    cfg.backup.path = dir.path().join("backups");
    std::fs::create_dir_all(&cfg.backup.path).unwrap();

    let lock_path = media_refresh_lock_path(&cfg);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(rc, 0);

    let outcome = refresh_library_paths_detailed(
        &cfg,
        &[PathBuf::from(
            "/mnt/storage/plex/anime/Show/Season 01/E01.mkv",
        )],
        false,
    )
    .await
    .unwrap();

    assert!(outcome.aggregate.deferred_due_to_lock);
    assert_eq!(outcome.servers.len(), 1);
    assert!(outcome.servers[0].refresh.deferred_due_to_lock);
    assert!(media_refresh_queue_path(&cfg).exists());
}

#[tokio::test]
async fn refresh_library_paths_detailed_reports_only_backends_with_pending_targets_on_lock() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.plex.url = "http://localhost:32400".to_string();
    cfg.plex.token = "plex-token".to_string();
    cfg.emby.url = "http://localhost:8096".to_string();
    cfg.emby.api_key = "emby-token".to_string();
    cfg.backup.path = dir.path().join("backups");
    std::fs::create_dir_all(&cfg.backup.path).unwrap();

    queue_deferred_refresh_targets(
        &cfg,
        &[(
            MediaServerKind::Emby,
            vec![PathBuf::from(
                "/mnt/storage/plex/anime/Show/Season 01/E01.mkv",
            )],
        )],
    )
    .unwrap();

    let lock_path = media_refresh_lock_path(&cfg);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(rc, 0);

    let outcome = refresh_library_paths_detailed(&cfg, &[], false)
        .await
        .unwrap();
    assert!(outcome.aggregate.deferred_due_to_lock);
    assert_eq!(outcome.servers.len(), 1);
    assert_eq!(outcome.servers[0].server, MediaServerKind::Emby);
    assert_eq!(outcome.servers[0].requested_targets, 1);
    assert_eq!(outcome.servers[0].refresh.requested_paths, 1);
}

#[test]
fn try_acquire_media_refresh_guard_creates_missing_parent_directory() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.backup.path = dir.path().join("nested/backups");

    let guard = try_acquire_media_refresh_guard(&cfg).unwrap();
    assert!(guard.is_some());
    assert!(media_refresh_lock_path(&cfg).exists());
}

#[test]
fn deferred_refresh_queue_roundtrip_dedupes_per_server() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.backup.path = dir.path().join("backups");
    std::fs::create_dir_all(&cfg.backup.path).unwrap();

    queue_deferred_refresh_targets(
        &cfg,
        &[
            (
                MediaServerKind::Emby,
                vec![
                    PathBuf::from("/mnt/storage/plex/anime"),
                    PathBuf::from("/mnt/storage/plex/anime"),
                ],
            ),
            (
                MediaServerKind::Plex,
                vec![PathBuf::from("/mnt/storage/plex/series")],
            ),
        ],
    )
    .unwrap();
    queue_deferred_refresh_targets(
        &cfg,
        &[(
            MediaServerKind::Emby,
            vec![PathBuf::from("/mnt/storage/plex/movies")],
        )],
    )
    .unwrap();

    let queue = load_deferred_refresh_queue(&cfg).unwrap();
    assert_eq!(queue.servers.len(), 2);
    assert_eq!(
        queue.servers[0],
        DeferredRefreshQueueServer {
            server: MediaServerKind::Emby,
            paths: vec![
                PathBuf::from("/mnt/storage/plex/anime"),
                PathBuf::from("/mnt/storage/plex/movies")
            ]
        }
    );
    assert_eq!(
        queue.servers[1],
        DeferredRefreshQueueServer {
            server: MediaServerKind::Plex,
            paths: vec![PathBuf::from("/mnt/storage/plex/series")]
        }
    );
    store_deferred_refresh_queue(&cfg, &DeferredRefreshQueue::default()).unwrap();
    assert!(!media_refresh_queue_path(&cfg).exists());
}

#[tokio::test]
async fn refresh_library_paths_detailed_requeues_failed_targets() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    cfg.plex.url = "http://127.0.0.1:9".to_string();
    cfg.plex.token = "plex-token".to_string();
    cfg.backup.path = dir.path().join("backups");
    std::fs::create_dir_all(&cfg.backup.path).unwrap();

    let deferred = PathBuf::from("/mnt/storage/plex/anime/Queued/Season 01/E01.mkv");
    let new_target = PathBuf::from("/mnt/storage/plex/anime/New/Season 01/E02.mkv");
    queue_deferred_refresh_targets(&cfg, &[(MediaServerKind::Plex, vec![deferred.clone()])])
        .unwrap();

    let outcome = refresh_library_paths_detailed(&cfg, std::slice::from_ref(&new_target), false)
        .await
        .unwrap();

    assert_eq!(outcome.servers.len(), 1);
    assert_eq!(outcome.servers[0].server, MediaServerKind::Plex);
    assert_eq!(outcome.servers[0].refresh.failed_batches, 1);

    let queue = load_deferred_refresh_queue(&cfg).unwrap();
    assert_eq!(queue.servers.len(), 1);
    assert_eq!(queue.servers[0].server, MediaServerKind::Plex);
    let mut queued_paths = queue.servers[0].paths.clone();
    queued_paths.sort();
    assert_eq!(queued_paths, vec![new_target, deferred]);
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_refresh_locking_defers_without_blocking_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = test_config();
    let (plex_url, refresh_calls) = match spawn_mock_plex_server(Duration::from_millis(150)).await {
        Ok(values) => values,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!(
                "skipping concurrent refresh locking test because the sandbox denied loopback bind: {}",
                err
            );
            return;
        }
        Err(err) => panic!("failed to start mock plex server: {}", err),
    };
    cfg.plex.url = plex_url;
    cfg.plex.token = "plex-token".to_string();
    cfg.backup.path = dir.path().join("backups");
    std::fs::create_dir_all(&cfg.backup.path).unwrap();

    let refresh_targets = vec![PathBuf::from(
        "/mnt/storage/plex/anime/Show/Season 01/E01.mkv",
    )];

    let (left, right) = timeout(Duration::from_secs(2), async {
        tokio::join!(
            refresh_library_paths_detailed(&cfg, &refresh_targets, false),
            refresh_library_paths_detailed(&cfg, &refresh_targets, false)
        )
    })
    .await
    .expect("concurrent refresh probe should not deadlock");

    let outcomes = [left.unwrap(), right.unwrap()];
    assert_eq!(outcomes.len(), 2);
    assert!(
        outcomes
            .iter()
            .any(|outcome| outcome.aggregate.deferred_due_to_lock),
        "expected one concurrent caller to defer while another refreshed"
    );
    assert!(
        outcomes
            .iter()
            .any(|outcome| !outcome.aggregate.deferred_due_to_lock),
        "expected one concurrent caller to acquire the refresh lock"
    );

    let queued_before_drain = deferred_refresh_summary(&cfg).unwrap();
    assert_eq!(queued_before_drain.pending_targets, 1);

    let drain = timeout(
        Duration::from_secs(2),
        refresh_library_paths_detailed(&cfg, &[], false),
    )
    .await
    .expect("deferred refresh drain should not deadlock")
    .unwrap();
    assert!(!drain.aggregate.deferred_due_to_lock);

    let queued_after_drain = deferred_refresh_summary(&cfg).unwrap();
    assert_eq!(queued_after_drain.pending_targets, 0);
    assert_eq!(refresh_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
#[ignore = "hits real configured media servers and writes a temporary probe directory under a library root"]
async fn live_refresh_lock_probe_against_real_backends() {
    let mut cfg = Config::load(Some("config.yaml".to_string()))
        .expect("expected local config.yaml with real media server credentials");
    assert!(
        !configured_refresh_backends(&cfg).is_empty(),
        "expected at least one configured refresh backend in config.yaml"
    );

    let backup_dir = tempfile::tempdir().unwrap();
    cfg.backup.path = backup_dir.path().join("backups");
    std::fs::create_dir_all(&cfg.backup.path).unwrap();

    let library_root = cfg
        .libraries
        .iter()
        .find(|library| library.path.exists())
        .map(|library| library.path.clone())
        .expect("expected at least one existing library root in config.yaml");

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let probe_root = library_root.join(format!("__symlinkarr_rc_refresh_probe_{nonce}"));

    let probe_paths = vec![
        probe_root.join("batch-a"),
        probe_root.join("batch-b"),
        probe_root.join("batch-c"),
    ];
    for path in &probe_paths {
        std::fs::create_dir_all(path).unwrap();
    }

    cfg.plex.refresh_delay_ms = cfg.plex.refresh_delay_ms.max(300);
    cfg.emby.refresh_delay_ms = cfg.emby.refresh_delay_ms.max(300);
    cfg.jellyfin.refresh_delay_ms = cfg.jellyfin.refresh_delay_ms.max(300);
    cfg.emby.refresh_batch_size = 1;
    cfg.jellyfin.refresh_batch_size = 1;
    cfg.emby.max_refresh_batches_per_run =
        cfg.emby.max_refresh_batches_per_run.max(probe_paths.len());
    cfg.jellyfin.max_refresh_batches_per_run = cfg
        .jellyfin
        .max_refresh_batches_per_run
        .max(probe_paths.len());
    cfg.plex.max_refresh_batches_per_run =
        cfg.plex.max_refresh_batches_per_run.max(probe_paths.len());

    let worker_count = 3usize;
    let mut join_set = tokio::task::JoinSet::new();
    for _ in 0..worker_count {
        let cfg = cfg.clone();
        let probe_paths = probe_paths.clone();
        join_set.spawn(async move {
            refresh_library_paths_detailed(&cfg, &probe_paths, true)
                .await
                .expect("live refresh probe should complete")
        });
    }

    let mut outcomes = Vec::new();
    while let Some(result) = join_set.join_next().await {
        outcomes.push(result.expect("live refresh worker panicked"));
    }

    assert_eq!(outcomes.len(), worker_count);
    assert!(
        outcomes
            .iter()
            .any(|outcome| outcome.aggregate.deferred_due_to_lock),
        "expected at least one worker to defer while another held the refresh lock"
    );
    assert!(
        outcomes
            .iter()
            .any(|outcome| !outcome.aggregate.deferred_due_to_lock),
        "expected at least one worker to acquire the refresh lock"
    );

    let queued_before_drain = deferred_refresh_summary(&cfg).unwrap();
    assert!(
        queued_before_drain.pending_targets > 0,
        "expected deferred refresh targets to remain queued after lock contention"
    );

    let drain = refresh_library_paths_detailed(&cfg, &[], true)
        .await
        .expect("deferred refresh drain should complete");
    assert!(
        !drain.aggregate.deferred_due_to_lock,
        "expected deferred refresh drain to acquire the lock"
    );

    let queued_after_drain = deferred_refresh_summary(&cfg).unwrap();
    assert_eq!(
        queued_after_drain.pending_targets, 0,
        "expected deferred refresh queue to drain completely"
    );

    std::fs::remove_dir_all(&probe_root).unwrap();
}

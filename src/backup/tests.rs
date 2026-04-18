use super::*;
use crate::config::{
    ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, Config, DaemonConfig,
    DecypharrConfig, DmmConfig, FeaturesConfig, MatchingConfig, MediaBrowserConfig, PlexConfig,
    ProwlarrConfig, RadarrConfig, RealDebridConfig, SecurityConfig, SonarrConfig, SymlinkConfig,
    TautulliConfig, WebConfig,
};
use crate::db::Database;
use crate::models::LinkRecord;
use crate::utils::PathHealth;

fn test_config(dir: &Path) -> BackupConfig {
    BackupConfig {
        enabled: true,
        path: dir.to_path_buf(),
        interval_hours: 24,
        max_backups: 3,
        max_safety_backups: 0, // keep all safety snapshots by default
    }
}

fn test_runtime_config(root: &Path, backup_dir: &Path) -> Config {
    Config {
        libraries: Vec::new(),
        sources: Vec::new(),
        api: ApiConfig::default(),
        realdebrid: RealDebridConfig::default(),
        decypharr: DecypharrConfig::default(),
        dmm: DmmConfig::default(),
        backup: test_config(backup_dir),
        db_path: root.join("symlinkarr.db").display().to_string(),
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
async fn test_backup_restore_roundtrip() {
    let dir = tempfile::TempDir::new().unwrap();
    let manager = BackupManager::new(&test_config(dir.path()));
    let db_path = dir.path().join("symlinkarr.db");
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();
    let source_root = dir.path().join("rd");
    std::fs::create_dir_all(&source_root).unwrap();
    std::fs::write(source_root.join("ep1.mkv"), "video").unwrap();
    std::fs::write(source_root.join("ep2.mkv"), "video").unwrap();

    // Create a fake manifest
    let manifest = BackupManifest {
        version: 1,
        timestamp: Utc::now(),
        backup_type: BackupType::Scheduled,
        label: "test".to_string(),
        symlinks: vec![
            BackupEntry {
                symlink_path: dir.path().join("show/Season 01/ep1.mkv"),
                target_path: dir.path().join("rd/ep1.mkv"),
                media_id: "tvdb-12345".to_string(),
                media_type: MediaType::Tv,
                db_tracked: true,
            },
            BackupEntry {
                symlink_path: dir.path().join("show/Season 01/ep2.mkv"),
                target_path: dir.path().join("rd/ep2.mkv"),
                media_id: "tvdb-12345".to_string(),
                media_type: MediaType::Tv,
                db_tracked: true,
            },
        ],
        total_count: 2,
        database_snapshot: None,
        app_state: None,
        content_sha256: None,
    };

    // Write backup
    let backup_path = dir.path().join("test-backup.json");
    let json = serde_json::to_string_pretty(&manifest).unwrap();
    std::fs::write(&backup_path, json).unwrap();

    // Restore from backup (dry-run)
    let roots = vec![dir.path().to_path_buf()];
    let (restored, skipped, errors) = manager
        .restore(&db, &backup_path, true, &roots, &roots, true)
        .await
        .unwrap();
    assert_eq!(restored, 2);
    assert_eq!(skipped, 0);
    assert_eq!(errors, 0);

    // Restore for real
    let (restored, skipped, errors) = manager
        .restore(&db, &backup_path, false, &roots, &roots, true)
        .await
        .unwrap();
    assert_eq!(restored, 2);
    assert_eq!(skipped, 0);
    assert_eq!(errors, 0);

    // Verify symlinks exist
    assert!(dir.path().join("show/Season 01/ep1.mkv").is_symlink());
    assert!(dir.path().join("show/Season 01/ep2.mkv").is_symlink());

    // Restore again — should skip existing
    let (restored, skipped, errors) = manager
        .restore(&db, &backup_path, false, &roots, &roots, true)
        .await
        .unwrap();
    assert_eq!(restored, 0);
    assert_eq!(skipped, 2);
    assert_eq!(errors, 0);

    assert_eq!(db.get_stats().await.unwrap().0, 2);
}

#[tokio::test]
async fn test_create_backup_writes_database_snapshot_and_checksum() {
    let dir = tempfile::TempDir::new().unwrap();
    let manager = BackupManager::new(&test_config(dir.path()));
    let db_path = dir.path().join("symlinkarr.db");
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();
    let cfg = test_runtime_config(dir.path(), dir.path());

    let source_root = dir.path().join("rd");
    let library_root = dir.path().join("library");
    std::fs::create_dir_all(&source_root).unwrap();
    std::fs::create_dir_all(&library_root).unwrap();
    let source_file = source_root.join("ep1.mkv");
    let symlink_file = library_root.join("ep1.mkv");
    std::fs::write(&source_file, "video").unwrap();

    db.insert_link(&LinkRecord {
        id: None,
        source_path: source_file,
        target_path: symlink_file,
        media_id: "tvdb-1".to_string(),
        media_type: MediaType::Tv,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();

    let backup_path = manager
        .create_backup(&cfg, &db, "Manual backup")
        .await
        .unwrap();
    assert!(backup_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .starts_with("symlinkarr-backup-"));
    let manifest = parse_backup_manifest(
        &std::fs::read_to_string(&backup_path).unwrap(),
        &backup_path,
    )
    .unwrap();

    let snapshot = manifest.database_snapshot.as_ref().unwrap();
    let snapshot_path = dir.path().join(&snapshot.filename);
    assert!(snapshot_path.exists());
    assert_eq!(snapshot.sha256, sha256_file(&snapshot_path).unwrap());
    assert!(manifest.content_sha256.is_some());
}

#[tokio::test]
async fn test_create_backup_captures_app_state_snapshots() {
    let dir = tempfile::TempDir::new().unwrap();
    let backup_dir = dir.path().join("backups");
    std::fs::create_dir_all(&backup_dir).unwrap();
    let manager = BackupManager::new(&test_config(&backup_dir));
    let db = Database::new(dir.path().join("symlinkarr.db").to_str().unwrap())
        .await
        .unwrap();

    let config_path = dir.path().join("config.yaml");
    let secret_path = dir.path().join("rd.token");
    std::fs::write(&config_path, "db_path: ./data/symlinkarr.db\n").unwrap();
    std::fs::write(&secret_path, "super-secret-token\n").unwrap();

    let mut cfg = test_runtime_config(dir.path(), &backup_dir);
    cfg.loaded_from = Some(config_path.clone());
    cfg.secret_files = vec![secret_path.clone()];

    let backup_path = manager
        .create_backup(&cfg, &db, "Manual backup")
        .await
        .unwrap();
    let manifest = parse_backup_manifest(
        &std::fs::read_to_string(&backup_path).unwrap(),
        &backup_path,
    )
    .unwrap();

    let app_state = manifest.app_state.expect("app_state should be captured");
    let config_snapshot = app_state
        .config_snapshot
        .as_ref()
        .expect("config snapshot should be present");
    assert_eq!(app_state.secret_snapshots.len(), 1);
    assert_eq!(config_snapshot.original_path, config_path);
    assert_eq!(app_state.secret_snapshots[0].original_path, secret_path);
    assert!(backup_dir.join(&config_snapshot.filename).exists());
    assert!(backup_dir
        .join(&app_state.secret_snapshots[0].filename)
        .exists());
    assert_eq!(
        config_snapshot.sha256,
        sha256_file(&backup_dir.join(&config_snapshot.filename)).unwrap()
    );
}

#[tokio::test]
async fn test_restore_app_state_restores_current_install_paths() {
    let dir = tempfile::TempDir::new().unwrap();
    let backup_dir = dir.path().join("backups");
    std::fs::create_dir_all(&backup_dir).unwrap();
    let manager = BackupManager::new(&test_config(&backup_dir));
    let db = Database::new(dir.path().join("symlinkarr.db").to_str().unwrap())
        .await
        .unwrap();

    let config_path = dir.path().join("config.yaml");
    let secret_path = dir.path().join("rd.token");
    std::fs::write(&config_path, "original-config\n").unwrap();
    std::fs::write(&secret_path, "original-secret\n").unwrap();

    let mut cfg = test_runtime_config(dir.path(), &backup_dir);
    cfg.loaded_from = Some(config_path.clone());
    cfg.secret_files = vec![secret_path.clone()];

    let backup_path = manager
        .create_backup(&cfg, &db, "Manual backup")
        .await
        .unwrap();

    std::fs::write(&config_path, "modified-config\n").unwrap();
    std::fs::write(&secret_path, "modified-secret\n").unwrap();

    let summary = manager
        .restore_app_state(&cfg, &backup_path, false)
        .unwrap();

    assert!(summary.present);
    assert!(summary.config_restored);
    assert_eq!(summary.secrets_restored, 1);
    assert_eq!(summary.secrets_skipped, 0);
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        "original-config\n"
    );
    assert_eq!(
        std::fs::read_to_string(&secret_path).unwrap(),
        "original-secret\n"
    );
}

#[test]
fn test_resolve_restore_path_rejects_absolute_input_inside_backup_root() {
    let dir = tempfile::TempDir::new().unwrap();
    let backup_dir = dir.path().join("backups");
    std::fs::create_dir_all(&backup_dir).unwrap();
    let manager = BackupManager::new(&test_config(&backup_dir));
    let manifest_path = backup_dir.join("backup.json");
    std::fs::write(&manifest_path, "{}").unwrap();

    let err = manager.resolve_restore_path(&manifest_path).unwrap_err();
    assert!(err
        .to_string()
        .contains("only accepts files inside the configured backup directory"));
}

#[test]
fn test_resolve_restore_path_rejects_parent_segments_even_if_they_normalize_inside_backup_root() {
    let dir = tempfile::TempDir::new().unwrap();
    let backup_dir = dir.path().join("backups");
    std::fs::create_dir_all(&backup_dir).unwrap();
    let manager = BackupManager::new(&test_config(&backup_dir));
    let manifest_path = backup_dir.join("backup.json");
    std::fs::write(&manifest_path, "{}").unwrap();

    let err = manager
        .resolve_restore_path(Path::new("../backups/backup.json"))
        .unwrap_err();
    assert!(err
        .to_string()
        .contains("escapes the configured backup directory"));
}

#[tokio::test]
async fn test_restore_backfills_database_for_existing_symlink() {
    let dir = tempfile::TempDir::new().unwrap();
    let manager = BackupManager::new(&test_config(dir.path()));
    let db_path = dir.path().join("symlinkarr.db");
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    let symlink_path = dir.path().join("show/Season 01/ep1.mkv");
    let target_path = dir.path().join("rd/ep1.mkv");
    std::fs::create_dir_all(symlink_path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(target_path.parent().unwrap()).unwrap();
    std::fs::write(&target_path, "video").unwrap();
    std::os::unix::fs::symlink(&target_path, &symlink_path).unwrap();

    let manifest = BackupManifest {
        version: 1,
        timestamp: Utc::now(),
        backup_type: BackupType::Scheduled,
        label: "existing symlink".to_string(),
        symlinks: vec![BackupEntry {
            symlink_path: symlink_path.clone(),
            target_path: target_path.clone(),
            media_id: "tvdb-1".to_string(),
            media_type: MediaType::Tv,
            db_tracked: true,
        }],
        total_count: 1,
        database_snapshot: None,
        app_state: None,
        content_sha256: None,
    };
    let backup_path = dir.path().join("existing-symlink.json");
    std::fs::write(
        &backup_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let roots = vec![dir.path().to_path_buf()];
    let (restored, skipped, errors) = manager
        .restore(&db, &backup_path, false, &roots, &roots, true)
        .await
        .unwrap();

    assert_eq!(restored, 1);
    assert_eq!(skipped, 0);
    assert_eq!(errors, 0);
    assert!(db
        .get_link_by_target_path(&symlink_path)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn test_create_safety_snapshot_with_extras_captures_disk_only_symlink() {
    let dir = tempfile::TempDir::new().unwrap();
    let backup_dir = dir.path().join("backups");
    std::fs::create_dir_all(&backup_dir).unwrap();
    let manager = BackupManager::new(&test_config(&backup_dir));
    let db_path = dir.path().join("symlinkarr.db");
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    let source_dir = dir.path().join("rd");
    let library_dir = dir.path().join("library");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::create_dir_all(&library_dir).unwrap();
    let source_file = source_dir.join("episode.mkv");
    std::fs::write(&source_file, "video").unwrap();
    let disk_only_symlink = library_dir.join("legacy.mkv");
    std::os::unix::fs::symlink(&source_file, &disk_only_symlink).unwrap();

    let snapshot_path = manager
        .create_safety_snapshot_with_extras(
            &db,
            "cleanup-prune",
            std::slice::from_ref(&disk_only_symlink),
        )
        .await
        .unwrap();

    let manifest: BackupManifest =
        serde_json::from_str(&std::fs::read_to_string(&snapshot_path).unwrap()).unwrap();
    assert_eq!(manifest.total_count, 1);
    assert_eq!(manifest.symlinks[0].symlink_path, disk_only_symlink);
    assert_eq!(manifest.symlinks[0].target_path, source_file);
    assert!(!manifest.symlinks[0].db_tracked);
}

#[tokio::test]
async fn test_restore_disk_only_backup_entry_skips_db_backfill() {
    let dir = tempfile::TempDir::new().unwrap();
    let manager = BackupManager::new(&test_config(dir.path()));
    let db_path = dir.path().join("symlinkarr.db");
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    let symlink_path = dir.path().join("show/Season 01/legacy.mkv");
    let target_path = dir.path().join("rd/legacy.mkv");
    std::fs::create_dir_all(symlink_path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(target_path.parent().unwrap()).unwrap();
    std::fs::write(&target_path, "video").unwrap();

    let manifest = BackupManifest {
        version: 1,
        timestamp: Utc::now(),
        backup_type: BackupType::Safety {
            operation: "cleanup-prune".to_string(),
        },
        label: "disk only".to_string(),
        symlinks: vec![BackupEntry {
            symlink_path: symlink_path.clone(),
            target_path: target_path.clone(),
            media_id: String::new(),
            media_type: MediaType::Tv,
            db_tracked: false,
        }],
        total_count: 1,
        database_snapshot: None,
        app_state: None,
        content_sha256: None,
    };
    let backup_path = dir.path().join("disk-only-backup.json");
    std::fs::write(
        &backup_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let roots = vec![dir.path().to_path_buf()];
    let (restored, skipped, errors) = manager
        .restore(&db, &backup_path, false, &roots, &roots, true)
        .await
        .unwrap();

    assert_eq!(restored, 1);
    assert_eq!(skipped, 0);
    assert_eq!(errors, 0);
    assert!(symlink_path.is_symlink());
    assert!(db
        .get_link_by_target_path(&symlink_path)
        .await
        .unwrap()
        .is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn test_restore_disk_only_relative_target_passes_root_validation() {
    let dir = tempfile::TempDir::new().unwrap();
    let manager = BackupManager::new(&test_config(dir.path()));
    let db_path = dir.path().join("symlinkarr.db");
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    let symlink_path = dir.path().join("library/show/Season 01/legacy.mkv");
    let target_path = dir.path().join("rd/legacy.mkv");
    std::fs::create_dir_all(symlink_path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(target_path.parent().unwrap()).unwrap();
    std::fs::write(&target_path, "video").unwrap();

    let relative_target = PathBuf::from("../../../rd/legacy.mkv");
    let manifest = BackupManifest {
        version: 1,
        timestamp: Utc::now(),
        backup_type: BackupType::Safety {
            operation: "cleanup-prune".to_string(),
        },
        label: "disk only relative".to_string(),
        symlinks: vec![BackupEntry {
            symlink_path: symlink_path.clone(),
            target_path: relative_target.clone(),
            media_id: String::new(),
            media_type: MediaType::Tv,
            db_tracked: false,
        }],
        total_count: 1,
        database_snapshot: None,
        app_state: None,
        content_sha256: None,
    };
    let backup_path = dir.path().join("disk-only-relative-backup.json");
    std::fs::write(
        &backup_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let library_roots = vec![dir.path().join("library")];
    let source_roots = vec![dir.path().join("rd")];
    let (restored, skipped, errors) = manager
        .restore(
            &db,
            &backup_path,
            false,
            &library_roots,
            &source_roots,
            true,
        )
        .await
        .unwrap();

    assert_eq!(restored, 1);
    assert_eq!(skipped, 0);
    assert_eq!(errors, 0);
    assert!(symlink_path.is_symlink());
    assert_eq!(std::fs::read_link(&symlink_path).unwrap(), relative_target);
}

#[test]
fn test_rotation_keeps_max_backups() {
    let dir = tempfile::TempDir::new().unwrap();
    let config = test_config(dir.path());
    let manager = BackupManager::new(&config);

    // Create 5 scheduled backups (max is 3)
    for i in 0..5 {
        let filename = format!("symlinkarr-backup-2026010{}-120000.json", i);
        let manifest = BackupManifest {
            version: 1,
            timestamp: Utc::now(),
            backup_type: BackupType::Scheduled,
            label: format!("test {}", i),
            symlinks: vec![],
            total_count: 0,
            database_snapshot: None,
            app_state: None,
            content_sha256: None,
        };
        let json = serde_json::to_string_pretty(&manifest).unwrap();
        std::fs::write(dir.path().join(filename), json).unwrap();
    }

    // Also create a safety snapshot (should NOT be rotated)
    let safety_manifest = BackupManifest {
        version: 1,
        timestamp: Utc::now(),
        backup_type: BackupType::Safety {
            operation: "repair".to_string(),
        },
        label: "safety test".to_string(),
        symlinks: vec![],
        total_count: 0,
        database_snapshot: None,
        app_state: None,
        content_sha256: None,
    };
    let json = serde_json::to_string_pretty(&safety_manifest).unwrap();
    std::fs::write(
        dir.path()
            .join("symlinkarr-restore-point-repair-20260101-120000.json"),
        json,
    )
    .unwrap();

    // Rotate
    manager.rotate().unwrap();

    // Count remaining scheduled backups
    let remaining: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| n.starts_with("backup-") || n.starts_with("symlinkarr-backup-"))
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(remaining.len(), 3);

    // Safety snapshot should still exist
    assert!(dir
        .path()
        .join("symlinkarr-restore-point-repair-20260101-120000.json")
        .exists());
}

#[test]
fn test_rotation_removes_snapshot_alongside_rotated_manifest() {
    let dir = tempfile::TempDir::new().unwrap();
    let config = test_config(dir.path());
    let manager = BackupManager::new(&config);

    for i in 0..5 {
        let timestamp = format!("2026010{}-120000", i);
        let manifest_name = format!("symlinkarr-backup-{timestamp}.json");
        let snapshot_name = format!("symlinkarr-backup-{timestamp}.sqlite3");
        let manifest = BackupManifest {
            version: BACKUP_MANIFEST_VERSION,
            timestamp: Utc::now(),
            backup_type: BackupType::Scheduled,
            label: format!("test {}", i),
            symlinks: vec![],
            total_count: 0,
            database_snapshot: Some(BackupDatabaseSnapshot {
                filename: snapshot_name.clone(),
                sha256: "deadbeef".to_string(),
                size_bytes: 4,
            }),
            app_state: None,
            content_sha256: Some("checksum".to_string()),
        };
        std::fs::write(
            dir.path().join(&manifest_name),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.path().join(snapshot_name), "db").unwrap();
    }

    manager.rotate().unwrap();

    let remaining_manifests: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().map(|ext| ext == "json").unwrap_or(false))
        .collect();
    let remaining_snapshots: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .map(|ext| ext == "sqlite3")
                .unwrap_or(false)
        })
        .collect();

    assert_eq!(remaining_manifests.len(), 3);
    assert_eq!(remaining_snapshots.len(), 3);

    let remaining_stems = remaining_manifests
        .iter()
        .filter_map(|path| path.file_stem().map(|stem| stem.to_os_string()))
        .collect::<Vec<_>>();
    for snapshot in remaining_snapshots {
        let stem = snapshot.file_stem().unwrap().to_os_string();
        assert!(
            remaining_stems.contains(&stem),
            "snapshot {:?} should not survive without its manifest",
            snapshot
        );
    }
}

#[test]
fn test_list_backups() {
    let dir = tempfile::TempDir::new().unwrap();
    let manager = BackupManager::new(&test_config(dir.path()));

    // Create a test backup
    let manifest = BackupManifest {
        version: 1,
        timestamp: Utc::now(),
        backup_type: BackupType::Scheduled,
        label: "full backup".to_string(),
        symlinks: vec![BackupEntry {
            symlink_path: PathBuf::from("/plex/tv/show/ep.mkv"),
            target_path: PathBuf::from("/mnt/rd/ep.mkv"),
            media_id: "tvdb-1".to_string(),
            media_type: MediaType::Tv,
            db_tracked: true,
        }],
        total_count: 1,
        database_snapshot: None,
        app_state: None,
        content_sha256: None,
    };
    let json = serde_json::to_string_pretty(&manifest).unwrap();
    std::fs::write(
        dir.path().join("symlinkarr-backup-20260212-120000.json"),
        json,
    )
    .unwrap();

    let list = manager.list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].symlink_count, 1);
    assert_eq!(list[0].backup_type, BackupType::Scheduled);
}

#[test]
fn test_path_is_within_roots_rejects_parent_escape() {
    let dir = tempfile::TempDir::new().unwrap();
    let allowed = dir.path().join("allowed");
    std::fs::create_dir_all(&allowed).unwrap();

    let escaped = allowed.join("..").join("escaped").join("file.mkv");
    assert!(!path_is_within_roots(&escaped, &[allowed], true));
}

#[cfg(unix)]
#[test]
fn test_path_is_within_roots_rejects_symlink_escape() {
    let dir = tempfile::TempDir::new().unwrap();
    let allowed = dir.path().join("allowed");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&allowed).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    let outside_file = outside.join("file.mkv");
    std::fs::write(&outside_file, "video").unwrap();

    let alias_dir = allowed.join("alias");
    std::os::unix::fs::symlink(&outside, &alias_dir).unwrap();

    assert!(!path_is_within_roots(
        &alias_dir.join("file.mkv"),
        &[allowed],
        true
    ));
}

#[cfg(unix)]
#[test]
fn test_path_is_within_roots_accepts_nested_paths() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let nested = root.join("a/b/c");
    std::fs::create_dir_all(&nested).unwrap();

    assert!(path_is_within_roots(&nested, &[root.to_path_buf()], true));
    assert!(path_is_within_roots(
        &nested.join("file.mkv"),
        &[root.to_path_buf()],
        true
    ));
}

#[cfg(unix)]
#[test]
fn test_path_is_within_roots_accepts_existing_library_symlink_without_following_leaf() {
    let dir = tempfile::TempDir::new().unwrap();
    let library_root = dir.path().join("library");
    let source_root = dir.path().join("source");
    std::fs::create_dir_all(library_root.join("Show")).unwrap();
    std::fs::create_dir_all(&source_root).unwrap();

    let source_file = source_root.join("episode.mkv");
    std::fs::write(&source_file, "video").unwrap();

    let symlink_path = library_root.join("Show").join("S01E01.mkv");
    std::os::unix::fs::symlink(&source_file, &symlink_path).unwrap();

    assert!(path_is_within_roots(&symlink_path, &[library_root], false));
}

#[cfg(unix)]
#[test]
fn test_path_is_within_roots_rejects_leaf_symlink_escape_when_following_leaf() {
    let dir = tempfile::TempDir::new().unwrap();
    let source_root = dir.path().join("source");
    let outside_root = dir.path().join("outside");
    std::fs::create_dir_all(&source_root).unwrap();
    std::fs::create_dir_all(&outside_root).unwrap();

    let outside_file = outside_root.join("episode.mkv");
    std::fs::write(&outside_file, "video").unwrap();

    let alias = source_root.join("alias.mkv");
    std::os::unix::fs::symlink(&outside_file, &alias).unwrap();

    assert!(!path_is_within_roots(&alias, &[source_root], true));
}

#[test]
fn test_restore_target_available_rejects_unhealthy_parent() {
    let path = PathBuf::from("/mnt/rd/file.mkv");
    let parent = path.parent().unwrap().to_path_buf();
    let mut source_cache = std::collections::HashMap::new();
    let mut parent_cache = std::collections::HashMap::new();
    parent_cache.insert(parent, PathHealth::TransportDisconnected);

    let err = restore_target_available(&path, &mut source_cache, &mut parent_cache).unwrap_err();

    assert!(err.to_string().contains("Aborting backup restore"));
}

#[cfg(unix)]
#[test]
fn test_path_is_within_roots_rejects_empty_roots() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("file.mkv");
    std::fs::write(&file, "video").unwrap();
    assert!(!path_is_within_roots(&file, &[], true));
}

#[cfg(unix)]
#[tokio::test]
async fn test_restore_skips_symlink_path_escape_even_with_lexical_prefix_match() {
    let dir = tempfile::TempDir::new().unwrap();
    let backup_dir = dir.path().join("backups");
    let library_root = dir.path().join("library");
    let source_root = dir.path().join("sources");
    std::fs::create_dir_all(&backup_dir).unwrap();
    std::fs::create_dir_all(&library_root).unwrap();
    std::fs::create_dir_all(&source_root).unwrap();
    let db_path = dir.path().join("symlinkarr.db");
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    let manager = BackupManager::new(&BackupConfig {
        enabled: true,
        path: backup_dir.clone(),
        interval_hours: 24,
        max_backups: 3,
        max_safety_backups: 3,
    });

    let manifest = BackupManifest {
        version: 1,
        timestamp: Utc::now(),
        backup_type: BackupType::Scheduled,
        label: "escape".to_string(),
        symlinks: vec![BackupEntry {
            symlink_path: library_root.join("..").join("escaped").join("outside.mkv"),
            target_path: source_root.join("video.mkv"),
            media_id: "tmdb-123".to_string(),
            media_type: MediaType::Movie,
            db_tracked: true,
        }],
        total_count: 1,
        database_snapshot: None,
        app_state: None,
        content_sha256: None,
    };

    std::fs::write(source_root.join("video.mkv"), "video").unwrap();

    let backup_path = backup_dir.join("backup.json");
    std::fs::write(
        &backup_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let (restored, skipped, errors) = manager
        .restore(
            &db,
            &backup_path,
            false,
            std::slice::from_ref(&library_root),
            std::slice::from_ref(&source_root),
            true,
        )
        .await
        .unwrap();

    assert_eq!(restored, 0);
    assert_eq!(skipped, 1);
    assert_eq!(errors, 0);
    assert!(!dir.path().join("escaped").join("outside.mkv").exists());
}

#[tokio::test]
async fn test_restore_skips_missing_target_path() {
    let dir = tempfile::TempDir::new().unwrap();
    let manager = BackupManager::new(&test_config(dir.path()));
    let db_path = dir.path().join("symlinkarr.db");
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    let library_root = dir.path().join("library");
    let source_root = dir.path().join("rd");
    std::fs::create_dir_all(&library_root).unwrap();
    std::fs::create_dir_all(&source_root).unwrap();

    let manifest = BackupManifest {
        version: 1,
        timestamp: Utc::now(),
        backup_type: BackupType::Scheduled,
        label: "missing target".to_string(),
        symlinks: vec![BackupEntry {
            symlink_path: library_root.join("Show/Season 01/S01E01.mkv"),
            target_path: source_root.join("missing.mkv"),
            media_id: "tvdb-12345".to_string(),
            media_type: MediaType::Tv,
            db_tracked: true,
        }],
        total_count: 1,
        database_snapshot: None,
        app_state: None,
        content_sha256: None,
    };

    let backup_path = dir.path().join("missing-target.json");
    std::fs::write(
        &backup_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let roots = vec![dir.path().to_path_buf()];
    let source_roots = vec![source_root];
    let (restored, skipped, errors) = manager
        .restore(&db, &backup_path, false, &roots, &source_roots, true)
        .await
        .unwrap();

    assert_eq!(restored, 0);
    assert_eq!(skipped, 1);
    assert_eq!(errors, 0);
    assert!(!manifest.symlinks[0].symlink_path.exists());
    assert!(db
        .get_link_by_target_path(&manifest.symlinks[0].symlink_path)
        .await
        .unwrap()
        .is_none());
}

#[test]
fn test_list_skips_unsupported_backup_manifest_version() {
    let dir = tempfile::TempDir::new().unwrap();
    let manager = BackupManager::new(&test_config(dir.path()));

    let manifest = BackupManifest {
        version: BACKUP_MANIFEST_VERSION + 1,
        timestamp: Utc::now(),
        backup_type: BackupType::Scheduled,
        label: "future backup".to_string(),
        symlinks: vec![],
        total_count: 0,
        database_snapshot: None,
        app_state: None,
        content_sha256: None,
    };
    std::fs::write(
        dir.path().join("backup-future.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let list = manager.list().unwrap();
    assert!(list.is_empty());
}

#[tokio::test]
async fn test_restore_rejects_unsupported_backup_manifest_version() {
    let dir = tempfile::TempDir::new().unwrap();
    let manager = BackupManager::new(&test_config(dir.path()));
    let db = Database::new(dir.path().join("symlinkarr.db").to_str().unwrap())
        .await
        .unwrap();

    let backup_path = dir.path().join("future-backup.json");
    let manifest = BackupManifest {
        version: BACKUP_MANIFEST_VERSION + 1,
        timestamp: Utc::now(),
        backup_type: BackupType::Scheduled,
        label: "future backup".to_string(),
        symlinks: vec![],
        total_count: 0,
        database_snapshot: None,
        app_state: None,
        content_sha256: None,
    };
    std::fs::write(
        &backup_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let err = manager
        .restore(&db, &backup_path, false, &[], &[], false)
        .await
        .unwrap_err();

    assert!(err
        .to_string()
        .contains("Unsupported backup manifest version"));
}

#[tokio::test]
async fn test_restore_rejects_manifest_checksum_mismatch() {
    let dir = tempfile::TempDir::new().unwrap();
    let manager = BackupManager::new(&test_config(dir.path()));
    let db = Database::new(dir.path().join("symlinkarr.db").to_str().unwrap())
        .await
        .unwrap();

    let backup_path = dir.path().join("tampered-backup.json");
    let mut manifest = BackupManifest {
        version: BACKUP_MANIFEST_VERSION,
        timestamp: Utc::now(),
        backup_type: BackupType::Scheduled,
        label: "tampered backup".to_string(),
        symlinks: vec![],
        total_count: 0,
        database_snapshot: None,
        app_state: None,
        content_sha256: None,
    };
    manifest.content_sha256 = Some(compute_manifest_checksum(&manifest).unwrap());
    manifest.label = "tampered after checksum".to_string();
    std::fs::write(
        &backup_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let err = manager
        .restore(&db, &backup_path, false, &[], &[], false)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("integrity check failed"));
}

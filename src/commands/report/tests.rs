use super::*;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use crate::config::{
    ApiConfig, BackupConfig, BazarrConfig, CleanupPolicyConfig, ContentType, DaemonConfig,
    DecypharrConfig, DmmConfig, FeaturesConfig, MatchingConfig, MediaBrowserConfig, PlexConfig,
    ProwlarrConfig, RadarrConfig, RealDebridConfig, SecurityConfig, SonarrConfig, SourceConfig,
    SymlinkConfig, TautulliConfig, WebConfig,
};
use crate::db::Database;
use crate::models::{LinkRecord, LinkStatus, MediaType};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

// === Unit tests for pure helper functions ===

#[test]
fn test_media_type_key_movie() {
    assert_eq!(media_type_key(MediaType::Movie), "movie");
}

#[test]
fn test_media_type_key_tv() {
    assert_eq!(media_type_key(MediaType::Tv), "series");
}

#[test]
fn test_sample_difference_left_only() {
    let left: HashSet<PathBuf> = vec![
        PathBuf::from("/a"),
        PathBuf::from("/b"),
        PathBuf::from("/c"),
    ]
    .into_iter()
    .collect();
    let right: HashSet<PathBuf> = vec![PathBuf::from("/b")].into_iter().collect();
    let result = sample_difference(&left, &right);
    assert_eq!(result.count, 2);
    assert!(result.samples.contains(&PathBuf::from("/a")));
    assert!(result.samples.contains(&PathBuf::from("/c")));
}

#[test]
fn test_sample_difference_empty() {
    let left: HashSet<PathBuf> = HashSet::new();
    let right: HashSet<PathBuf> = HashSet::new();
    let result = sample_difference(&left, &right);
    assert_eq!(result.count, 0);
    assert!(result.samples.is_empty());
}

#[test]
fn test_sample_difference_identical() {
    let set: HashSet<PathBuf> = vec![PathBuf::from("/x")].into_iter().collect();
    let result = sample_difference(&set, &set);
    assert_eq!(result.count, 0);
    assert!(result.samples.is_empty());
}

#[test]
fn test_sample_intersection() {
    let left: HashSet<PathBuf> = vec![PathBuf::from("/a"), PathBuf::from("/b")]
        .into_iter()
        .collect();
    let right: HashSet<PathBuf> = vec![PathBuf::from("/b"), PathBuf::from("/c")]
        .into_iter()
        .collect();
    let result = sample_intersection(&left, &right);
    assert_eq!(result.count, 1);
    assert_eq!(result.samples, vec![PathBuf::from("/b")]);
}

#[test]
fn test_sample_intersection_empty() {
    let left: HashSet<PathBuf> = vec![PathBuf::from("/a")].into_iter().collect();
    let right: HashSet<PathBuf> = vec![PathBuf::from("/b")].into_iter().collect();
    let result = sample_intersection(&left, &right);
    assert_eq!(result.count, 0);
    assert!(result.samples.is_empty());
}

#[test]
fn test_collect_anime_root_duplicates_detects_legacy_and_tagged_pairs() {
    let dir = tempfile::TempDir::new().unwrap();
    let anime = dir.path().join("anime");
    std::fs::create_dir_all(&anime).unwrap();
    std::fs::create_dir_all(anime.join("Black Clover")).unwrap();
    std::fs::create_dir_all(anime.join("Black Clover (2017) {tvdb-331753}")).unwrap();
    std::fs::create_dir_all(anime.join("Blue Lock (2022) {tvdb-408629}")).unwrap();

    let library = LibraryConfig {
        name: "Anime".to_string(),
        path: anime.clone(),
        media_type: MediaType::Tv,
        content_type: Some(ContentType::Anime),
        depth: 1,
    };

    let groups = collect_anime_root_duplicate_groups(&[&library]);

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].normalized_title, "Black Clover");
    assert_eq!(groups[0].untagged_roots, vec![anime.join("Black Clover")]);
    assert_eq!(
        groups[0].tagged_roots,
        vec![anime.join("Black Clover (2017) {tvdb-331753}")]
    );
}

#[test]
fn test_summarize_plex_duplicate_show_records_counts_hama_splits() {
    let records = [
        plex_db::PlexDuplicateShowRecord {
            title: "Show A".to_string(),
            original_title: String::new(),
            year: Some(2024),
            guid: "com.plexapp.agents.hama://anidb-100?lang=en".to_string(),
            guid_kind: "hama-anidb".to_string(),
            live: true,
        },
        plex_db::PlexDuplicateShowRecord {
            title: "Show A".to_string(),
            original_title: String::new(),
            year: Some(2024),
            guid: "com.plexapp.agents.hama://tvdb-200?lang=en".to_string(),
            guid_kind: "hama-tvdb".to_string(),
            live: true,
        },
        plex_db::PlexDuplicateShowRecord {
            title: "Show B".to_string(),
            original_title: String::new(),
            year: Some(2025),
            guid: "com.plexapp.agents.hama://tvdb-201?lang=en".to_string(),
            guid_kind: "hama-tvdb".to_string(),
            live: true,
        },
        plex_db::PlexDuplicateShowRecord {
            title: "Show B".to_string(),
            original_title: String::new(),
            year: Some(2025),
            guid: "com.plexapp.agents.hama://tvdb-202?lang=en".to_string(),
            guid_kind: "hama-tvdb".to_string(),
            live: false,
        },
    ];
    let summary = summarize_plex_duplicate_show_records(&records, PATH_SAMPLE_LIMIT);

    assert_eq!(summary.total_groups, 2);
    assert_eq!(summary.hama_anidb_tvdb_groups, 1);
    assert_eq!(summary.other_groups, 1);
    assert_eq!(summary.all_groups.len(), 2);
    assert_eq!(summary.sample_groups.len(), 2);
    assert_eq!(summary.sample_groups[0].total_rows, 2);
    assert_eq!(summary.sample_groups[0].live_rows, 2);
    assert_eq!(summary.sample_groups[0].deleted_rows, 0);
    assert_eq!(summary.sample_groups[1].total_rows, 2);
    assert_eq!(summary.sample_groups[1].live_rows, 1);
    assert_eq!(summary.sample_groups[1].deleted_rows, 1);
    assert_eq!(
        summary.sample_groups[0].guids,
        vec![
            "com.plexapp.agents.hama://anidb-100?lang=en".to_string(),
            "com.plexapp.agents.hama://tvdb-200?lang=en".to_string()
        ]
    );

    let limited = summarize_plex_duplicate_show_records(&records, 1);
    assert_eq!(limited.sample_groups.len(), 1);
    assert_eq!(limited.total_groups, 2);
    assert_eq!(limited.all_groups.len(), 2);
}

#[test]
fn test_correlate_anime_duplicate_groups_matches_hama_split_titles() {
    let filesystem_groups = vec![crate::anime_roots::AnimeRootDuplicateGroup {
        normalized_title: "Show A".to_string(),
        tagged_roots: vec![PathBuf::from("/anime/Show A (2024) {tvdb-1}")],
        untagged_roots: vec![PathBuf::from("/anime/Show A")],
    }];
    let plex_groups = vec![
        PlexDuplicateShowSample {
            title: "Show A".to_string(),
            original_title: String::new(),
            year: Some(2024),
            total_rows: 3,
            live_rows: 2,
            deleted_rows: 1,
            guid_kinds: vec!["hama-anidb".to_string(), "hama-tvdb".to_string()],
            guids: vec![
                "com.plexapp.agents.hama://anidb-100?lang=en".to_string(),
                "com.plexapp.agents.hama://tvdb-200?lang=en".to_string(),
            ],
        },
        PlexDuplicateShowSample {
            title: "Show B".to_string(),
            original_title: String::new(),
            year: Some(2024),
            total_rows: 2,
            live_rows: 2,
            deleted_rows: 0,
            guid_kinds: vec!["hama-tvdb".to_string(), "hama-tvdb".to_string()],
            guids: vec![
                "com.plexapp.agents.hama://tvdb-201?lang=en".to_string(),
                "com.plexapp.agents.hama://tvdb-202?lang=en".to_string(),
            ],
        },
    ];

    let correlated = correlate_anime_duplicate_groups(&filesystem_groups, &plex_groups);
    assert_eq!(
        correlated,
        vec![CorrelatedAnimeDuplicateSample {
            normalized_title: "Show A".to_string(),
            tagged_roots: vec![PathBuf::from("/anime/Show A (2024) {tvdb-1}")],
            untagged_roots: vec![PathBuf::from("/anime/Show A")],
            plex_total_rows: 3,
            plex_live_rows: 2,
            plex_deleted_rows: 1,
            plex_guid_kinds: vec!["hama-anidb".to_string(), "hama-tvdb".to_string()],
            plex_guids: vec![
                "com.plexapp.agents.hama://anidb-100?lang=en".to_string(),
                "com.plexapp.agents.hama://tvdb-200?lang=en".to_string(),
            ],
        }]
    );
}

#[test]
fn test_collect_anime_root_usage_counts_filesystem_and_db_activity() {
    let dir = tempfile::TempDir::new().unwrap();
    let anime = dir.path().join("anime");
    let source = dir.path().join("rd");
    std::fs::create_dir_all(&anime).unwrap();
    std::fs::create_dir_all(&source).unwrap();

    let legacy_root = anime.join("Show A");
    let tagged_root = anime.join("Show A (2024) {tvdb-1}");
    std::fs::create_dir_all(legacy_root.join("Season 01")).unwrap();
    std::fs::create_dir_all(tagged_root.join("Season 01")).unwrap();

    let source_a = source.join("show-a-e01.mkv");
    let source_b = source.join("show-a-e02.mkv");
    std::fs::write(&source_a, b"a").unwrap();
    std::fs::write(&source_b, b"b").unwrap();

    let legacy_link = legacy_root.join("Season 01/Show A - S01E01.mkv");
    let tagged_link = tagged_root.join("Season 01/Show A - S01E02.mkv");
    symlink(&source_a, &legacy_link).unwrap();
    symlink(&source_b, &tagged_link).unwrap();

    let library = LibraryConfig {
        name: "Anime".to_string(),
        path: anime.clone(),
        media_type: MediaType::Tv,
        content_type: Some(ContentType::Anime),
        depth: 1,
    };
    let link_records = vec![LinkRecord {
        id: None,
        source_path: source_b,
        target_path: tagged_link.clone(),
        media_id: "tvdb-1".to_string(),
        media_type: MediaType::Tv,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    }];

    let usage = collect_anime_root_usage(&[&library], &link_records);
    assert_eq!(usage.get(&legacy_root).unwrap().filesystem_symlinks, 1);
    assert_eq!(usage.get(&legacy_root).unwrap().db_active_links, 0);
    assert_eq!(usage.get(&tagged_root).unwrap().filesystem_symlinks, 1);
    assert_eq!(usage.get(&tagged_root).unwrap().db_active_links, 1);
}

#[test]
fn test_build_anime_remediation_samples_prioritizes_heaviest_legacy_root() {
    let correlated_groups = vec![
        CorrelatedAnimeDuplicateSample {
            normalized_title: "Show A".to_string(),
            tagged_roots: vec![PathBuf::from("/anime/Show A (2024) {tvdb-1}")],
            untagged_roots: vec![PathBuf::from("/anime/Show A")],
            plex_total_rows: 2,
            plex_live_rows: 2,
            plex_deleted_rows: 0,
            plex_guid_kinds: vec!["hama-anidb".to_string(), "hama-tvdb".to_string()],
            plex_guids: vec![],
        },
        CorrelatedAnimeDuplicateSample {
            normalized_title: "Show B".to_string(),
            tagged_roots: vec![PathBuf::from("/anime/Show B (2024) {tvdb-2}")],
            untagged_roots: vec![PathBuf::from("/anime/Show B")],
            plex_total_rows: 2,
            plex_live_rows: 1,
            plex_deleted_rows: 1,
            plex_guid_kinds: vec!["hama-anidb".to_string(), "hama-tvdb".to_string()],
            plex_guids: vec![],
        },
    ];
    let root_usage = HashMap::from([
        (
            PathBuf::from("/anime/Show A"),
            AnimeRootUsage {
                filesystem_symlinks: 12,
                db_active_links: 0,
            },
        ),
        (
            PathBuf::from("/anime/Show A (2024) {tvdb-1}"),
            AnimeRootUsage {
                filesystem_symlinks: 8,
                db_active_links: 8,
            },
        ),
        (
            PathBuf::from("/anime/Show B"),
            AnimeRootUsage {
                filesystem_symlinks: 3,
                db_active_links: 2,
            },
        ),
        (
            PathBuf::from("/anime/Show B (2024) {tvdb-2}"),
            AnimeRootUsage {
                filesystem_symlinks: 5,
                db_active_links: 5,
            },
        ),
    ]);

    let remediation = build_anime_remediation_samples(&correlated_groups, &root_usage);
    assert_eq!(remediation.len(), 2);
    assert_eq!(remediation[0].normalized_title, "Show A");
    assert_eq!(remediation[0].legacy_roots[0].filesystem_symlinks, 12);
    assert_eq!(
        remediation[0].recommended_tagged_root.path,
        PathBuf::from("/anime/Show A (2024) {tvdb-1}")
    );
}

#[test]
fn test_collect_anime_root_usage_counts_fs_and_db_links_per_show_root() {
    let dir = tempfile::TempDir::new().unwrap();
    let anime = dir.path().join("anime");
    let rd = dir.path().join("rd");
    std::fs::create_dir_all(&anime).unwrap();
    std::fs::create_dir_all(&rd).unwrap();

    let tagged_root = anime.join("Show A (2024) {tvdb-1}");
    let legacy_root = anime.join("Show A");
    let tagged_season = tagged_root.join("Season 01");
    let legacy_season = legacy_root.join("Season 01");
    std::fs::create_dir_all(&tagged_season).unwrap();
    std::fs::create_dir_all(&legacy_season).unwrap();

    let tagged_source = rd.join("tagged.mkv");
    let legacy_source = rd.join("legacy.mkv");
    std::fs::write(&tagged_source, b"a").unwrap();
    std::fs::write(&legacy_source, b"b").unwrap();

    let tagged_link = tagged_season.join("Show A - S01E01.mkv");
    let legacy_link = legacy_season.join("Show A - S01E01.mkv");
    symlink(&tagged_source, &tagged_link).unwrap();
    symlink(&legacy_source, &legacy_link).unwrap();

    let library = LibraryConfig {
        name: "Anime".to_string(),
        path: anime.clone(),
        media_type: MediaType::Tv,
        content_type: Some(ContentType::Anime),
        depth: 1,
    };
    let link_records = vec![
        LinkRecord {
            id: None,
            source_path: tagged_source,
            target_path: tagged_link,
            media_id: "tvdb-1".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        },
        LinkRecord {
            id: None,
            source_path: legacy_source,
            target_path: legacy_link,
            media_id: "tvdb-1".to_string(),
            media_type: MediaType::Tv,
            status: LinkStatus::Active,
            created_at: None,
            updated_at: None,
        },
    ];

    let usage = collect_anime_root_usage(&[&library], &link_records);
    assert_eq!(
        usage.get(&tagged_root),
        Some(&AnimeRootUsage {
            filesystem_symlinks: 1,
            db_active_links: 1,
        })
    );
    assert_eq!(
        usage.get(&legacy_root),
        Some(&AnimeRootUsage {
            filesystem_symlinks: 1,
            db_active_links: 1,
        })
    );
}

#[test]
fn test_build_anime_remediation_samples_prefers_tagged_root_with_more_tracked_links() {
    let tagged_a = PathBuf::from("/anime/Show A (2024) {tvdb-1}");
    let tagged_b = PathBuf::from("/anime/Show A (2024) {tvdb-2}");
    let legacy = PathBuf::from("/anime/Show A");
    let correlated = vec![CorrelatedAnimeDuplicateSample {
        normalized_title: "Show A".to_string(),
        tagged_roots: vec![tagged_a.clone(), tagged_b.clone()],
        untagged_roots: vec![legacy.clone()],
        plex_total_rows: 2,
        plex_live_rows: 2,
        plex_deleted_rows: 0,
        plex_guid_kinds: vec!["hama-anidb".to_string(), "hama-tvdb".to_string()],
        plex_guids: vec![
            "com.plexapp.agents.hama://anidb-100?lang=en".to_string(),
            "com.plexapp.agents.hama://tvdb-200?lang=en".to_string(),
        ],
    }];
    let usage = HashMap::from([
        (
            tagged_a.clone(),
            AnimeRootUsage {
                filesystem_symlinks: 2,
                db_active_links: 1,
            },
        ),
        (
            tagged_b.clone(),
            AnimeRootUsage {
                filesystem_symlinks: 1,
                db_active_links: 3,
            },
        ),
        (
            legacy.clone(),
            AnimeRootUsage {
                filesystem_symlinks: 7,
                db_active_links: 2,
            },
        ),
    ]);

    let samples = build_anime_remediation_samples(&correlated, &usage);
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].recommended_tagged_root.path, tagged_b);
    assert_eq!(samples[0].recommended_tagged_root.db_active_links, 3);
    assert_eq!(samples[0].alternate_tagged_roots.len(), 1);
    assert_eq!(samples[0].alternate_tagged_roots[0].path, tagged_a);
    assert_eq!(samples[0].legacy_roots.len(), 1);
    assert_eq!(samples[0].legacy_roots[0].path, legacy);
    assert_eq!(samples[0].legacy_roots[0].filesystem_symlinks, 7);
}

#[test]
fn test_write_anime_remediation_tsv_outputs_expected_columns() {
    let dir = tempfile::TempDir::new().unwrap();
    let out = dir.path().join("remediation.tsv");
    let report = ReportOutput {
        generated_at: "2026-03-30T18:00:00Z".to_string(),
        summary: Summary::default(),
        by_media_type: BTreeMap::new(),
        top_libraries: Vec::new(),
        path_compare: PathCompareOutput::default(),
        anime_duplicates: Some(AnimeDuplicateAuditOutput {
            remediation_groups: Some(1),
            remediation_sample_groups: Some(vec![AnimeRemediationSample {
                normalized_title: "Show A".to_string(),
                recommended_tagged_root: AnimeRootUsageSample {
                    path: PathBuf::from("/anime/Show A (2024) {tvdb-1}"),
                    filesystem_symlinks: 12,
                    db_active_links: 10,
                },
                alternate_tagged_roots: Vec::new(),
                legacy_roots: vec![AnimeRootUsageSample {
                    path: PathBuf::from("/anime/Show A"),
                    filesystem_symlinks: 7,
                    db_active_links: 2,
                }],
                plex_total_rows: 2,
                plex_live_rows: 2,
                plex_deleted_rows: 0,
                plex_guid_kinds: vec!["hama-anidb".to_string(), "hama-tvdb".to_string()],
                plex_guids: vec![
                    "com.plexapp.agents.hama://anidb-100?lang=en".to_string(),
                    "com.plexapp.agents.hama://tvdb-200?lang=en".to_string(),
                ],
            }]),
            ..Default::default()
        }),
    };

    write_anime_remediation_tsv(&out, &report).unwrap();
    let tsv = std::fs::read_to_string(out).unwrap();
    assert!(tsv.contains("normalized_title\tlegacy_root_paths"));
    assert!(tsv.contains("Show A"));
    assert!(tsv.contains("/anime/Show A"));
    assert!(tsv.contains("/anime/Show A (2024) {tvdb-1}"));
    assert!(tsv.contains("hama-anidb | hama-tvdb"));
}

#[test]
fn test_symlink_source_missing_broken() {
    let dir = tempfile::TempDir::new().unwrap();
    let link_path = dir.path().join("broken_link");
    std::os::unix::fs::symlink(std::path::Path::new("/nonexistent_target"), &link_path).unwrap();
    assert!(symlink_source_missing(&link_path));
}

#[test]
fn test_symlink_source_missing_valid() {
    let dir = tempfile::TempDir::new().unwrap();
    let target = dir.path().join("target_file");
    std::fs::write(&target, b"content").unwrap();
    let link_path = dir.path().join("valid_link");
    std::os::unix::fs::symlink(&target, &link_path).unwrap();
    assert!(!symlink_source_missing(&link_path));
}

#[test]
fn test_collect_link_presence_active_and_dead() {
    let dir = tempfile::TempDir::new().unwrap();
    let movies = dir.path().join("movies");
    let series = dir.path().join("series");
    std::fs::create_dir_all(&movies).unwrap();
    std::fs::create_dir_all(&series).unwrap();

    let movie_a = movies.join("Movie A {tmdb-1}");
    let series_b = series.join("Show B {tvdb-2}");
    std::fs::create_dir_all(&movie_a).unwrap();
    std::fs::create_dir_all(&series_b).unwrap();

    let movie_lib = LibraryConfig {
        name: "Movies".to_string(),
        path: movies.clone(),
        media_type: crate::models::MediaType::Movie,
        content_type: None,
        depth: 1,
    };
    let series_lib = LibraryConfig {
        name: "Series".to_string(),
        path: series.clone(),
        media_type: crate::models::MediaType::Tv,
        content_type: None,
        depth: 1,
    };
    let libraries = vec![&movie_lib, &series_lib];

    let link_a = LinkRecord {
        id: None,
        source_path: PathBuf::from("/rd/movie-a.mkv"),
        target_path: movie_a.join("Movie A.mkv"),
        media_id: "tmdb-1".to_string(),
        media_type: MediaType::Movie,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    };
    let link_b = LinkRecord {
        id: None,
        source_path: PathBuf::from("/rd/show-b-s01e01.mkv"),
        target_path: series_b.join("Show B - S01E01.mkv"),
        media_id: "tvdb-2".to_string(),
        media_type: MediaType::Tv,
        status: LinkStatus::Dead,
        created_at: None,
        updated_at: None,
    };
    let link_c = LinkRecord {
        id: None,
        source_path: PathBuf::from("/rd/removed.mkv"),
        target_path: movies.join("Removed.mkv"),
        media_id: "tmdb-99".to_string(),
        media_type: MediaType::Movie,
        status: LinkStatus::Removed,
        created_at: None,
        updated_at: None,
    };

    let link_records = vec![link_a.clone(), link_b.clone(), link_c.clone()];
    let presence = collect_link_presence(&libraries, &link_records);

    // Movies: 1 active, 0 dead
    let movie_presence = presence.get("Movies").unwrap();
    assert!(movie_presence.active_media_ids.contains("tmdb-1"));
    assert!(!movie_presence.active_media_ids.contains("tmdb-99")); // Removed is not active
    assert!(!movie_presence.dead_media_ids.contains("tmdb-1"));
    assert!(!movie_presence.dead_media_ids.contains("tmdb-99"));

    // Series: 0 active, 1 dead
    let series_presence = presence.get("Series").unwrap();
    assert!(series_presence.active_media_ids.is_empty());
    assert!(series_presence.dead_media_ids.contains("tvdb-2"));

    // All libraries pre-filled even with no links
    assert!(presence.contains_key("Movies"));
    assert!(presence.contains_key("Series"));
}

#[test]
fn test_collect_link_presence_unknown_library() {
    // Link to a path not under any configured library should be ignored
    let dir = tempfile::TempDir::new().unwrap();
    let movies = dir.path().join("movies");
    std::fs::create_dir_all(&movies).unwrap();

    let movie_lib = LibraryConfig {
        name: "Movies".to_string(),
        path: movies.clone(),
        media_type: crate::models::MediaType::Movie,
        content_type: None,
        depth: 1,
    };
    let libraries = vec![&movie_lib];

    // Link pointing to /somewhere/else entirely
    let orphan_link = LinkRecord {
        id: None,
        source_path: PathBuf::from("/rd/unknown.mkv"),
        target_path: PathBuf::from("/unknown/path/link.mkv"),
        media_id: "tmdb-999".to_string(),
        media_type: MediaType::Movie,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    };

    let presence = collect_link_presence(&libraries, &[orphan_link]);
    // Should still have Movies pre-filled
    let movie_presence = presence.get("Movies").unwrap();
    assert!(movie_presence.active_media_ids.is_empty());
    assert!(movie_presence.dead_media_ids.is_empty());
}

fn test_config(movies: PathBuf, anime: PathBuf, source: PathBuf, db_path: String) -> Config {
    Config {
        libraries: vec![
            LibraryConfig {
                name: "Movies".to_string(),
                path: movies,
                media_type: MediaType::Movie,
                content_type: Some(ContentType::Movie),
                depth: 1,
            },
            LibraryConfig {
                name: "Anime".to_string(),
                path: anime,
                media_type: MediaType::Tv,
                content_type: Some(ContentType::Anime),
                depth: 1,
            },
        ],
        sources: vec![SourceConfig {
            name: "RD".to_string(),
            path: source,
            media_type: "auto".to_string(),
        }],
        api: ApiConfig::default(),
        realdebrid: RealDebridConfig::default(),
        decypharr: DecypharrConfig::default(),
        dmm: DmmConfig::default(),
        backup: BackupConfig::default(),
        db_path,
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

async fn create_test_plex_db(
    db_path: &Path,
    movie_root: &Path,
    anime_root: &Path,
    indexed_paths: &[PathBuf],
) {
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();

    for statement in [
            "CREATE TABLE section_locations (id INTEGER PRIMARY KEY, library_section_id INTEGER, root_path TEXT, available BOOLEAN, scanned_at INTEGER, created_at INTEGER, updated_at INTEGER)",
            "CREATE TABLE metadata_items (id INTEGER PRIMARY KEY, library_section_id INTEGER, metadata_type INTEGER, title TEXT, original_title TEXT, year INTEGER, guid TEXT, deleted_at INTEGER)",
            "CREATE TABLE media_items (id INTEGER PRIMARY KEY, library_section_id INTEGER, section_location_id INTEGER, metadata_item_id INTEGER, deleted_at INTEGER)",
            "CREATE TABLE media_parts (id INTEGER PRIMARY KEY, media_item_id INTEGER, file TEXT, deleted_at INTEGER)",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }

    sqlx::query("INSERT INTO section_locations (id, library_section_id, root_path) VALUES (1, 1, ?), (2, 2, ?)")
            .bind(movie_root.to_string_lossy().to_string())
            .bind(anime_root.to_string_lossy().to_string())
            .execute(&pool)
            .await
            .unwrap();

    for (idx, path) in indexed_paths.iter().enumerate() {
        let media_item_id = (idx + 1) as i64;
        let metadata_item_id = media_item_id;
        let section_location_id = if path.starts_with(movie_root) { 1 } else { 2 };
        let library_section_id = section_location_id;
        sqlx::query(
                "INSERT INTO metadata_items (id, library_section_id, metadata_type, title, original_title, year, guid, deleted_at) VALUES (?, ?, 4, '', '', NULL, '', NULL)",
            )
            .bind(metadata_item_id)
            .bind(library_section_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
                "INSERT INTO media_items (id, library_section_id, section_location_id, metadata_item_id, deleted_at) VALUES (?, ?, ?, ?, NULL)",
            )
            .bind(media_item_id)
            .bind(library_section_id)
            .bind(section_location_id)
            .bind(metadata_item_id)
            .execute(&pool)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO media_parts (id, media_item_id, file, deleted_at) VALUES (?, ?, ?, NULL)",
        )
        .bind(media_item_id)
        .bind(media_item_id)
        .bind(path.to_string_lossy().to_string())
        .execute(&pool)
        .await
        .unwrap();
    }

    pool.close().await;
}

async fn mark_test_plex_path_deleted(db_path: &Path, path: &Path) {
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(false);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();

    sqlx::query("UPDATE media_parts SET deleted_at = 1 WHERE file = ?")
        .bind(path.to_string_lossy().to_string())
        .execute(&pool)
        .await
        .unwrap();

    pool.close().await;
}

#[tokio::test]
async fn test_build_report_groups_by_library_and_media_type() {
    let dir = tempfile::TempDir::new().unwrap();
    let movies = dir.path().join("movies");
    let anime = dir.path().join("anime");
    let source = dir.path().join("rd");
    std::fs::create_dir_all(&movies).unwrap();
    std::fs::create_dir_all(&anime).unwrap();
    std::fs::create_dir_all(&source).unwrap();

    std::fs::create_dir_all(movies.join("Movie A {tmdb-1}")).unwrap();
    std::fs::create_dir_all(movies.join("Movie B {tmdb-2}")).unwrap();
    std::fs::create_dir_all(anime.join("Show A {tvdb-10}")).unwrap();
    std::fs::create_dir_all(anime.join("Show B {tvdb-11}")).unwrap();

    let db_path = dir.path().join("test.db");
    let cfg = test_config(
        movies.clone(),
        anime.clone(),
        source.clone(),
        db_path.to_string_lossy().into_owned(),
    );
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    db.insert_link(&LinkRecord {
        id: None,
        source_path: source.join("movie-a.mkv"),
        target_path: movies.join("Movie A {tmdb-1}/Movie A.mkv"),
        media_id: "tmdb-1".to_string(),
        media_type: MediaType::Movie,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();
    db.insert_link(&LinkRecord {
        id: None,
        source_path: source.join("movie-b.mkv"),
        target_path: movies.join("Movie B {tmdb-2}/Movie B.mkv"),
        media_id: "tmdb-2".to_string(),
        media_type: MediaType::Movie,
        status: LinkStatus::Dead,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();
    db.insert_link(&LinkRecord {
        id: None,
        source_path: source.join("show-a.mkv"),
        target_path: anime.join("Show A {tvdb-10}/Season 01/Show A - S01E01.mkv"),
        media_id: "tvdb-10".to_string(),
        media_type: MediaType::Tv,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();
    db.insert_link(&LinkRecord {
        id: None,
        source_path: source.join("show-a-stale.mkv"),
        target_path: anime.join("Show A {tvdb-10}/Season 01/Show A - S01E02.mkv"),
        media_id: "tvdb-10".to_string(),
        media_type: MediaType::Tv,
        status: LinkStatus::Dead,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();

    let report = build_report(&cfg, &db, None, None, None, false)
        .await
        .unwrap();

    assert_eq!(
        report.summary,
        Summary {
            total_library_items: 4,
            items_with_symlinks: 2,
            broken_symlinks: 2,
            missing_from_rd: 2,
        }
    );
    assert_eq!(
        report.by_media_type.get("movie"),
        Some(&MediaTypeInfo {
            library_items: 2,
            linked: 1,
            broken: 1,
        })
    );
    assert_eq!(
        report.by_media_type.get("series"),
        Some(&MediaTypeInfo {
            library_items: 2,
            linked: 1,
            broken: 1,
        })
    );
    assert_eq!(report.top_libraries.len(), 2);
    assert!(report.top_libraries.iter().any(|lib| lib
        == &LibraryInfo {
            name: "Movies".to_string(),
            items: 2,
            linked: 1,
            broken: 1,
        }));
    assert!(report.top_libraries.iter().any(|lib| lib
        == &LibraryInfo {
            name: "Anime".to_string(),
            items: 2,
            linked: 1,
            broken: 1,
        }));
}

#[tokio::test]
async fn test_build_report_applies_media_type_filter() {
    let dir = tempfile::TempDir::new().unwrap();
    let movies = dir.path().join("movies");
    let anime = dir.path().join("anime");
    let source = dir.path().join("rd");
    std::fs::create_dir_all(&movies).unwrap();
    std::fs::create_dir_all(&anime).unwrap();
    std::fs::create_dir_all(&source).unwrap();

    std::fs::create_dir_all(movies.join("Movie A {tmdb-1}")).unwrap();
    std::fs::create_dir_all(anime.join("Show A {tvdb-10}")).unwrap();

    let db_path = dir.path().join("test.db");
    let cfg = test_config(
        movies.clone(),
        anime.clone(),
        source.clone(),
        db_path.to_string_lossy().into_owned(),
    );
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    db.insert_link(&LinkRecord {
        id: None,
        source_path: source.join("movie-a.mkv"),
        target_path: movies.join("Movie A {tmdb-1}/Movie A.mkv"),
        media_id: "tmdb-1".to_string(),
        media_type: MediaType::Movie,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();
    db.insert_link(&LinkRecord {
        id: None,
        source_path: source.join("show-a.mkv"),
        target_path: anime.join("Show A {tvdb-10}/Season 01/Show A - S01E01.mkv"),
        media_id: "tvdb-10".to_string(),
        media_type: MediaType::Tv,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();

    let report = build_report(&cfg, &db, Some(MediaType::Movie), None, None, false)
        .await
        .unwrap();

    assert_eq!(
        report.summary,
        Summary {
            total_library_items: 1,
            items_with_symlinks: 1,
            broken_symlinks: 0,
            missing_from_rd: 0,
        }
    );
    assert_eq!(report.by_media_type.len(), 1);
    assert_eq!(report.top_libraries.len(), 1);
    assert_eq!(report.top_libraries[0].name, "Movies");
}

#[tokio::test]
async fn test_build_report_path_compare_tracks_fs_db_and_plex_drift() {
    let dir = tempfile::TempDir::new().unwrap();
    let movies = dir.path().join("movies");
    let anime = dir.path().join("anime");
    let source = dir.path().join("rd");
    std::fs::create_dir_all(&movies).unwrap();
    std::fs::create_dir_all(&anime).unwrap();
    std::fs::create_dir_all(&source).unwrap();

    let movie_a_dir = movies.join("Movie A {tmdb-1}");
    let movie_b_dir = movies.join("Movie B {tmdb-2}");
    let show_a_dir = anime.join("Show A {tvdb-10}/Season 01");
    let show_b_dir = anime.join("Show B {tvdb-11}/Season 01");
    std::fs::create_dir_all(&movie_a_dir).unwrap();
    std::fs::create_dir_all(&movie_b_dir).unwrap();
    std::fs::create_dir_all(&show_a_dir).unwrap();
    std::fs::create_dir_all(&show_b_dir).unwrap();

    let source_a = source.join("movie-a-source.mkv");
    let source_b = source.join("movie-b-source.mkv");
    std::fs::write(&source_a, b"a").unwrap();
    std::fs::write(&source_b, b"b").unwrap();

    let movie_a_link = movie_a_dir.join("Movie A.mkv");
    let movie_b_link = movie_b_dir.join("Movie B.mkv");
    symlink(&source_a, &movie_a_link).unwrap();
    symlink(&source_b, &movie_b_link).unwrap();

    let db_path = dir.path().join("test.db");
    let plex_db_path = dir.path().join("plex.db");
    let cfg = test_config(
        movies.clone(),
        anime.clone(),
        source.clone(),
        db_path.to_string_lossy().into_owned(),
    );
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    let db_only_path = show_a_dir.join("Show A - S01E01.mkv");
    db.insert_link(&LinkRecord {
        id: None,
        source_path: source_a.clone(),
        target_path: movie_a_link.clone(),
        media_id: "tmdb-1".to_string(),
        media_type: MediaType::Movie,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();
    db.insert_link(&LinkRecord {
        id: None,
        source_path: source.join("show-a-source.mkv"),
        target_path: db_only_path.clone(),
        media_id: "tvdb-10".to_string(),
        media_type: MediaType::Tv,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();

    let plex_only_path = show_b_dir.join("Show B - S01E01.mkv");
    create_test_plex_db(
        &plex_db_path,
        &movies,
        &anime,
        &[movie_a_link.clone(), plex_only_path.clone()],
    )
    .await;

    let report = build_report(&cfg, &db, None, None, Some(&plex_db_path), false)
        .await
        .unwrap();

    assert_eq!(report.path_compare.filesystem_symlinks, 2);
    assert_eq!(report.path_compare.db_active_links, 2);
    assert_eq!(report.path_compare.plex_indexed_files, Some(2));
    assert_eq!(report.path_compare.plex_deleted_paths, Some(0));
    assert_eq!(report.path_compare.fs_not_in_db.count, 1);
    assert_eq!(report.path_compare.db_not_on_fs.count, 1);
    assert_eq!(
        report.path_compare.fs_not_in_plex.as_ref().unwrap().count,
        1
    );
    assert_eq!(
        report.path_compare.db_not_in_plex.as_ref().unwrap().count,
        1
    );
    assert_eq!(
        report.path_compare.plex_not_on_fs.as_ref().unwrap().count,
        1
    );
    assert_eq!(
        report
            .path_compare
            .plex_deleted_and_known_missing_source
            .as_ref()
            .unwrap()
            .count,
        0
    );
    assert_eq!(
        report
            .path_compare
            .plex_deleted_without_known_missing_source
            .as_ref()
            .unwrap()
            .count,
        0
    );
    assert_eq!(report.path_compare.all_three, Some(1));

    assert_eq!(report.path_compare.fs_not_in_db.samples, vec![movie_b_link]);
    assert_eq!(report.path_compare.db_not_on_fs.samples, vec![db_only_path]);
    assert_eq!(
        report.path_compare.plex_not_on_fs.as_ref().unwrap().samples,
        vec![plex_only_path]
    );
}

#[tokio::test]
async fn test_build_report_path_compare_without_plex_db_still_tracks_fs_db_drift() {
    let dir = tempfile::TempDir::new().unwrap();
    let movies = dir.path().join("movies");
    let anime = dir.path().join("anime");
    let source = dir.path().join("rd");
    std::fs::create_dir_all(&movies).unwrap();
    std::fs::create_dir_all(&anime).unwrap();
    std::fs::create_dir_all(&source).unwrap();

    let movie_a_dir = movies.join("Movie A {tmdb-1}");
    let movie_b_dir = movies.join("Movie B {tmdb-2}");
    let show_a_dir = anime.join("Show A {tvdb-10}/Season 01");
    std::fs::create_dir_all(&movie_a_dir).unwrap();
    std::fs::create_dir_all(&movie_b_dir).unwrap();
    std::fs::create_dir_all(&show_a_dir).unwrap();

    let source_a = source.join("movie-a-source.mkv");
    let source_b = source.join("movie-b-source.mkv");
    std::fs::write(&source_a, b"a").unwrap();
    std::fs::write(&source_b, b"b").unwrap();

    let movie_a_link = movie_a_dir.join("Movie A.mkv");
    let movie_b_link = movie_b_dir.join("Movie B.mkv");
    symlink(&source_a, &movie_a_link).unwrap();
    symlink(&source_b, &movie_b_link).unwrap();

    let db_path = dir.path().join("test.db");
    let cfg = test_config(
        movies.clone(),
        anime.clone(),
        source.clone(),
        db_path.to_string_lossy().into_owned(),
    );
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    let db_only_path = show_a_dir.join("Show A - S01E01.mkv");
    db.insert_link(&LinkRecord {
        id: None,
        source_path: source_a.clone(),
        target_path: movie_a_link.clone(),
        media_id: "tmdb-1".to_string(),
        media_type: MediaType::Movie,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();
    db.insert_link(&LinkRecord {
        id: None,
        source_path: source.join("show-a-source.mkv"),
        target_path: db_only_path.clone(),
        media_id: "tvdb-10".to_string(),
        media_type: MediaType::Tv,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();

    let report = build_report(&cfg, &db, None, None, None, false)
        .await
        .unwrap();

    assert_eq!(report.path_compare.filesystem_symlinks, 2);
    assert_eq!(report.path_compare.db_active_links, 2);
    assert_eq!(report.path_compare.plex_indexed_files, None);
    assert_eq!(report.path_compare.plex_deleted_paths, None);
    assert_eq!(report.path_compare.fs_not_in_db.count, 1);
    assert_eq!(report.path_compare.db_not_on_fs.count, 1);
    assert_eq!(report.path_compare.fs_not_in_db.samples, vec![movie_b_link]);
    assert_eq!(report.path_compare.db_not_on_fs.samples, vec![db_only_path]);
    assert_eq!(report.path_compare.fs_not_in_plex, None);
    assert_eq!(report.path_compare.db_not_in_plex, None);
    assert_eq!(report.path_compare.plex_not_on_fs, None);
    assert_eq!(report.path_compare.all_three, None);
}

#[tokio::test]
async fn test_build_report_path_compare_does_not_treat_plex_deleted_as_truth_without_missing_source(
) {
    let dir = tempfile::TempDir::new().unwrap();
    let movies = dir.path().join("movies");
    let anime = dir.path().join("anime");
    let source = dir.path().join("rd");
    std::fs::create_dir_all(&movies).unwrap();
    std::fs::create_dir_all(&anime).unwrap();
    std::fs::create_dir_all(&source).unwrap();

    let movie_a_dir = movies.join("Movie A {tmdb-1}");
    std::fs::create_dir_all(&movie_a_dir).unwrap();
    let source_a = source.join("movie-a-source.mkv");
    std::fs::write(&source_a, b"a").unwrap();
    let movie_a_link = movie_a_dir.join("Movie A.mkv");
    symlink(&source_a, &movie_a_link).unwrap();

    let db_path = dir.path().join("test.db");
    let plex_db_path = dir.path().join("plex.db");
    let cfg = test_config(
        movies.clone(),
        anime.clone(),
        source.clone(),
        db_path.to_string_lossy().into_owned(),
    );
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    db.insert_link(&LinkRecord {
        id: None,
        source_path: source_a.clone(),
        target_path: movie_a_link.clone(),
        media_id: "tmdb-1".to_string(),
        media_type: MediaType::Movie,
        status: LinkStatus::Active,
        created_at: None,
        updated_at: None,
    })
    .await
    .unwrap();

    create_test_plex_db(
        &plex_db_path,
        &movies,
        &anime,
        std::slice::from_ref(&movie_a_link),
    )
    .await;
    mark_test_plex_path_deleted(&plex_db_path, &movie_a_link).await;

    let report = build_report(&cfg, &db, None, None, Some(&plex_db_path), false)
        .await
        .unwrap();

    assert_eq!(report.path_compare.plex_indexed_files, Some(1));
    assert_eq!(report.path_compare.plex_deleted_paths, Some(1));
    assert_eq!(
        report
            .path_compare
            .plex_deleted_and_known_missing_source
            .as_ref()
            .unwrap()
            .count,
        0
    );
    assert_eq!(
        report
            .path_compare
            .plex_deleted_without_known_missing_source
            .as_ref()
            .unwrap()
            .count,
        1
    );
    assert_eq!(
        report
            .path_compare
            .plex_deleted_without_known_missing_source
            .as_ref()
            .unwrap()
            .samples,
        vec![movie_a_link]
    );
}

#[tokio::test]
async fn test_build_report_includes_anime_duplicate_audit_without_plex_db() {
    let dir = tempfile::TempDir::new().unwrap();
    let movies = dir.path().join("movies");
    let anime = dir.path().join("anime");
    let source = dir.path().join("rd");
    std::fs::create_dir_all(&movies).unwrap();
    std::fs::create_dir_all(&anime).unwrap();
    std::fs::create_dir_all(&source).unwrap();

    std::fs::create_dir_all(anime.join("Aldnoah.Zero")).unwrap();
    std::fs::create_dir_all(anime.join("Aldnoah.Zero (2014) {tvdb-279827}")).unwrap();
    std::fs::create_dir_all(anime.join("Blue Lock (2022) {tvdb-408629}")).unwrap();

    let db_path = dir.path().join("test.db");
    let cfg = test_config(
        movies,
        anime.clone(),
        source,
        db_path.to_string_lossy().into_owned(),
    );
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    let report = build_report(&cfg, &db, Some(MediaType::Tv), None, None, false)
        .await
        .unwrap();

    let anime_duplicates = report.anime_duplicates.expect("anime audit present");
    assert_eq!(anime_duplicates.filesystem_mixed_root_groups, 1);
    assert_eq!(anime_duplicates.filesystem_sample_groups.len(), 1);
    assert_eq!(
        anime_duplicates.filesystem_sample_groups[0].normalized_title,
        "Aldnoah.Zero"
    );
    assert_eq!(anime_duplicates.plex_duplicate_show_groups, None);
    assert_eq!(anime_duplicates.correlated_hama_split_groups, None);
    assert_eq!(anime_duplicates.correlated_sample_groups, None);
    assert_eq!(anime_duplicates.remediation_groups, None);
    assert_eq!(anime_duplicates.remediation_sample_groups, None);
}

#[tokio::test]
async fn test_build_report_full_anime_duplicates_disables_sample_cap() {
    let dir = tempfile::TempDir::new().unwrap();
    let movies = dir.path().join("movies");
    let anime = dir.path().join("anime");
    let source = dir.path().join("rd");
    std::fs::create_dir_all(&movies).unwrap();
    std::fs::create_dir_all(&anime).unwrap();
    std::fs::create_dir_all(&source).unwrap();

    for idx in 0..(PATH_SAMPLE_LIMIT + 3) {
        let title = format!("Show {idx:02}");
        std::fs::create_dir_all(anime.join(&title)).unwrap();
        std::fs::create_dir_all(anime.join(format!("{title} (2024) {{tvdb-{}}}", 1000 + idx)))
            .unwrap();
    }

    let db_path = dir.path().join("test.db");
    let cfg = test_config(
        movies,
        anime,
        source,
        db_path.to_string_lossy().into_owned(),
    );
    let db = Database::new(db_path.to_str().unwrap()).await.unwrap();

    let sampled = build_report(&cfg, &db, Some(MediaType::Tv), None, None, false)
        .await
        .unwrap();
    let full = build_report(&cfg, &db, Some(MediaType::Tv), None, None, true)
        .await
        .unwrap();

    let sampled_duplicates = sampled
        .anime_duplicates
        .expect("sampled anime audit present");
    let full_duplicates = full.anime_duplicates.expect("full anime audit present");

    assert_eq!(
        sampled_duplicates.filesystem_mixed_root_groups,
        PATH_SAMPLE_LIMIT + 3
    );
    assert_eq!(
        sampled_duplicates.filesystem_sample_groups.len(),
        PATH_SAMPLE_LIMIT
    );
    assert_eq!(
        full_duplicates.filesystem_sample_groups.len(),
        PATH_SAMPLE_LIMIT + 3
    );
}

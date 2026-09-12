use std::collections::HashSet;
use std::time::Instant;

use anyhow::Result;
use tracing::info;

use crate::api;
use crate::api::bazarr::BazarrClient;
use crate::api::tautulli::TautulliClient;
use crate::auto_acquire::{process_auto_acquire_queue, AutoAcquireRequest, RelinkCheck};
use crate::commands::{
    decypharr_arr_name, ensure_runtime_directories_healthy, is_safe_auto_acquire_query,
    prowlarr_categories, selected_libraries,
};
use crate::config::Config;
use crate::db::Database;
use crate::media_servers::{
    configured_refresh_backends, display_server_list, invalidate_after_mutation,
};
use crate::models::MediaType;
use crate::repair;
use crate::RepairAction;

async fn print_remaining_dead_link_notice(db: &Database) {
    let Ok((_, dead_links, _)) = db.get_stats().await else {
        return;
    };
    if dead_links == 0 {
        return;
    }

    println!(
        "\n   ⚠️  {} dead link(s) remain tracked after repair. They will continue to surface on scan/status until repaired or pruned.",
        dead_links
    );

    let preview_limit = 3i64;
    if let Ok(preview_links) = db.get_dead_links_limited(preview_limit).await {
        for link in &preview_links {
            println!("      ✗ {}", link.target_path.display());
        }
        let remaining = dead_links.saturating_sub(preview_links.len() as i64);
        if remaining > 0 {
            println!("      … {} more tracked dead link(s) remain.", remaining);
        }
    }
}

pub(crate) async fn run_repair(
    cfg: &Config,
    db: &Database,
    action: RepairAction,
    library_filter: Option<&str>,
) -> Result<()> {
    match action {
        RepairAction::Scan => {
            info!("=== Symlinkarr Repair Scan ===");
            let repairer = repair::Repairer::new();
            let selected = selected_libraries(cfg, library_filter)?;
            let selected_library_paths: Vec<_> = selected.iter().map(|l| l.path.clone()).collect();
            let source_paths: Vec<_> = cfg.sources.iter().map(|s| s.path.clone()).collect();
            let dead = repairer
                .scan_for_dead_symlinks(&selected_library_paths, &source_paths)
                .await;

            if dead.is_empty() {
                println!("✅ No dead symlinks found!");
            } else {
                println!("\n⚠️  {} dead symlinks found:\n", dead.len());
                for d in &dead {
                    println!("  ✗ {:?} → {:?}", d.symlink_path, d.original_source);
                }
            }
        }
        RepairAction::Auto { dry_run, self_heal } => {
            info!("=== Symlinkarr Repair Auto ===");
            if self_heal && !dry_run {
                println!(
                    "   ⚠️  --self-heal may trigger external downloads via Prowlarr/Decypharr."
                );
            }
            let results = execute_repair_auto(cfg, db, library_filter, dry_run, true).await?;

            let repaired: Vec<_> = results
                .iter()
                .filter(|r| matches!(r, repair::RepairResult::Repaired { .. }))
                .collect();
            let unrepairable: Vec<_> = results
                .iter()
                .filter(|r| matches!(r, repair::RepairResult::Unrepairable { .. }))
                .collect();

            println!("\n📋 Repair Results:");
            println!("   ✅ Repaired: {}", repaired.len());
            for result in &repaired {
                if let repair::RepairResult::Repaired {
                    dead_link,
                    replacement,
                } = result
                {
                    println!(
                        "      ✓ {} → {:?}",
                        dead_link.meta.title,
                        replacement.file_name().unwrap_or_default()
                    );
                }
            }
            println!("   ❌ Unrepairable: {}", unrepairable.len());
            for result in &unrepairable {
                if let repair::RepairResult::Unrepairable { dead_link, reason } = result {
                    println!("      ✗ {} — {}", dead_link.meta.title, reason);
                }
            }

            let skipped: Vec<_> = results
                .iter()
                .filter(|r| matches!(r, repair::RepairResult::Skipped { .. }))
                .collect();
            if !skipped.is_empty() {
                println!("   ⏸️  Skipped (streaming): {}", skipped.len());
                for result in &skipped {
                    if let repair::RepairResult::Skipped { dead_link, reason } = result {
                        println!("      ⏸ {} — {}", dead_link.meta.title, reason);
                    }
                }
            }

            let stale: Vec<_> = results
                .iter()
                .filter(|r| matches!(r, repair::RepairResult::Stale { .. }))
                .collect();
            if !stale.is_empty() {
                println!("   🗃️  Stale DB records: {}", stale.len());
                if dry_run {
                    println!(
                        "      ℹ️ Re-run without --dry-run to mark stale records as removed in DB"
                    );
                }
            }

            if self_heal
                && !unrepairable.is_empty()
                && (cfg.has_prowlarr() || cfg.has_dmm())
                && cfg.has_decypharr()
            {
                let mut attempted_queries = HashSet::new();
                let mut requests = Vec::new();
                let max_requests = cfg.decypharr.max_requests_per_run;

                for result in &unrepairable {
                    if requests.len() >= max_requests {
                        println!(
                            "   ⚠️  Self-heal reached safety limit of {} requests. Stopping queue build.",
                            max_requests
                        );
                        break;
                    }
                    if let repair::RepairResult::Unrepairable { dead_link, .. } = result {
                        let Some(query) = build_repair_self_heal_query(dead_link) else {
                            println!(
                                "   ⚠️  Skipping self-heal for '{}' — query too ambiguous",
                                dead_link.meta.title
                            );
                            continue;
                        };

                        let cats =
                            prowlarr_categories(dead_link.media_type, dead_link.content_type);
                        let request = AutoAcquireRequest {
                            label: dead_link.meta.title.clone(),
                            query: query.clone(),
                            query_hints: Vec::new(),
                            imdb_id: None,
                            categories: cats,
                            arr: decypharr_arr_name(
                                cfg,
                                dead_link.media_type,
                                dead_link.content_type,
                            )
                            .to_string(),
                            library_filter: library_name_for_path(cfg, &dead_link.symlink_path),
                            relink_check: RelinkCheck::SymlinkPath(dead_link.symlink_path.clone()),
                        };

                        if !attempted_queries.insert(query.clone()) {
                            continue;
                        }

                        requests.push(request);
                    }
                }

                let summary = process_auto_acquire_queue(cfg, db, requests, dry_run).await?;
                if summary.total > 0 {
                    println!(
                        "\n   📡 Self-heal summary: submitted={}, linked={}, completed_unlinked={}, no_result={}, blocked={}, failed={}",
                        summary.submitted,
                        summary.completed_linked,
                        summary.completed_unlinked,
                        summary.no_result,
                        summary.blocked,
                        summary.failed
                    );
                    if !dry_run {
                        println!("   ℹ️  Symlinkarr processed the queue with live progress and relink attempts");
                    }
                }
            } else if self_heal && !unrepairable.is_empty() && !cfg.has_prowlarr() && !cfg.has_dmm()
            {
                println!(
                    "\n   ⚠️  --self-heal specified but neither Prowlarr nor DMM is configured"
                );
            } else if self_heal && !unrepairable.is_empty() && !cfg.has_decypharr() {
                println!("\n   ⚠️  --self-heal specified but Decypharr not configured");
            }

            if !repaired.is_empty() && cfg.has_bazarr() && !dry_run {
                let bazarr = BazarrClient::new(&cfg.bazarr);
                println!(
                    "\n   📝 Notifying Bazarr about {} repaired files...",
                    repaired.len()
                );
                info!("Bazarr integration ready — needs Sonarr/Radarr IDs in future");
                let _ = bazarr;
            }

            if !dry_run {
                print_remaining_dead_link_notice(db).await;
            }
        }
        RepairAction::Trigger { arr } => {
            if !cfg.has_decypharr() {
                anyhow::bail!("Decypharr not configured in config.yaml");
            }

            let client = api::decypharr::DecypharrClient::from_config(&cfg.decypharr);
            if let Some(name) = arr.as_deref() {
                println!(
                    "ℹ️  Decypharr 2.3+ repairs every configured *Arr in one sweep; --arr {name} is ignored"
                );
            }
            let msg = client.trigger_repair(None, false, true).await?;
            println!("✅ {}", msg);
            if let Ok(status) = client.get_repair_status().await {
                if let Some(next) = status.next_run_at.as_deref() {
                    println!("   next scheduled sweep: {next}");
                }
                if let Some(run) = status.last_run.as_ref() {
                    println!("   last run: {} ({})", run.id, run.status);
                }
            }
        }
        RepairAction::NormalizeNames { apply } => {
            let selected = selected_libraries(cfg, library_filter)?;
            let roots: Vec<std::path::PathBuf> =
                selected.iter().map(|lib| lib.path.clone()).collect();
            normalize_movie_names(db, &roots, apply).await?;
        }
    }

    Ok(())
}

pub(crate) async fn execute_repair_auto(
    cfg: &Config,
    db: &Database,
    library_filter: Option<&str>,
    dry_run: bool,
    emit_text: bool,
) -> Result<Vec<repair::RepairResult>> {
    let repairer = repair::Repairer::new();
    let selected = selected_libraries(cfg, library_filter)?;
    let selected_library_paths: Vec<_> = selected.iter().map(|l| l.path.clone()).collect();
    let selected_owned: Vec<_> = selected.iter().map(|lib| (*lib).clone()).collect();

    ensure_runtime_directories_healthy(&selected, &cfg.sources, "repair auto").await?;

    if cfg.backup.enabled {
        if dry_run {
            println!("   ℹ️  Skipping safety snapshot in --dry-run mode");
        } else {
            println!("   🛡️ Creating safety snapshot before repair...");
            let started = Instant::now();
            let bm = crate::backup::BackupManager::new(&cfg.backup);
            bm.create_safety_snapshot(db, "repair").await?;
            println!(
                "   ✅ Safety snapshot created in {:.1}s",
                started.elapsed().as_secs_f64()
            );
        }
    }

    let skip_paths = if cfg.has_tautulli() {
        let tautulli = TautulliClient::new(&cfg.tautulli);
        match tautulli.get_active_file_paths().await {
            Ok(paths) => {
                if !paths.is_empty() {
                    println!(
                        "   🎬 Tautulli: {} active streams detected — protecting those files",
                        paths.len()
                    );
                }
                paths
            }
            Err(e) => {
                println!(
                    "   ⚠️  Tautulli query failed ({}), proceeding without guard",
                    e
                );
                vec![]
            }
        }
    } else {
        vec![]
    };

    let source_paths: Vec<_> = cfg.sources.iter().map(|s| s.path.clone()).collect();

    let rd_client = if cfg.has_realdebrid() {
        Some(crate::api::realdebrid::RealDebridClient::from_config(
            &cfg.realdebrid,
        ))
    } else {
        None
    };
    let torrent_cache = rd_client
        .as_ref()
        .map(|rd| crate::cache::TorrentCache::new(db, rd));
    const REPAIR_CACHE_COVERAGE: f64 = 0.80;
    let cache_ref = if let Some(ref tc) = torrent_cache {
        match db.get_rd_torrent_counts().await {
            Ok((cached, total)) if total > 0 => {
                let coverage = cached as f64 / total as f64;
                if coverage >= REPAIR_CACHE_COVERAGE {
                    println!(
                        "   ⚡ Using RD cache for repair catalog ({:.0}% coverage)",
                        coverage * 100.0
                    );
                    Some(tc)
                } else {
                    println!(
                        "   ℹ️  RD cache coverage {:.0}% < 80%, using filesystem walk",
                        coverage * 100.0
                    );
                    None
                }
            }
            _ => None,
        }
    } else {
        None
    };

    let results = repairer
        .repair_all(
            db,
            &source_paths,
            dry_run,
            &skip_paths,
            Some(&selected_library_paths),
            Some(&selected_owned),
            cache_ref,
        )
        .await?;

    let repaired_count = results
        .iter()
        .filter(|r| matches!(r, repair::RepairResult::Repaired { .. }))
        .count();
    let stale_count = results
        .iter()
        .filter(|r| matches!(r, repair::RepairResult::Stale { .. }))
        .count();
    if repaired_count > 0 && cfg.has_bazarr() && !dry_run {
        let bazarr = BazarrClient::new(&cfg.bazarr);
        println!(
            "\n   📝 Notifying Bazarr about {} repaired files...",
            repaired_count
        );
        info!("Bazarr integration ready — needs Sonarr/Radarr IDs in future");
        let _ = bazarr;
    }

    let affected_paths = collect_repair_affected_paths(&results);
    if !dry_run && (repaired_count > 0 || stale_count > 0) && !affected_paths.is_empty() {
        let servers = configured_refresh_backends(cfg);
        if emit_text && !servers.is_empty() {
            println!(
                "   📺 Post-repair: refreshing affected library roots in {}...",
                display_server_list(&servers)
            );
        }
        if let Err(err) =
            invalidate_after_mutation(cfg, &selected, &affected_paths, emit_text).await
        {
            if emit_text {
                println!("   ⚠️  Post-repair media-server refresh failed: {}", err);
            }
            tracing::warn!("Post-repair media-server refresh failed: {}", err);
        }
    }

    Ok(results)
}

pub(crate) fn summarize_repair_results(
    results: &[repair::RepairResult],
) -> (usize, usize, usize, usize) {
    let repaired = results
        .iter()
        .filter(|r| matches!(r, repair::RepairResult::Repaired { .. }))
        .count();
    let failed = results
        .iter()
        .filter(|r| matches!(r, repair::RepairResult::Unrepairable { .. }))
        .count();
    let skipped = results
        .iter()
        .filter(|r| matches!(r, repair::RepairResult::Skipped { .. }))
        .count();
    let stale = results
        .iter()
        .filter(|r| matches!(r, repair::RepairResult::Stale { .. }))
        .count();
    (repaired, failed, skipped, stale)
}

pub(crate) fn collect_repair_affected_paths(
    results: &[repair::RepairResult],
) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();

    for result in results {
        match result {
            repair::RepairResult::Repaired { dead_link, .. }
            | repair::RepairResult::Stale { dead_link, .. } => {
                paths.push(dead_link.symlink_path.clone());
            }
            repair::RepairResult::Unrepairable { .. } | repair::RepairResult::Skipped { .. } => {}
        }
    }

    paths.sort();
    paths.dedup();
    paths
}

fn build_repair_self_heal_query(dead_link: &repair::DeadLink) -> Option<String> {
    let mut query = dead_link.meta.title.trim().to_string();
    if query.is_empty() {
        query = dead_link.media_id.clone();
    }

    match dead_link.media_type {
        MediaType::Tv => match (dead_link.meta.season, dead_link.meta.episode) {
            (Some(season), Some(episode)) => {
                query.push_str(&format!(" S{:02}E{:02}", season, episode));
            }
            (Some(season), None) => {
                query.push_str(&format!(" S{:02}", season));
            }
            _ => {}
        },
        MediaType::Movie => {
            if let Some(year) = dead_link.meta.year {
                query.push_str(&format!(" {}", year));
            }
        }
    }

    is_safe_auto_acquire_query(&query).then_some(query)
}

fn library_name_for_path(cfg: &Config, path: &std::path::Path) -> Option<String> {
    cfg.libraries
        .iter()
        .find(|lib| path.starts_with(&lib.path))
        .map(|lib| lib.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ContentType;
    use crate::repair::{DeadLink, TrashMeta};

    fn dead_link(path: &str) -> DeadLink {
        DeadLink {
            symlink_path: std::path::PathBuf::from(path),
            original_source: std::path::PathBuf::from("/mnt/rd/source.mkv"),
            media_id: "tvdb-1".to_string(),
            media_type: MediaType::Tv,
            content_type: ContentType::Tv,
            meta: TrashMeta {
                title: "Show".to_string(),
                year: Some(2024),
                season: Some(1),
                episode: Some(1),
                quality: Some("1080p".to_string()),
                imdb_id: None,
            },
            original_size: None,
        }
    }

    #[test]
    fn collect_repair_affected_paths_returns_only_mutated_targets() {
        let repaired = repair::RepairResult::Repaired {
            dead_link: dead_link("/library/Show - S01E01.mkv"),
            replacement: std::path::PathBuf::from("/mnt/rd/replacement.mkv"),
        };
        let stale = repair::RepairResult::Stale {
            dead_link: dead_link("/library/Show - S01E02.mkv"),
            reason: "missing on disk".to_string(),
        };
        let unrepairable = repair::RepairResult::Unrepairable {
            dead_link: dead_link("/library/Show - S01E03.mkv"),
            reason: "no candidate".to_string(),
        };

        let affected = collect_repair_affected_paths(&[unrepairable, repaired, stale]);
        assert_eq!(
            affected,
            vec![
                std::path::PathBuf::from("/library/Show - S01E01.mkv"),
                std::path::PathBuf::from("/library/Show - S01E02.mkv"),
            ]
        );
    }
}

/// "Title (2014) (2014).mkv" -> "Title (2014).mkv" when the same year is doubled.
pub(crate) fn doubled_year_canonical_name(file_name: &str) -> Option<String> {
    fn strip_trailing_year(s: &str) -> Option<(&str, &str)> {
        let s = s.trim_end();
        let open = s.rfind(" (")?;
        let year = s[open + 2..].strip_suffix(')')?;
        (year.len() == 4 && year.chars().all(|c| c.is_ascii_digit())).then_some((&s[..open], year))
    }
    let (stem, ext) = file_name.rsplit_once('.')?;
    let (once, last_year) = strip_trailing_year(stem)?;
    let (_, first_year) = strip_trailing_year(once)?;
    (first_year == last_year).then(|| format!("{once}.{ext}"))
}

/// Fix movie links whose filename doubled the year (an old folder-title fallback bug).
/// Preview by default; with `apply`, renames the symlink in place (same directory, atomic)
/// and moves the database record. A canonical file that already serves the same source
/// makes the doubled link a duplicate, which is removed; a canonical file serving a
/// different source is left alone and reported. A record whose doubled symlink is gone
/// but whose canonical file exists is simply re-pointed (reconciled).
pub(crate) async fn normalize_movie_names(
    db: &Database,
    library_roots: &[std::path::PathBuf],
    apply: bool,
) -> Result<()> {
    let mut planned = 0usize;
    let mut counts = std::collections::BTreeMap::<&'static str, usize>::new();
    for link in db.get_active_links().await? {
        if link.media_type != crate::models::MediaType::Movie {
            continue;
        }
        if !library_roots
            .iter()
            .any(|root| link.target_path.starts_with(root))
        {
            continue;
        }
        let Some(name) = link.target_path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(canonical) = doubled_year_canonical_name(name) else {
            continue;
        };
        let new_path = link.target_path.with_file_name(&canonical);
        planned += 1;
        let old_exists = link.target_path.symlink_metadata().is_ok();
        let new_exists = new_path.symlink_metadata().is_ok();
        let action = match (old_exists, new_exists) {
            (true, false) => "rename",
            (true, true) => match std::fs::read_link(&new_path) {
                Ok(raw)
                    if crate::utils::resolve_link_target(&new_path, &raw) == link.source_path =>
                {
                    "remove-duplicate"
                }
                _ => "skip-conflict",
            },
            (false, true) => "reconcile-record",
            (false, false) => "skip-missing",
        };
        *counts.entry(action).or_default() += 1;
        if !apply {
            if planned <= 25 {
                println!("   {action:<17} {name}  ->  {canonical}");
            }
            continue;
        }
        match action {
            "rename" => {
                std::fs::rename(&link.target_path, &new_path)?;
                db.update_link_target_path(&link.target_path, &new_path)
                    .await?;
            }
            "remove-duplicate" => {
                std::fs::remove_file(&link.target_path)?;
                db.mark_removed_path(&link.target_path).await?;
            }
            "reconcile-record" => {
                db.update_link_target_path(&link.target_path, &new_path)
                    .await?;
            }
            _ => {}
        }
    }
    if planned == 0 {
        println!("✅ No movie links with a doubled year found.");
        return Ok(());
    }
    if !apply && planned > 25 {
        println!("   … {} more", planned - 25);
    }
    println!(
        "{} {} link(s) with a doubled year: {}",
        if apply {
            "✅ Processed"
        } else {
            "🔍 Preview:"
        },
        planned,
        counts
            .iter()
            .map(|(action, n)| format!("{action}={n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if !apply {
        println!("   Re-run with --apply to rename.");
    }
    Ok(())
}

#[cfg(test)]
mod normalize_names_tests {
    use super::*;

    #[test]
    fn doubled_year_is_detected_only_when_years_match() {
        assert_eq!(
            doubled_year_canonical_name("0.5 mm (2014) (2014).mkv").as_deref(),
            Some("0.5 mm (2014).mkv")
        );
        assert_eq!(
            doubled_year_canonical_name("A Nightmare on Elm Street (1984) (2010).mkv"),
            None
        );
        assert_eq!(doubled_year_canonical_name("Movie (2014).mkv"), None);
        assert_eq!(doubled_year_canonical_name("Movie (2014) (abcd).mkv"), None);
    }

    #[tokio::test]
    async fn normalize_renames_reconciles_and_removes_duplicates() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("film");
        let src = dir.path().join("rd");
        std::fs::create_dir_all(&src).unwrap();
        let db = Database::new(dir.path().join("t.db").to_str().unwrap())
            .await
            .unwrap();
        let mk =
            |folder: &str, name: &str, source: &str| -> (std::path::PathBuf, std::path::PathBuf) {
                let d = root.join(folder);
                std::fs::create_dir_all(&d).unwrap();
                let s = src.join(source);
                std::fs::write(&s, b"x").unwrap();
                let t = d.join(name);
                std::os::unix::fs::symlink(&s, &t).unwrap();
                (t, s)
            };
        let record =
            |t: &std::path::Path, s: &std::path::Path, id: u32| crate::models::LinkRecord {
                id: None,
                source_path: s.to_path_buf(),
                target_path: t.to_path_buf(),
                media_id: format!("tmdb-{id}"),
                media_type: crate::models::MediaType::Movie,
                status: crate::models::LinkStatus::Active,
                created_at: None,
                updated_at: None,
            };

        // 1. plain rename
        let (t1, s1) = mk("A (2014) {tmdb-1}", "A (2014) (2014).mkv", "a.mkv");
        db.insert_link(&record(&t1, &s1, 1)).await.unwrap();
        // 2. duplicate: canonical already serves the same source
        let (t2, s2) = mk("B (2015) {tmdb-2}", "B (2015) (2015).mkv", "b.mkv");
        std::os::unix::fs::symlink(&s2, root.join("B (2015) {tmdb-2}").join("B (2015).mkv"))
            .unwrap();
        db.insert_link(&record(&t2, &s2, 2)).await.unwrap();
        // 3. reconcile: symlink already renamed on disk, record still doubled
        let (t3, s3) = mk("C (2016) {tmdb-3}", "C (2016) (2016).mkv", "c.mkv");
        std::fs::rename(&t3, root.join("C (2016) {tmdb-3}").join("C (2016).mkv")).unwrap();
        db.insert_link(&record(&t3, &s3, 3)).await.unwrap();

        normalize_movie_names(&db, std::slice::from_ref(&root), false)
            .await
            .unwrap();
        assert!(t1.symlink_metadata().is_ok(), "preview must not touch disk");

        normalize_movie_names(&db, std::slice::from_ref(&root), true)
            .await
            .unwrap();
        let a_new = root.join("A (2014) {tmdb-1}").join("A (2014).mkv");
        assert!(a_new.symlink_metadata().is_ok() && t1.symlink_metadata().is_err());
        assert_eq!(
            db.get_link_by_target_path(&a_new)
                .await
                .unwrap()
                .unwrap()
                .source_path,
            s1
        );
        assert!(t2.symlink_metadata().is_err(), "duplicate removed");
        assert_eq!(
            db.get_link_by_target_path(&t2)
                .await
                .unwrap()
                .unwrap()
                .status,
            crate::models::LinkStatus::Removed
        );
        let c_new = root.join("C (2016) {tmdb-3}").join("C (2016).mkv");
        assert_eq!(
            db.get_link_by_target_path(&c_new)
                .await
                .unwrap()
                .unwrap()
                .source_path,
            s3
        );
        assert!(db.get_link_by_target_path(&t3).await.unwrap().is_none());
    }
}

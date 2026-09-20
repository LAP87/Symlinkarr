use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::commands::{panel_border, panel_kv_row, panel_title};
use crate::config::{Config, LibraryConfig};
use crate::db::Database;
use crate::library_scanner::LibraryScanner;
use crate::models::MediaType;
use crate::OutputFormat;

#[allow(unused_imports)]
pub(crate) use self::anime::AnimeRemediationReportOutput;
use self::anime::{build_anime_duplicate_audit, AnimeDuplicateAuditOutput};
pub(crate) use self::anime::{
    build_anime_remediation_report, AnimeRemediationSample, AnimeRootUsageSample,
};
#[cfg(test)]
use self::anime::{
    build_anime_remediation_samples, collect_anime_root_usage, correlate_anime_duplicate_groups,
    summarize_plex_duplicate_show_records, AnimeRootUsage, CorrelatedAnimeDuplicateSample,
    PlexDuplicateShowSample,
};
use self::path_compare::{build_path_compare, PathCompareOutput};
#[cfg(test)]
use self::path_compare::{
    sample_difference, sample_intersection, symlink_source_missing, PATH_SAMPLE_LIMIT,
};

#[derive(Serialize, Debug, Default, PartialEq, Eq)]
struct Summary {
    total_library_items: i64,
    items_with_symlinks: i64,
    broken_symlinks: i64,
    missing_from_rd: i64,
}

#[derive(Serialize, Debug, Default, PartialEq, Eq)]
struct MediaTypeInfo {
    library_items: i64,
    linked: i64,
    broken: i64,
}

#[derive(Serialize, Debug, Default, Clone, PartialEq, Eq)]
struct LibraryInfo {
    name: String,
    items: i64,
    linked: i64,
    broken: i64,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
struct ReportOutput {
    generated_at: String,
    summary: Summary,
    by_media_type: BTreeMap<String, MediaTypeInfo>,
    top_libraries: Vec<LibraryInfo>,
    path_compare: PathCompareOutput,
    provider_repair: ProviderRepairOutput,
    #[serde(skip_serializing_if = "Option::is_none")]
    anime_duplicates: Option<AnimeDuplicateAuditOutput>,
}

#[derive(Serialize, Debug, Default, PartialEq, Eq)]
struct ProviderRepairOutput {
    candidates: i64,
    sample: Vec<ProviderRepairSample>,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
struct ProviderRepairSample {
    last_seen: String,
    latest_action: String,
    reason: String,
    occurrences: i64,
    source_path: Option<String>,
    media_id: Option<String>,
    sample_targets: Vec<String>,
}

struct LibraryScannerItem {
    library_name: String,
    media_type: MediaType,
    media_id: String,
}

#[derive(Default)]
struct LinkPresence {
    active_media_ids: HashSet<String>,
    dead_media_ids: HashSet<String>,
}

pub(crate) struct ReportOptions<'a> {
    pub(crate) output_format: OutputFormat,
    pub(crate) filter: Option<MediaType>,
    pub(crate) library_filter: Option<&'a str>,
    pub(crate) plex_db_path: Option<&'a Path>,
    pub(crate) full_anime_duplicates: bool,
    pub(crate) anime_remediation_tsv_path: Option<&'a Path>,
    pub(crate) pretty: bool,
    /// Emit the RD torrents backing active links as JSON and nothing else.
    pub(crate) linked_torrents: bool,
    /// Emit unlinked / empty library items report.
    pub(crate) unlinked_items: bool,
}

pub(crate) async fn run_report(
    cfg: &Config,
    db: &Database,
    options: ReportOptions<'_>,
) -> Result<()> {
    if options.linked_torrents {
        return run_linked_torrents_report(cfg, db, options.pretty).await;
    }
    if options.unlinked_items {
        return run_unlinked_items_report(
            cfg,
            db,
            options.filter,
            options.library_filter,
            options.output_format,
            options.pretty,
        )
        .await;
    }
    let effective_full_anime_duplicates =
        options.full_anime_duplicates || options.anime_remediation_tsv_path.is_some();
    let report = build_report(
        cfg,
        db,
        options.filter,
        options.library_filter,
        options.plex_db_path,
        effective_full_anime_duplicates,
    )
    .await?;

    if let Some(tsv_path) = options.anime_remediation_tsv_path {
        write_anime_remediation_tsv(tsv_path, &report)?;
    }

    match options.output_format {
        OutputFormat::Json => {
            if options.pretty {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .unwrap_or_else(|e| format!(r#"{{"error":"{}"}}"#, e))
                );
            } else {
                println!("{}", serde_json::to_string(&report).unwrap_or_default());
            }
        }
        OutputFormat::Text => emit_text_report(&report, options.anime_remediation_tsv_path),
    }

    Ok(())
}

async fn build_report(
    cfg: &Config,
    db: &Database,
    filter: Option<MediaType>,
    library_filter: Option<&str>,
    plex_db_path: Option<&Path>,
    full_anime_duplicates: bool,
) -> Result<ReportOutput> {
    let selected_libraries = selected_report_libraries(cfg, filter, library_filter);
    let generated_at = Utc::now().to_rfc3339();

    if selected_libraries.is_empty() {
        return Ok(ReportOutput {
            generated_at,
            summary: Summary::default(),
            by_media_type: BTreeMap::new(),
            top_libraries: Vec::new(),
            path_compare: PathCompareOutput::default(),
            provider_repair: ProviderRepairOutput::default(),
            anime_duplicates: None,
        });
    }

    let scanner = LibraryScanner::new();
    let mut by_library: HashMap<String, LibraryInfo> = selected_libraries
        .iter()
        .map(|lib| {
            (
                lib.name.clone(),
                LibraryInfo {
                    name: lib.name.clone(),
                    ..LibraryInfo::default()
                },
            )
        })
        .collect();
    let mut by_media_type: BTreeMap<String, MediaTypeInfo> = BTreeMap::new();

    let selected_roots: Vec<_> = selected_libraries
        .iter()
        .map(|lib| lib.path.clone())
        .collect();

    // Run DB query and library scan in parallel using tokio::spawn
    let db_handle = tokio::spawn({
        let db = db.clone();
        let roots = selected_roots.clone();
        async move { db.get_links_scoped(Some(&roots)).await }
    });

    // Library scan is CPU-bound with file I/O, run in parallel with DB query
    let all_library_items: Vec<Vec<LibraryScannerItem>> = selected_libraries
        .par_iter()
        .map(|lib| {
            scanner
                .scan_library(lib)
                .into_iter()
                .map(|item| LibraryScannerItem {
                    library_name: item.library_name,
                    media_type: item.media_type,
                    media_id: item.id.to_string(),
                })
                .collect()
        })
        .collect();

    // Await DB results after library scan has started
    let link_records = db_handle.await??;
    let link_presence = collect_link_presence(&selected_libraries, &link_records);

    let mut summary = Summary::default();
    for library_items in &all_library_items {
        for item in library_items {
            let media_key = media_type_key(item.media_type).to_string();
            let (has_active, has_dead) = link_presence
                .get(&item.library_name)
                .map(|presence| {
                    (
                        presence.active_media_ids.contains(&item.media_id),
                        presence.dead_media_ids.contains(&item.media_id),
                    )
                })
                .unwrap_or((false, false));

            summary.total_library_items += 1;
            by_media_type.entry(media_key).or_default().library_items += 1;
            if let Some(entry) = by_library.get_mut(&item.library_name) {
                entry.items += 1;
            }

            if has_active {
                summary.items_with_symlinks += 1;
                if let Some(entry) = by_library.get_mut(&item.library_name) {
                    entry.linked += 1;
                }
                if let Some(entry) = by_media_type.get_mut(media_type_key(item.media_type)) {
                    entry.linked += 1;
                }
            }

            if has_dead {
                summary.broken_symlinks += 1;
                if let Some(entry) = by_library.get_mut(&item.library_name) {
                    entry.broken += 1;
                }
                if let Some(entry) = by_media_type.get_mut(media_type_key(item.media_type)) {
                    entry.broken += 1;
                }
            }
        }
    }

    summary.missing_from_rd = summary
        .total_library_items
        .saturating_sub(summary.items_with_symlinks);

    let mut top_libraries: Vec<_> = by_library
        .into_values()
        .filter(|lib| lib.items > 0)
        .collect();
    top_libraries.sort_by(|a, b| b.items.cmp(&a.items).then_with(|| a.name.cmp(&b.name)));
    top_libraries.truncate(10);

    let anime_duplicates = build_anime_duplicate_audit(
        &selected_libraries,
        &link_records,
        plex_db_path,
        full_anime_duplicates,
    )
    .await?;
    let source_roots: Vec<_> = cfg.sources.iter().map(|src| src.path.clone()).collect();
    let path_compare = build_path_compare(
        &selected_libraries,
        &selected_roots,
        &link_records,
        plex_db_path,
        &source_roots,
    )
    .await?;
    let provider_repair = build_provider_repair_output(db, Some(&selected_roots)).await?;

    Ok(ReportOutput {
        generated_at,
        summary,
        by_media_type,
        top_libraries,
        path_compare,
        provider_repair,
        anime_duplicates,
    })
}

async fn build_provider_repair_output(
    db: &Database,
    target_roots: Option<&[std::path::PathBuf]>,
) -> Result<ProviderRepairOutput> {
    let candidates = db.get_provider_repair_candidates(target_roots, 10).await?;
    Ok(ProviderRepairOutput {
        candidates: candidates.len() as i64,
        sample: candidates
            .into_iter()
            .map(|candidate| ProviderRepairSample {
                last_seen: candidate.last_seen,
                latest_action: candidate.latest_action,
                reason: candidate.reason,
                occurrences: candidate.occurrences,
                source_path: candidate.source_path.map(|path| path.display().to_string()),
                media_id: candidate.media_id,
                sample_targets: candidate
                    .sample_targets
                    .into_iter()
                    .map(|path| path.display().to_string())
                    .collect(),
            })
            .collect(),
    })
}

fn selected_report_libraries<'a>(
    cfg: &'a Config,
    filter: Option<MediaType>,
    library_filter: Option<&str>,
) -> Vec<&'a LibraryConfig> {
    cfg.libraries
        .iter()
        .filter(|lib| filter.is_none_or(|media_type| lib.media_type == media_type))
        .filter(|lib| {
            library_filter.is_none_or(|library_name| lib.name.eq_ignore_ascii_case(library_name))
        })
        .collect()
}

fn collect_link_presence(
    libraries: &[&LibraryConfig],
    link_records: &[crate::models::LinkRecord],
) -> HashMap<String, LinkPresence> {
    let mut presence_by_library: HashMap<String, LinkPresence> = HashMap::new();
    // Pre-fill with all libraries so we get consistent output ordering
    for lib in libraries {
        presence_by_library.entry(lib.name.clone()).or_default();
    }
    for link in link_records {
        // Find the most-specific matching library (longest path prefix)
        let library_name = libraries
            .iter()
            .filter(|lib| link.target_path.starts_with(&lib.path))
            .max_by_key(|lib| lib.path.components().count())
            .map(|lib| lib.name.clone());

        if let Some(name) = library_name {
            let entry = presence_by_library.entry(name).or_default();
            match link.status {
                crate::models::LinkStatus::Active => {
                    entry.active_media_ids.insert(link.media_id.clone());
                }
                crate::models::LinkStatus::Dead => {
                    entry.dead_media_ids.insert(link.media_id.clone());
                }
                crate::models::LinkStatus::Removed => {}
            }
        }
    }
    presence_by_library
}

fn media_type_key(media_type: MediaType) -> &'static str {
    match media_type {
        MediaType::Movie => "movie",
        MediaType::Tv => "series",
    }
}

fn write_anime_remediation_tsv(path: &Path, report: &ReportOutput) -> Result<()> {
    let Some(anime_duplicates) = &report.anime_duplicates else {
        anyhow::bail!("Anime remediation TSV export requires an anime library selection");
    };
    let Some(samples) = &anime_duplicates.remediation_sample_groups else {
        anyhow::bail!(
            "Anime remediation TSV export requires --plex-db so correlated groups can be resolved"
        );
    };

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let mut out = String::from(
        "normalized_title\tlegacy_root_paths\tlegacy_filesystem_symlinks\tlegacy_db_active_links\trecommended_tagged_root\trecommended_tagged_root_fs\trecommended_tagged_root_db\tplex_live_rows\tplex_deleted_rows\tplex_guid_kinds\tplex_guids\n",
    );

    for sample in samples {
        let legacy_paths = sample
            .legacy_roots
            .iter()
            .map(|root| root.path.display().to_string())
            .collect::<Vec<_>>()
            .join(" | ");
        let legacy_fs: usize = sample
            .legacy_roots
            .iter()
            .map(|root| root.filesystem_symlinks)
            .sum();
        let legacy_db: usize = sample
            .legacy_roots
            .iter()
            .map(|root| root.db_active_links)
            .sum();
        let guid_kinds = sample.plex_guid_kinds.join(" | ");
        let guids = sample.plex_guids.join(" | ");

        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            sample.normalized_title.replace('\t', " "),
            legacy_paths.replace('\t', " "),
            legacy_fs,
            legacy_db,
            sample
                .recommended_tagged_root
                .path
                .display()
                .to_string()
                .replace('\t', " "),
            sample.recommended_tagged_root.filesystem_symlinks,
            sample.recommended_tagged_root.db_active_links,
            sample.plex_live_rows,
            sample.plex_deleted_rows,
            guid_kinds.replace('\t', " "),
            guids.replace('\t', " "),
        ));
    }

    std::fs::write(path, out)?;
    Ok(())
}

fn emit_text_report(report: &ReportOutput, anime_remediation_tsv_path: Option<&Path>) {
    println!();
    panel_border('╔', '═', '╗');
    panel_title("Symlinkarr Report");
    panel_border('╠', '═', '╣');
    panel_kv_row("  Total library items:", report.summary.total_library_items);
    panel_kv_row("  Items with symlinks:", report.summary.items_with_symlinks);
    panel_kv_row("  Items with dead links:", report.summary.broken_symlinks);
    panel_kv_row("  Missing from RD:", report.summary.missing_from_rd);

    if !report.by_media_type.is_empty() {
        panel_border('╠', '═', '╣');
        panel_title("By Media Type");
        panel_border('╠', '═', '╣');
        for (media_type, info) in &report.by_media_type {
            let label = format!("  {}:", capitalize(media_type));
            let value = format!(
                "{} items ({} linked, {} broken)",
                info.library_items, info.linked, info.broken
            );
            panel_kv_row(&label, value);
        }
    }

    if !report.top_libraries.is_empty() {
        panel_border('╠', '═', '╣');
        panel_title("Top Libraries");
        panel_border('╠', '═', '╣');
        for lib in &report.top_libraries {
            let label = format!("  {}:", lib.name);
            panel_kv_row(
                &label,
                format!(
                    "{} items ({} linked, {} broken)",
                    lib.items, lib.linked, lib.broken
                ),
            );
        }
    }

    panel_border('╠', '═', '╣');
    panel_title("Path Compare");
    panel_border('╠', '═', '╣');
    panel_kv_row(
        "  Filesystem symlinks:",
        report.path_compare.filesystem_symlinks,
    );
    panel_kv_row("  DB active links:", report.path_compare.db_active_links);
    if let Some(plex_count) = report.path_compare.plex_indexed_files {
        panel_kv_row("  Plex indexed files:", plex_count);
    }
    if let Some(plex_deleted) = report.path_compare.plex_deleted_paths {
        panel_kv_row("  Plex deleted-only paths:", plex_deleted);
    }
    panel_kv_row("  FS not in DB:", report.path_compare.fs_not_in_db.count);
    panel_kv_row("  DB not on FS:", report.path_compare.db_not_on_fs.count);
    if let Some(sample) = &report.path_compare.fs_not_in_plex {
        panel_kv_row("  FS not in Plex:", sample.count);
    }
    if let Some(sample) = &report.path_compare.db_not_in_plex {
        panel_kv_row("  DB not in Plex:", sample.count);
    }
    if let Some(sample) = &report.path_compare.plex_not_on_fs {
        panel_kv_row("  Plex not on FS:", sample.count);
    }
    if let Some(sample) = &report.path_compare.plex_deleted_and_known_missing_source {
        panel_kv_row("  Plex del + src missing:", sample.count);
    }
    if let Some(sample) = &report
        .path_compare
        .plex_deleted_without_known_missing_source
    {
        panel_kv_row("  Plex del, src intact:", sample.count);
    }
    if let Some(all_three) = report.path_compare.all_three {
        panel_kv_row("  In all three:", all_three);
    }
    if let Some(sample) = &report.path_compare.unreachable_sources {
        panel_kv_row("  Unreachable sources:", sample.count);
    }

    panel_border('╠', '═', '╣');
    panel_title("Provider Repair");
    panel_border('╠', '═', '╣');
    panel_kv_row("  Sampled candidates:", report.provider_repair.candidates);
    if !report.provider_repair.sample.is_empty() {
        println!("  Sample provider/source issues:");
        for sample in &report.provider_repair.sample {
            println!(
                "    - {} [{}x, {}]",
                sample.reason, sample.occurrences, sample.last_seen
            );
            if let Some(media_id) = &sample.media_id {
                println!("      media: {}", media_id);
            }
            if let Some(source_path) = &sample.source_path {
                println!("      source: {}", source_path);
            }
            if let Some(target_path) = sample.sample_targets.first() {
                println!("      target: {}", target_path);
            }
        }
    }

    if let Some(anime_duplicates) = &report.anime_duplicates {
        panel_border('╠', '═', '╣');
        panel_title("Anime Duplicates");
        panel_border('╠', '═', '╣');
        panel_kv_row(
            "  Mixed roots:",
            anime_duplicates.filesystem_mixed_root_groups,
        );
        if let Some(groups) = anime_duplicates.plex_duplicate_show_groups {
            panel_kv_row("  Plex dup groups:", groups);
        }
        if let Some(groups) = anime_duplicates.plex_hama_anidb_tvdb_groups {
            panel_kv_row("  HAMA split groups:", groups);
        }
        if let Some(groups) = anime_duplicates.plex_other_duplicate_show_groups {
            panel_kv_row("  Other dup groups:", groups);
        }
        if let Some(groups) = anime_duplicates.correlated_hama_split_groups {
            panel_kv_row("  Correlated HAMA+FS:", groups);
        }
        if let Some(groups) = anime_duplicates.remediation_groups {
            panel_kv_row("  Remediation groups:", groups);
        }

        if !anime_duplicates.filesystem_sample_groups.is_empty() {
            println!("  Sample mixed roots:");
            for sample in &anime_duplicates.filesystem_sample_groups {
                println!("    - {}", sample.normalized_title);
                if let Some(path) = sample.untagged_roots.first() {
                    println!("      legacy: {}", path.display());
                }
                if let Some(path) = sample.tagged_roots.first() {
                    println!("      tagged: {}", path.display());
                }
            }
        }

        if let Some(samples) = &anime_duplicates.plex_sample_groups {
            if !samples.is_empty() {
                println!("  Sample Plex duplicate groups:");
                for sample in samples {
                    let year = sample
                        .year
                        .map(|year| format!(" ({year})"))
                        .unwrap_or_default();
                    let guid_kinds = sample.guid_kinds.join(", ");
                    println!(
                        "    - {}{} [{} total, {} live, {} deleted] <{}>",
                        sample.title,
                        year,
                        sample.total_rows,
                        sample.live_rows,
                        sample.deleted_rows,
                        guid_kinds
                    );
                }
            }
        }

        if let Some(samples) = &anime_duplicates.correlated_sample_groups {
            if !samples.is_empty() {
                println!("  Sample correlated duplicate groups:");
                for sample in samples {
                    let guid_kinds = sample.plex_guid_kinds.join(", ");
                    println!(
                        "    - {} [{} total, {} live, {} deleted] <{}>",
                        sample.normalized_title,
                        sample.plex_total_rows,
                        sample.plex_live_rows,
                        sample.plex_deleted_rows,
                        guid_kinds
                    );
                    if let Some(path) = sample.untagged_roots.first() {
                        println!("      legacy: {}", path.display());
                    }
                    if let Some(path) = sample.tagged_roots.first() {
                        println!("      tagged: {}", path.display());
                    }
                }
            }
        }

        if let Some(samples) = &anime_duplicates.remediation_sample_groups {
            if !samples.is_empty() {
                println!("  Sample remediation plan:");
                for sample in samples {
                    let legacy_fs: usize = sample
                        .legacy_roots
                        .iter()
                        .map(|root| root.filesystem_symlinks)
                        .sum();
                    let legacy_db: usize = sample
                        .legacy_roots
                        .iter()
                        .map(|root| root.db_active_links)
                        .sum();
                    println!(
                        "    - {} [legacy fs={}, legacy db={}]",
                        sample.normalized_title, legacy_fs, legacy_db
                    );
                    println!(
                        "      keep: {} (fs={}, db={})",
                        sample.recommended_tagged_root.path.display(),
                        sample.recommended_tagged_root.filesystem_symlinks,
                        sample.recommended_tagged_root.db_active_links
                    );
                    if let Some(root) = sample.legacy_roots.first() {
                        println!(
                            "      legacy: {} (fs={}, db={})",
                            root.path.display(),
                            root.filesystem_symlinks,
                            root.db_active_links
                        );
                    }
                    if !sample.alternate_tagged_roots.is_empty() {
                        println!(
                            "      alt tagged roots: {}",
                            sample.alternate_tagged_roots.len()
                        );
                    }
                }
            }
        }
    }

    panel_border('╚', '═', '╝');
    if let Some(path) = anime_remediation_tsv_path {
        println!("  Anime remediation TSV: {}", path.display());
    }
    println!();
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().chain(c).collect(),
    }
}

mod anime;
mod path_compare;
#[cfg(test)]
mod tests;

/// The first path component of `path` under whichever configured source root contains it:
/// the torrent folder on the mount.
pub(crate) fn source_folder(cfg: &Config, path: &std::path::Path) -> Option<String> {
    cfg.sources.iter().find_map(|source| {
        path.strip_prefix(&source.path).ok().and_then(|rel| {
            rel.components()
                .next()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
        })
    })
}

/// Join active-link source folders to the cached RD torrent index. A folder is the
/// torrent's mount folder when the cache knows it (original name), else the RD filename
/// (multi-file torrents) or the filename without its extension (single files).
pub(crate) fn aggregate_linked_torrents(
    index: &[(String, String, String, String)],
    folders: impl IntoIterator<Item = String>,
) -> (Vec<serde_json::Value>, u64) {
    let mount_folders: Vec<String> = index
        .iter()
        .map(|(_, _, filename, files_json)| {
            crate::cache::cached_mount_folder_name(filename, files_json)
        })
        .collect();
    let mut by_folder: std::collections::HashMap<&str, (&str, &str)> =
        std::collections::HashMap::new();
    for ((torrent_id, hash, filename, _), mount_folder) in index.iter().zip(&mount_folders) {
        by_folder.insert(mount_folder.as_str(), (torrent_id.as_str(), hash.as_str()));
        by_folder
            .entry(filename.as_str())
            .or_insert((torrent_id.as_str(), hash.as_str()));
        if let Some((stem, _)) = filename.rsplit_once('.') {
            by_folder
                .entry(stem)
                .or_insert((torrent_id.as_str(), hash.as_str()));
        }
    }
    let mut counts: std::collections::BTreeMap<String, (&str, &str, u64)> =
        std::collections::BTreeMap::new();
    let mut unmatched = 0u64;
    for folder in folders {
        match by_folder.get(folder.as_str()) {
            Some((id, hash)) => counts.entry(folder).or_insert((id, hash, 0)).2 += 1,
            None => unmatched += 1,
        }
    }
    let rows = counts
        .into_iter()
        .map(|(folder, (rd_id, hash, active_links))| {
            serde_json::json!({
                "rd_id": rd_id,
                "hash": hash,
                "folder": folder,
                "active_links": active_links,
            })
        })
        .collect();
    (rows, unmatched)
}

/// Build the report document for which RD torrents currently back active symlinks.
pub(crate) async fn build_linked_torrents_report(
    cfg: &Config,
    db: &Database,
) -> Result<serde_json::Value> {
    let index = db.get_rd_torrent_index().await?;
    let folders = db
        .get_active_links()
        .await?
        .into_iter()
        .filter_map(|link| source_folder(cfg, &link.source_path));
    let (torrents, unmatched_links) = aggregate_linked_torrents(&index, folders);
    Ok(serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "torrent_count": torrents.len(),
        "unmatched_links": unmatched_links,
        "torrents": torrents,
    }))
}

/// `report --linked-torrents`: which RD torrents currently back active symlinks, for a
/// keeper (e.g. backfill-buddy) to protect first.
pub(crate) async fn run_linked_torrents_report(
    cfg: &Config,
    db: &Database,
    pretty: bool,
) -> Result<()> {
    let doc = build_linked_torrents_report(cfg, db).await?;
    let text = if pretty {
        serde_json::to_string_pretty(&doc)?
    } else {
        serde_json::to_string(&doc)?
    };
    println!("{text}");
    Ok(())
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct UnlinkedLibraryItem {
    pub library_name: String,
    pub title: String,
    pub media_type: MediaType,
    pub tvdb_id: Option<u64>,
    pub tmdb_id: Option<u64>,
    pub folder_path: String,
    pub season_count: Option<usize>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct UnlinkedLibraryItemsReport {
    pub generated_at: String,
    pub total_unlinked: usize,
    pub items: Vec<UnlinkedLibraryItem>,
}

pub(crate) fn is_season_dir_name(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    if lower.starts_with("special") {
        return true;
    }
    if let Some(rest) = lower
        .strip_prefix("season")
        .or_else(|| lower.strip_prefix("staffel"))
        .or_else(|| lower.strip_prefix("saison"))
    {
        return rest.trim().parse::<u32>().is_ok();
    }
    if let Some(rest) = lower.strip_prefix('s') {
        if rest.trim().parse::<u32>().is_ok() {
            return true;
        }
    }
    false
}

pub(crate) fn count_season_folders(folder_path: &Path) -> usize {
    let Ok(read_dir) = std::fs::read_dir(folder_path) else {
        return 0;
    };
    read_dir
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.path().is_dir() && is_season_dir_name(&entry.file_name().to_string_lossy())
        })
        .count()
}

pub(crate) async fn build_unlinked_items_report(
    cfg: &Config,
    db: &Database,
    filter: Option<MediaType>,
    library_filter: Option<&str>,
) -> Result<UnlinkedLibraryItemsReport> {
    let selected_libraries = selected_report_libraries(cfg, filter, library_filter);
    let generated_at = Utc::now().to_rfc3339();

    if selected_libraries.is_empty() {
        return Ok(UnlinkedLibraryItemsReport {
            generated_at,
            total_unlinked: 0,
            items: Vec::new(),
        });
    }

    let selected_roots: Vec<_> = selected_libraries
        .iter()
        .map(|lib| lib.path.clone())
        .collect();

    let active_links = db.get_active_links_scoped(Some(&selected_roots)).await?;
    let link_presence = collect_link_presence(&selected_libraries, &active_links);

    let scanner = LibraryScanner::new();
    let all_library_items: Vec<Vec<crate::models::LibraryItem>> = selected_libraries
        .par_iter()
        .map(|lib| scanner.scan_library(lib))
        .collect();

    let mut unlinked_items = Vec::new();

    for library_items in all_library_items {
        for item in library_items {
            let media_id_str = item.id.to_string();
            let has_active_in_lib = link_presence
                .get(&item.library_name)
                .map(|presence| presence.active_media_ids.contains(&media_id_str))
                .unwrap_or(false);

            if has_active_in_lib {
                continue;
            }

            let has_active_under_path = active_links
                .iter()
                .any(|link| link.target_path.starts_with(&item.path));

            if has_active_under_path {
                continue;
            }

            let (tvdb_id, tmdb_id) = match item.id {
                crate::models::MediaId::Tvdb(id) => (Some(id), None),
                crate::models::MediaId::Tmdb(id) => (None, Some(id)),
            };

            let season_count = if item.media_type == MediaType::Tv {
                Some(count_season_folders(&item.path))
            } else {
                None
            };

            unlinked_items.push(UnlinkedLibraryItem {
                library_name: item.library_name,
                title: item.title,
                media_type: item.media_type,
                tvdb_id,
                tmdb_id,
                folder_path: item.path.display().to_string(),
                season_count,
            });
        }
    }

    unlinked_items.sort_by(|a, b| {
        a.library_name
            .cmp(&b.library_name)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });

    let total_unlinked = unlinked_items.len();

    Ok(UnlinkedLibraryItemsReport {
        generated_at,
        total_unlinked,
        items: unlinked_items,
    })
}

pub(crate) async fn run_unlinked_items_report(
    cfg: &Config,
    db: &Database,
    filter: Option<MediaType>,
    library_filter: Option<&str>,
    output_format: OutputFormat,
    pretty: bool,
) -> Result<()> {
    let report = build_unlinked_items_report(cfg, db, filter, library_filter).await?;
    match output_format {
        OutputFormat::Json => {
            let text = if pretty {
                serde_json::to_string_pretty(&report)?
            } else {
                serde_json::to_string(&report)?
            };
            println!("{text}");
        }
        OutputFormat::Text => {
            emit_unlinked_items_text_report(&report);
        }
    }
    Ok(())
}

fn emit_unlinked_items_text_report(report: &UnlinkedLibraryItemsReport) {
    println!();
    panel_border('╔', '═', '╗');
    panel_title("Unlinked / Empty Library Items");
    panel_border('╠', '═', '╣');
    panel_kv_row("  Total unlinked items:", report.total_unlinked);
    if report.items.is_empty() {
        println!("  No unlinked items found across configured libraries.");
    } else {
        println!("  Unlinked items:");
        for item in &report.items {
            let id_tag = match (item.tvdb_id, item.tmdb_id) {
                (Some(tvdb), _) => format!("tvdb-{}", tvdb),
                (_, Some(tmdb)) => format!("tmdb-{}", tmdb),
                _ => "unknown".to_string(),
            };
            let extra = match item.season_count {
                Some(seasons) => {
                    format!(
                        ", {} season{}",
                        seasons,
                        if seasons == 1 { "" } else { "s" }
                    )
                }
                None => String::new(),
            };
            println!(
                "    - {} [{}] ({}{})",
                item.title, id_tag, item.library_name, extra
            );
            println!("      path: {}", item.folder_path);
        }
    }
    panel_border('╚', '═', '╝');
    println!();
}

#[cfg(test)]
mod linked_torrents_tests {
    use super::*;

    #[test]
    fn folders_join_to_torrents_by_filename_or_stem() {
        let index = vec![
            (
                "RD1".to_string(),
                "aaa".to_string(),
                "Show.S01.Pack".to_string(),
                r#"{"files":[]}"#.to_string(),
            ),
            (
                "RD2".to_string(),
                "bbb".to_string(),
                "Movie.2014.mkv".to_string(),
                r#"{"files":[]}"#.to_string(),
            ),
            (
                "RD3".to_string(),
                "ccc".to_string(),
                "Depraved 2019.mkv".to_string(),
                r#"{"files":[],"mount_folder":"Depraved 2019 UHD-E"}"#.to_string(),
            ),
        ];
        let folders = vec![
            "Show.S01.Pack".to_string(),
            "Show.S01.Pack".to_string(),
            "Movie.2014".to_string(),
            "Depraved 2019 UHD-E".to_string(),
            "Unknown".to_string(),
        ];
        let (rows, unmatched) = aggregate_linked_torrents(&index, folders);
        assert_eq!(unmatched, 1);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["folder"], "Depraved 2019 UHD-E");
        assert_eq!(rows[0]["rd_id"], "RD3");
        assert_eq!(rows[1]["folder"], "Movie.2014");
        assert_eq!(rows[1]["rd_id"], "RD2");
        assert_eq!(rows[2]["active_links"], 2);
        assert_eq!(rows[2]["hash"], "aaa");
    }
}

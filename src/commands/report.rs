use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use rayon::prelude::*;
use serde::Serialize;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    anime_duplicates: Option<AnimeDuplicateAuditOutput>,
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
}

pub(crate) async fn run_report(
    cfg: &Config,
    db: &Database,
    options: ReportOptions<'_>,
) -> Result<()> {
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
    let path_compare = build_path_compare(
        &selected_libraries,
        &selected_roots,
        &link_records,
        plex_db_path,
    )
    .await?;

    Ok(ReportOutput {
        generated_at,
        summary,
        by_media_type,
        top_libraries,
        path_compare,
        anime_duplicates,
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

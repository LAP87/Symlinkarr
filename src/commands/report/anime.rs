use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::anime_roots::collect_anime_root_duplicate_groups;
use crate::config::{Config, ContentType, LibraryConfig};
use crate::db::Database;
use crate::media_servers::plex_db;
use crate::models::LinkStatus;
use crate::utils::normalize;

use super::path_compare::PATH_SAMPLE_LIMIT;

#[derive(Serialize, Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct AnimeRootDuplicateSample {
    pub(super) normalized_title: String,
    pub(super) tagged_roots: Vec<PathBuf>,
    pub(super) untagged_roots: Vec<PathBuf>,
}

#[derive(Serialize, Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct PlexDuplicateShowSample {
    pub(super) title: String,
    pub(super) original_title: String,
    pub(super) year: Option<i64>,
    pub(super) total_rows: usize,
    pub(super) live_rows: usize,
    pub(super) deleted_rows: usize,
    pub(super) guid_kinds: Vec<String>,
    pub(super) guids: Vec<String>,
}

#[derive(Serialize, Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct CorrelatedAnimeDuplicateSample {
    pub(super) normalized_title: String,
    pub(super) tagged_roots: Vec<PathBuf>,
    pub(super) untagged_roots: Vec<PathBuf>,
    pub(super) plex_total_rows: usize,
    pub(super) plex_live_rows: usize,
    pub(super) plex_deleted_rows: usize,
    pub(super) plex_guid_kinds: Vec<String>,
    pub(super) plex_guids: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct AnimeRootUsageSample {
    pub path: PathBuf,
    pub filesystem_symlinks: usize,
    pub db_active_links: usize,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct AnimeRemediationSample {
    pub normalized_title: String,
    pub recommended_tagged_root: AnimeRootUsageSample,
    pub alternate_tagged_roots: Vec<AnimeRootUsageSample>,
    pub legacy_roots: Vec<AnimeRootUsageSample>,
    pub plex_total_rows: usize,
    pub plex_live_rows: usize,
    pub plex_deleted_rows: usize,
    pub plex_guid_kinds: Vec<String>,
    pub plex_guids: Vec<String>,
}

#[derive(Serialize, Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct AnimeDuplicateAuditOutput {
    pub(super) filesystem_mixed_root_groups: usize,
    pub(super) filesystem_sample_groups: Vec<AnimeRootDuplicateSample>,
    pub(super) plex_duplicate_show_groups: Option<usize>,
    pub(super) plex_hama_anidb_tvdb_groups: Option<usize>,
    pub(super) plex_other_duplicate_show_groups: Option<usize>,
    pub(super) plex_sample_groups: Option<Vec<PlexDuplicateShowSample>>,
    pub(super) correlated_hama_split_groups: Option<usize>,
    pub(super) correlated_sample_groups: Option<Vec<CorrelatedAnimeDuplicateSample>>,
    pub(super) remediation_groups: Option<usize>,
    pub(super) remediation_sample_groups: Option<Vec<AnimeRemediationSample>>,
}

#[derive(Serialize, Debug, Default, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct AnimeRemediationReportOutput {
    pub generated_at: String,
    pub filesystem_mixed_root_groups: usize,
    pub plex_duplicate_show_groups: usize,
    pub plex_hama_anidb_tvdb_groups: usize,
    pub correlated_hama_split_groups: usize,
    pub remediation_groups: usize,
    pub returned_groups: usize,
    pub groups: Vec<AnimeRemediationSample>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct AnimeRootUsage {
    pub(super) filesystem_symlinks: usize,
    pub(super) db_active_links: usize,
}

#[allow(dead_code)]
pub(crate) async fn build_anime_remediation_report(
    cfg: &Config,
    db: &Database,
    plex_db_path: &Path,
    full: bool,
) -> Result<Option<AnimeRemediationReportOutput>> {
    let anime_libraries: Vec<&LibraryConfig> = cfg
        .libraries
        .iter()
        .filter(|lib| lib.content_type == Some(ContentType::Anime))
        .collect();
    if anime_libraries.is_empty() {
        return Ok(None);
    }

    let roots: Vec<PathBuf> = anime_libraries.iter().map(|lib| lib.path.clone()).collect();
    let link_records = db.get_links_scoped(Some(&roots)).await?;
    let generated_at = Utc::now().to_rfc3339();
    let anime_duplicates =
        build_anime_duplicate_audit(&anime_libraries, &link_records, Some(plex_db_path), full)
            .await?;
    let Some(anime_duplicates) = anime_duplicates else {
        return Ok(None);
    };

    Ok(Some(AnimeRemediationReportOutput {
        generated_at,
        filesystem_mixed_root_groups: anime_duplicates.filesystem_mixed_root_groups,
        plex_duplicate_show_groups: anime_duplicates.plex_duplicate_show_groups.unwrap_or(0),
        plex_hama_anidb_tvdb_groups: anime_duplicates.plex_hama_anidb_tvdb_groups.unwrap_or(0),
        correlated_hama_split_groups: anime_duplicates.correlated_hama_split_groups.unwrap_or(0),
        remediation_groups: anime_duplicates.remediation_groups.unwrap_or(0),
        returned_groups: anime_duplicates
            .remediation_sample_groups
            .as_ref()
            .map_or(0, Vec::len),
        groups: anime_duplicates
            .remediation_sample_groups
            .unwrap_or_default(),
    }))
}

pub(super) async fn build_anime_duplicate_audit(
    libraries: &[&LibraryConfig],
    link_records: &[crate::models::LinkRecord],
    plex_db_path: Option<&Path>,
    full: bool,
) -> Result<Option<AnimeDuplicateAuditOutput>> {
    let anime_libraries: Vec<&LibraryConfig> = libraries
        .iter()
        .copied()
        .filter(|lib| lib.content_type == Some(ContentType::Anime))
        .collect();
    if anime_libraries.is_empty() {
        return Ok(None);
    }

    let filesystem_groups = collect_anime_root_duplicate_groups(&anime_libraries);
    let anime_sample_limit = if full { usize::MAX } else { PATH_SAMPLE_LIMIT };
    let filesystem_mixed_root_groups = filesystem_groups.len();
    let filesystem_sample_groups = filesystem_groups
        .iter()
        .take(anime_sample_limit)
        .map(|group| AnimeRootDuplicateSample {
            normalized_title: group.normalized_title.clone(),
            tagged_roots: group.tagged_roots.clone(),
            untagged_roots: group.untagged_roots.clone(),
        })
        .collect();

    let (
        plex_duplicate_show_groups,
        plex_hama_anidb_tvdb_groups,
        plex_other_duplicate_show_groups,
        plex_sample_groups,
        correlated_hama_split_groups,
        correlated_sample_groups,
        remediation_groups,
        remediation_sample_groups,
    ) = if let Some(plex_db_path) = plex_db_path {
        let roots: Vec<PathBuf> = anime_libraries.iter().map(|lib| lib.path.clone()).collect();
        let records = plex_db::load_duplicate_show_records(plex_db_path, &roots).await?;
        let summary = summarize_plex_duplicate_show_records(&records, anime_sample_limit);
        let correlated_groups =
            correlate_anime_duplicate_groups(&filesystem_groups, &summary.all_groups);
        let remediation_groups_all = build_anime_remediation_samples(
            &correlated_groups,
            &collect_anime_root_usage(&anime_libraries, link_records),
        );
        (
            Some(summary.total_groups),
            Some(summary.hama_anidb_tvdb_groups),
            Some(summary.other_groups),
            Some(summary.sample_groups),
            Some(correlated_groups.len()),
            Some(
                correlated_groups
                    .into_iter()
                    .take(anime_sample_limit)
                    .collect(),
            ),
            Some(remediation_groups_all.len()),
            Some(
                remediation_groups_all
                    .into_iter()
                    .take(anime_sample_limit)
                    .collect(),
            ),
        )
    } else {
        (None, None, None, None, None, None, None, None)
    };

    Ok(Some(AnimeDuplicateAuditOutput {
        filesystem_mixed_root_groups,
        filesystem_sample_groups,
        plex_duplicate_show_groups,
        plex_hama_anidb_tvdb_groups,
        plex_other_duplicate_show_groups,
        plex_sample_groups,
        correlated_hama_split_groups,
        correlated_sample_groups,
        remediation_groups,
        remediation_sample_groups,
    }))
}

#[derive(Default)]
pub(super) struct PlexDuplicateSummary {
    pub(super) total_groups: usize,
    pub(super) hama_anidb_tvdb_groups: usize,
    pub(super) other_groups: usize,
    pub(super) all_groups: Vec<PlexDuplicateShowSample>,
    pub(super) sample_groups: Vec<PlexDuplicateShowSample>,
}

pub(super) fn summarize_plex_duplicate_show_records(
    records: &[plex_db::PlexDuplicateShowRecord],
    sample_limit: usize,
) -> PlexDuplicateSummary {
    #[derive(Default)]
    struct Bucket {
        total_rows: usize,
        live_rows: usize,
        guids: Vec<String>,
        guid_kinds: Vec<String>,
    }

    let mut grouped: BTreeMap<(String, String, Option<i64>), Bucket> = BTreeMap::new();
    for record in records {
        let bucket = grouped
            .entry((
                record.title.clone(),
                record.original_title.clone(),
                record.year,
            ))
            .or_default();
        bucket.total_rows += 1;
        if record.live {
            bucket.live_rows += 1;
        }
        bucket.guids.push(record.guid.clone());
        bucket.guid_kinds.push(record.guid_kind.clone());
    }

    let mut all_groups = Vec::new();
    let mut hama_anidb_tvdb_groups = 0;

    for ((title, original_title, year), bucket) in grouped {
        let mut unique_guids: Vec<String> = bucket.guids.into_iter().collect();
        unique_guids.sort();
        unique_guids.dedup();

        let mut unique_guid_kinds: Vec<String> = bucket.guid_kinds.into_iter().collect();
        unique_guid_kinds.sort();
        unique_guid_kinds.dedup();

        if unique_guid_kinds.iter().any(|kind| kind == "hama-anidb")
            && unique_guid_kinds.iter().any(|kind| kind == "hama-tvdb")
        {
            hama_anidb_tvdb_groups += 1;
        }

        all_groups.push(PlexDuplicateShowSample {
            title,
            original_title,
            year,
            total_rows: bucket.total_rows,
            live_rows: bucket.live_rows,
            deleted_rows: bucket.total_rows.saturating_sub(bucket.live_rows),
            guid_kinds: unique_guid_kinds,
            guids: unique_guids,
        });
    }

    let total_groups = all_groups.len();
    let sample_groups = all_groups.iter().take(sample_limit).cloned().collect();

    PlexDuplicateSummary {
        total_groups,
        hama_anidb_tvdb_groups,
        other_groups: total_groups.saturating_sub(hama_anidb_tvdb_groups),
        all_groups,
        sample_groups,
    }
}

pub(super) fn correlate_anime_duplicate_groups(
    filesystem_groups: &[crate::anime_roots::AnimeRootDuplicateGroup],
    plex_groups: &[PlexDuplicateShowSample],
) -> Vec<CorrelatedAnimeDuplicateSample> {
    let mut plex_by_title: HashMap<String, Vec<&PlexDuplicateShowSample>> = HashMap::new();
    for group in plex_groups {
        let is_hama_split = group.guid_kinds.iter().any(|kind| kind == "hama-anidb")
            && group.guid_kinds.iter().any(|kind| kind == "hama-tvdb");
        if !is_hama_split {
            continue;
        }

        plex_by_title
            .entry(normalize(&group.title))
            .or_default()
            .push(group);
    }

    let mut correlated = Vec::new();
    for fs_group in filesystem_groups {
        let Some(plex_matches) = plex_by_title.get(&normalize(&fs_group.normalized_title)) else {
            continue;
        };

        for plex_group in plex_matches {
            correlated.push(CorrelatedAnimeDuplicateSample {
                normalized_title: fs_group.normalized_title.clone(),
                tagged_roots: fs_group.tagged_roots.clone(),
                untagged_roots: fs_group.untagged_roots.clone(),
                plex_total_rows: plex_group.total_rows,
                plex_live_rows: plex_group.live_rows,
                plex_deleted_rows: plex_group.deleted_rows,
                plex_guid_kinds: plex_group.guid_kinds.clone(),
                plex_guids: plex_group.guids.clone(),
            });
        }
    }

    correlated.sort_by(|a, b| {
        b.untagged_roots
            .len()
            .cmp(&a.untagged_roots.len())
            .then_with(|| a.normalized_title.cmp(&b.normalized_title))
    });
    correlated
}

pub(super) fn collect_anime_root_usage(
    anime_libraries: &[&LibraryConfig],
    link_records: &[crate::models::LinkRecord],
) -> HashMap<PathBuf, AnimeRootUsage> {
    let mut by_root: HashMap<PathBuf, AnimeRootUsage> = HashMap::new();

    for library in anime_libraries {
        for entry in WalkDir::new(&library.path).follow_links(false) {
            let Ok(entry) = entry else {
                continue;
            };
            if !entry.file_type().is_symlink() {
                continue;
            }
            let Some(root) = anime_show_root_for_path(anime_libraries, entry.path()) else {
                continue;
            };
            by_root.entry(root).or_default().filesystem_symlinks += 1;
        }
    }

    for link in link_records
        .iter()
        .filter(|link| link.status == LinkStatus::Active)
    {
        let Some(root) = anime_show_root_for_path(anime_libraries, &link.target_path) else {
            continue;
        };
        by_root.entry(root).or_default().db_active_links += 1;
    }

    by_root
}

fn anime_show_root_for_path(anime_libraries: &[&LibraryConfig], path: &Path) -> Option<PathBuf> {
    let library = anime_libraries
        .iter()
        .filter(|lib| path.starts_with(&lib.path))
        .max_by_key(|lib| lib.path.components().count())?;
    let first_component = path.strip_prefix(&library.path).ok()?.components().next()?;
    Some(library.path.join(first_component.as_os_str()))
}

pub(super) fn build_anime_remediation_samples(
    correlated_groups: &[CorrelatedAnimeDuplicateSample],
    root_usage: &HashMap<PathBuf, AnimeRootUsage>,
) -> Vec<AnimeRemediationSample> {
    let mut samples = Vec::new();

    for group in correlated_groups {
        let mut tagged_roots: Vec<_> = group
            .tagged_roots
            .iter()
            .map(|path| AnimeRootUsageSample {
                path: path.clone(),
                filesystem_symlinks: root_usage
                    .get(path)
                    .map_or(0, |usage| usage.filesystem_symlinks),
                db_active_links: root_usage
                    .get(path)
                    .map_or(0, |usage| usage.db_active_links),
            })
            .collect();
        tagged_roots.sort_by(compare_root_usage);

        let Some(recommended_tagged_root) = tagged_roots.first().cloned() else {
            continue;
        };

        let mut legacy_roots: Vec<_> = group
            .untagged_roots
            .iter()
            .map(|path| AnimeRootUsageSample {
                path: path.clone(),
                filesystem_symlinks: root_usage
                    .get(path)
                    .map_or(0, |usage| usage.filesystem_symlinks),
                db_active_links: root_usage
                    .get(path)
                    .map_or(0, |usage| usage.db_active_links),
            })
            .collect();
        legacy_roots.sort_by(compare_root_usage);

        samples.push(AnimeRemediationSample {
            normalized_title: group.normalized_title.clone(),
            recommended_tagged_root,
            alternate_tagged_roots: tagged_roots.into_iter().skip(1).collect(),
            legacy_roots,
            plex_total_rows: group.plex_total_rows,
            plex_live_rows: group.plex_live_rows,
            plex_deleted_rows: group.plex_deleted_rows,
            plex_guid_kinds: group.plex_guid_kinds.clone(),
            plex_guids: group.plex_guids.clone(),
        });
    }

    samples.sort_by(|left, right| {
        remediation_impact(right)
            .cmp(&remediation_impact(left))
            .then_with(|| left.normalized_title.cmp(&right.normalized_title))
    });
    samples
}

fn compare_root_usage(
    left: &AnimeRootUsageSample,
    right: &AnimeRootUsageSample,
) -> std::cmp::Ordering {
    right
        .db_active_links
        .cmp(&left.db_active_links)
        .then_with(|| right.filesystem_symlinks.cmp(&left.filesystem_symlinks))
        .then_with(|| left.path.cmp(&right.path))
}

fn remediation_impact(sample: &AnimeRemediationSample) -> (usize, usize, usize) {
    let legacy_fs = sample
        .legacy_roots
        .iter()
        .map(|root| root.filesystem_symlinks)
        .sum();
    let legacy_db = sample
        .legacy_roots
        .iter()
        .map(|root| root.db_active_links)
        .sum();
    (legacy_fs, legacy_db, sample.plex_live_rows)
}

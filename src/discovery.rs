use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::models::{LinkRecord, MatchResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoverPlacementAction {
    Create,
    Update,
    BlockedRegularFile,
    BlockedDirectory,
}

impl DiscoverPlacementAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::BlockedRegularFile => "blocked_regular_file",
            Self::BlockedDirectory => "blocked_directory",
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_blocked(self) -> bool {
        matches!(self, Self::BlockedRegularFile | Self::BlockedDirectory)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverPlacement {
    pub library_name: String,
    pub media_id: String,
    pub title: String,
    pub folder_path: PathBuf,
    pub source_path: PathBuf,
    pub target_path: PathBuf,
    pub source_name: String,
    pub action: DiscoverPlacementAction,
    pub season: Option<u32>,
    pub episode: Option<u32>,
}

impl DiscoverPlacement {
    pub fn action_label(&self) -> &'static str {
        self.action.as_str()
    }

    pub fn action_badge_class(&self) -> &'static str {
        match self.action {
            DiscoverPlacementAction::Create => "badge-success",
            DiscoverPlacementAction::Update => "badge-warning",
            DiscoverPlacementAction::BlockedRegularFile
            | DiscoverPlacementAction::BlockedDirectory => "badge-danger",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverFolderPlan {
    pub library_name: String,
    pub media_id: String,
    pub title: String,
    pub folder_path: PathBuf,
    pub existing_links: usize,
    pub planned_creates: usize,
    pub planned_updates: usize,
    pub blocked: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiscoverSummary {
    pub folders: usize,
    pub placements: usize,
    pub creates: usize,
    pub updates: usize,
    pub blocked: usize,
}

#[derive(Debug, Clone, Default)]
pub struct DiscoverPlan {
    pub folders: Vec<DiscoverFolderPlan>,
    pub placements: Vec<DiscoverPlacement>,
}

impl DiscoverPlan {
    pub fn summary(&self) -> DiscoverSummary {
        let mut summary = DiscoverSummary {
            folders: self.folders.len(),
            placements: self.placements.len(),
            ..DiscoverSummary::default()
        };

        for placement in &self.placements {
            match placement.action {
                DiscoverPlacementAction::Create => summary.creates += 1,
                DiscoverPlacementAction::Update => summary.updates += 1,
                DiscoverPlacementAction::BlockedRegularFile
                | DiscoverPlacementAction::BlockedDirectory => summary.blocked += 1,
            }
        }

        summary
    }
}

#[derive(Default)]
struct FolderAccumulator {
    library_name: String,
    media_id: String,
    title: String,
    folder_path: PathBuf,
    existing_links: usize,
    planned_creates: usize,
    planned_updates: usize,
    blocked: usize,
}

/// Discover review builder.
///
/// Unlike the old gap list, this plans concrete source -> target placements using
/// the same destination naming/path rules as the linker.
pub struct Discovery;

impl Discovery {
    pub fn new() -> Self {
        Self
    }

    pub async fn build_link_plan<F>(
        &self,
        db: &Database,
        matches: &[MatchResult],
        mut build_target_path: F,
    ) -> Result<DiscoverPlan>
    where
        F: FnMut(&MatchResult) -> Result<PathBuf>,
    {
        if matches.is_empty() {
            return Ok(DiscoverPlan::default());
        }

        let target_paths = matches
            .iter()
            .map(&mut build_target_path)
            .collect::<Result<Vec<_>>>()?;
        let existing_by_target = load_existing_links(db, &target_paths).await?;

        let mut folder_roots = Vec::new();
        for m in matches {
            if !folder_roots
                .iter()
                .any(|root: &PathBuf| root == &m.library_item.path)
            {
                folder_roots.push(m.library_item.path.clone());
            }
        }

        let active_links = db.get_active_links_scoped(Some(&folder_roots)).await?;
        let existing_counts = existing_link_counts_by_folder(&folder_roots, &active_links);

        let mut folders: BTreeMap<PathBuf, FolderAccumulator> = BTreeMap::new();
        let mut placements = Vec::new();

        for (m, target_path) in matches.iter().zip(target_paths.into_iter()) {
            let action = classify_target_path(
                &target_path,
                &m.source_item.path,
                existing_by_target.get(&target_path),
            );

            let Some(action) = action else {
                continue;
            };

            let folder_entry = folders
                .entry(m.library_item.path.clone())
                .or_insert_with(|| FolderAccumulator {
                    library_name: m.library_item.library_name.clone(),
                    media_id: m.library_item.id.to_string(),
                    title: m.library_item.title.clone(),
                    folder_path: m.library_item.path.clone(),
                    existing_links: existing_counts
                        .get(&m.library_item.path)
                        .copied()
                        .unwrap_or(0),
                    ..FolderAccumulator::default()
                });

            match action {
                DiscoverPlacementAction::Create => folder_entry.planned_creates += 1,
                DiscoverPlacementAction::Update => folder_entry.planned_updates += 1,
                DiscoverPlacementAction::BlockedRegularFile
                | DiscoverPlacementAction::BlockedDirectory => folder_entry.blocked += 1,
            }

            placements.push(DiscoverPlacement {
                library_name: m.library_item.library_name.clone(),
                media_id: m.library_item.id.to_string(),
                title: m.library_item.title.clone(),
                folder_path: m.library_item.path.clone(),
                source_name: m
                    .source_item
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_string(),
                source_path: m.source_item.path.clone(),
                target_path,
                action,
                season: m.source_item.season,
                episode: m.source_item.episode,
            });
        }

        placements.sort_by(|a, b| {
            (
                a.library_name.as_str(),
                a.title.as_str(),
                a.season.unwrap_or(0),
                a.episode.unwrap_or(0),
                a.source_name.as_str(),
            )
                .cmp(&(
                    b.library_name.as_str(),
                    b.title.as_str(),
                    b.season.unwrap_or(0),
                    b.episode.unwrap_or(0),
                    b.source_name.as_str(),
                ))
        });

        let mut folder_list = folders
            .into_values()
            .map(|folder| DiscoverFolderPlan {
                library_name: folder.library_name,
                media_id: folder.media_id,
                title: folder.title,
                folder_path: folder.folder_path,
                existing_links: folder.existing_links,
                planned_creates: folder.planned_creates,
                planned_updates: folder.planned_updates,
                blocked: folder.blocked,
            })
            .collect::<Vec<_>>();

        folder_list.sort_by(|a, b| {
            (
                a.library_name.as_str(),
                a.title.as_str(),
                a.folder_path.as_os_str(),
            )
                .cmp(&(
                    b.library_name.as_str(),
                    b.title.as_str(),
                    b.folder_path.as_os_str(),
                ))
        });

        Ok(DiscoverPlan {
            folders: folder_list,
            placements,
        })
    }

    pub fn print_summary(plan: &DiscoverPlan) {
        let summary = plan.summary();
        if summary.placements == 0 {
            println!("✅ No discoverable placements found for underlinked library folders.");
            return;
        }

        println!(
            "\n📦 {} placement(s) across {} folder(s): create={}, update={}, blocked={}\n",
            summary.placements, summary.folders, summary.creates, summary.updates, summary.blocked
        );

        println!("Folder summary:");
        for folder in &plan.folders {
            println!(
                "  - {} [{}] existing={} create={} update={} blocked={} :: {}",
                folder.title,
                folder.media_id,
                folder.existing_links,
                folder.planned_creates,
                folder.planned_updates,
                folder.blocked,
                folder.folder_path.display()
            );
        }

        println!("\nPlacement report:");
        for placement in &plan.placements {
            println!(
                "  - [{}] {} -> {}",
                placement.action.as_str(),
                placement.source_path.display(),
                placement.target_path.display()
            );
        }
    }
}

fn classify_target_path(
    target_path: &Path,
    expected_source: &Path,
    existing_link: Option<&LinkRecord>,
) -> Option<DiscoverPlacementAction> {
    if let Ok(meta) = std::fs::symlink_metadata(target_path) {
        if meta.file_type().is_dir() {
            return Some(DiscoverPlacementAction::BlockedDirectory);
        }

        if !meta.file_type().is_symlink() {
            return Some(DiscoverPlacementAction::BlockedRegularFile);
        }

        let current_target = std::fs::read_link(target_path).ok()?;
        let resolved = resolve_link_target(target_path, &current_target);
        if resolved == expected_source {
            return None;
        }

        return Some(DiscoverPlacementAction::Update);
    }

    existing_link.map_or(Some(DiscoverPlacementAction::Create), |_| {
        Some(DiscoverPlacementAction::Update)
    })
}

fn resolve_link_target(link_path: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        link_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(target)
    }
}

async fn load_existing_links(
    db: &Database,
    target_paths: &[PathBuf],
) -> Result<HashMap<PathBuf, LinkRecord>> {
    let mut by_target = HashMap::new();
    for chunk in target_paths.chunks(500) {
        for link in db.get_links_by_targets(chunk).await? {
            by_target.insert(link.target_path.clone(), link);
        }
    }
    Ok(by_target)
}

fn existing_link_counts_by_folder(
    folder_roots: &[PathBuf],
    active_links: &[LinkRecord],
) -> HashMap<PathBuf, usize> {
    let mut counts = HashMap::new();
    let folder_root_set: HashSet<PathBuf> = folder_roots.iter().cloned().collect();

    for link in active_links {
        for ancestor in link.target_path.ancestors() {
            let candidate = ancestor.to_path_buf();
            if folder_root_set.contains(&candidate) {
                *counts.entry(candidate).or_insert(0) += 1;
                break;
            }
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use crate::config::ContentType;
    use crate::linker::Linker;
    use crate::models::{LibraryItem, MediaId, MediaType, SourceItem};

    fn sample_tv_match(
        lib_path: &Path,
        source_path: &Path,
        season: u32,
        episode: u32,
    ) -> MatchResult {
        MatchResult {
            library_item: LibraryItem {
                id: MediaId::Tvdb(81189),
                path: lib_path.to_path_buf(),
                title: "Sample Show".to_string(),
                library_name: "Anime".to_string(),
                media_type: MediaType::Tv,
                content_type: ContentType::Anime,
            },
            source_item: SourceItem {
                path: source_path.to_path_buf(),
                parsed_title: "Sample Show".to_string(),
                season: Some(season),
                episode: Some(episode),
                episode_end: None,
                quality: Some("1080p".to_string()),
                extension: "mkv".to_string(),
                year: None,
            },
            confidence: 1.0,
            matched_alias: "sample show".to_string(),
            episode_title: Some("Pilot".to_string()),
        }
    }

    #[test]
    fn existing_link_counts_by_folder_matches_nested_targets_without_quadratic_scan() {
        let folder = PathBuf::from("/plex/Anime/Sample Show {tvdb-81189}");
        let active_links = vec![
            LinkRecord {
                id: Some(1),
                source_path: PathBuf::from("/rd/Sample.Show.S01E01.mkv"),
                target_path: folder.join("Season 01").join("Sample Show - S01E01.mkv"),
                media_id: "tvdb-81189".to_string(),
                media_type: crate::models::MediaType::Tv,
                status: crate::models::LinkStatus::Active,
                created_at: None,
                updated_at: None,
            },
            LinkRecord {
                id: Some(2),
                source_path: PathBuf::from("/rd/Sample.Show.S01E02.mkv"),
                target_path: folder.join("Season 01").join("Sample Show - S01E02.mkv"),
                media_id: "tvdb-81189".to_string(),
                media_type: crate::models::MediaType::Tv,
                status: crate::models::LinkStatus::Active,
                created_at: None,
                updated_at: None,
            },
        ];

        let counts = existing_link_counts_by_folder(std::slice::from_ref(&folder), &active_links);
        assert_eq!(counts.get(&folder), Some(&2));
    }

    #[tokio::test]
    async fn build_link_plan_marks_missing_target_as_create() {
        let dir = tempfile::tempdir().unwrap();
        let lib_path = dir.path().join("Sample Show {tvdb-81189}");
        let source_path = dir.path().join("rd").join("Sample.Show.S01E01.1080p.mkv");
        fs::create_dir_all(&lib_path).unwrap();
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "video").unwrap();

        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let linker = Linker::new(false, true, "{title} - S{season:02}E{episode:02}");
        let plan = Discovery::new()
            .build_link_plan(
                &db,
                &[sample_tv_match(&lib_path, &source_path, 1, 1)],
                |m| linker.build_target_path(m),
            )
            .await
            .unwrap();

        let summary = plan.summary();
        assert_eq!(summary.creates, 1);
        assert_eq!(summary.updates, 0);
        assert_eq!(summary.blocked, 0);
        assert_eq!(plan.folders.len(), 1);
        assert_eq!(plan.folders[0].existing_links, 0);
        assert_eq!(plan.placements[0].action, DiscoverPlacementAction::Create);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn build_link_plan_marks_wrong_symlink_as_update() {
        let dir = tempfile::tempdir().unwrap();
        let lib_path = dir.path().join("Sample Show {tvdb-81189}");
        let source_path = dir.path().join("rd").join("Sample.Show.S01E01.1080p.mkv");
        let wrong_source = dir.path().join("rd").join("Wrong.Show.S01E01.1080p.mkv");
        fs::create_dir_all(&lib_path).unwrap();
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "video").unwrap();
        fs::write(&wrong_source, "video").unwrap();

        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let linker = Linker::new(false, true, "{title} - S{season:02}E{episode:02}");
        let target_path = linker
            .build_target_path(&sample_tv_match(&lib_path, &source_path, 1, 1))
            .unwrap();
        fs::create_dir_all(target_path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&wrong_source, &target_path).unwrap();

        let plan = Discovery::new()
            .build_link_plan(
                &db,
                &[sample_tv_match(&lib_path, &source_path, 1, 1)],
                |m| linker.build_target_path(m),
            )
            .await
            .unwrap();

        assert_eq!(plan.summary().updates, 1);
        assert_eq!(plan.placements[0].action, DiscoverPlacementAction::Update);
    }

    #[tokio::test]
    async fn build_link_plan_blocks_regular_file_target() {
        let dir = tempfile::tempdir().unwrap();
        let lib_path = dir.path().join("Sample Show {tvdb-81189}");
        let source_path = dir.path().join("rd").join("Sample.Show.S01E01.1080p.mkv");
        fs::create_dir_all(&lib_path).unwrap();
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "video").unwrap();

        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();
        let linker = Linker::new(false, true, "{title} - S{season:02}E{episode:02}");
        let target_path = linker
            .build_target_path(&sample_tv_match(&lib_path, &source_path, 1, 1))
            .unwrap();
        fs::create_dir_all(target_path.parent().unwrap()).unwrap();
        fs::write(&target_path, "not-a-symlink").unwrap();

        let plan = Discovery::new()
            .build_link_plan(
                &db,
                &[sample_tv_match(&lib_path, &source_path, 1, 1)],
                |m| linker.build_target_path(m),
            )
            .await
            .unwrap();

        assert_eq!(plan.summary().blocked, 1);
        assert!(plan.placements[0].action.is_blocked());
    }
}

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::api::realdebrid::{RdFile, RealDebridClient};
use crate::db::Database;
use crate::utils::ProgressLine;

/// Represents the cached state of a Real-Debrid torrent
#[derive(Debug, Serialize, Deserialize)]
struct CachedTorrentFiles {
    files: Vec<RdFile>,
}

/// Returns the number of selected files stored in a cached `files_json` blob.
///
/// This fingerprint is compared against the link count from the RD API list endpoint
/// to detect file-selection changes (deselecting files doesn't change hash or status).
/// Returns 0 on parse failure so mismatched/empty JSON triggers a re-fetch.
fn selected_files_fingerprint(files_json: &str) -> usize {
    serde_json::from_str::<CachedTorrentFiles>(files_json)
        .map(|c| c.files.iter().filter(|f| f.selected == 1).count())
        .unwrap_or(0)
}

pub struct TorrentCache<'a> {
    db: &'a Database,
    rd: &'a RealDebridClient,
}

impl<'a> TorrentCache<'a> {
    pub fn new(db: &'a Database, rd: &'a RealDebridClient) -> Self {
        Self { db, rd }
    }

    /// Synchronize the local cache with the Real-Debrid API.
    /// Caps per-torrent info fetches to avoid 429s on large accounts.
    /// The scanner falls back to a filesystem walk for un-cached torrents.
    pub async fn sync(&self) -> Result<()> {
        self.sync_inner(Some(150)).await
    }

    /// Full cache build — no cap on per-torrent info fetches.
    /// Use for scheduled nightly jobs where wall-clock time doesn't matter.
    pub async fn sync_full(&self) -> Result<()> {
        self.sync_inner(None).await
    }

    /// Inner sync implementation.  `max_fetches` caps the number of per-torrent
    /// `get_torrent_info` calls.  `None` means no limit (full build).
    async fn sync_inner(&self, max_fetches: Option<usize>) -> Result<()> {
        info!("Starting Real-Debrid cache sync...");

        // 1. Fetch all torrents from RD API
        let api_torrents = self.rd.list_all_torrents().await?;
        debug!("Fetched {} torrents from RD API", api_torrents.len());

        // 2. Load current cache state from DB
        let db_torrents = self.db.get_rd_torrents().await?;
        let mut db_map: HashMap<String, (String, String, usize)> = HashMap::new();
        for (id, hash, _, status, files_json) in &db_torrents {
            let fingerprint = selected_files_fingerprint(files_json);
            db_map.insert(id.clone(), (hash.clone(), status.clone(), fingerprint));
        }

        let api_ids: HashSet<String> = api_torrents.iter().map(|t| t.id.clone()).collect();
        let mut added = 0;
        let mut updated = 0;
        let mut removed = 0;

        let total_api = api_torrents.len();

        // Count torrents that changed (hash/status/fingerprint drift)
        let needs_change_update = api_torrents
            .iter()
            .filter(|t| {
                t.status == "downloaded"
                    && match db_map.get(&t.id) {
                        Some((h, s, fp)) => {
                            h != &t.hash
                                || s != &t.status
                                || (!t.links.is_empty() && *fp != t.links.len())
                        }
                        None => true,
                    }
            })
            .count();

        // Count downloaded torrents in DB with empty file info (need backfill).
        // The RD list endpoint returns links=[] for most torrents, so the
        // fingerprint comparison (0 == 0) misses these — they need explicit
        // file info fetching even though nothing "changed".
        let needs_backfill = api_torrents
            .iter()
            .filter(|t| {
                t.status == "downloaded"
                    && match db_map.get(&t.id) {
                        Some((_, _, fp)) => *fp == 0 && t.links.is_empty(),
                        None => false, // already counted in needs_change_update
                    }
            })
            .count();

        let total_need_fetch = needs_change_update + needs_backfill;
        let effective_limit = max_fetches.unwrap_or(usize::MAX);
        let fetch_limit = total_need_fetch.min(effective_limit);
        let is_capped = total_need_fetch > effective_limit;
        let mut progress = ProgressLine::new("RD cache sync:");

        if is_capped {
            info!(
                "RD cache: {} torrents need file info but capping at {} per cycle \
                 (filesystem fallback will cover the rest)",
                total_need_fetch, effective_limit
            );
            progress.update(format!(
                "{} need file info, fetching {} (filesystem fallback for rest)",
                total_need_fetch, effective_limit
            ));
        } else if total_need_fetch > 0 {
            progress.update(format!(
                "{} torrents need file info ({} total, {} cached)",
                total_need_fetch,
                total_api,
                db_map.len()
            ));
        } else {
            progress.update(format!("{} torrents, checking for changes...", total_api));
        }

        let mut info_fetched = 0usize;
        for t in api_torrents {
            // The number of generated links from the list endpoint reflects how many files are
            // selected. We use this as a cheap proxy to detect file-selection changes without
            // an extra per-torrent API call on every sync cycle.
            let api_links_count = t.links.len();

            let (needs_update, needs_backfill) = match db_map.get(&t.id) {
                Some((db_hash, db_status, db_fingerprint)) => {
                    let changed = if db_hash != &t.hash {
                        // Hash changed (torrent replaced)
                        true
                    } else if db_status != &t.status {
                        // Status transition (e.g. downloading → downloaded)
                        true
                    } else if api_links_count > 0 && *db_fingerprint != api_links_count {
                        // File selection changed: stored selected-file count differs from
                        // the link count reported by the API list endpoint.
                        // Only compare when API actually returns links — RD's list endpoint
                        // often returns links=[] even for downloaded torrents, so a 0 from
                        // the API is not a meaningful signal.
                        debug!(
                            "Torrent {} file selection changed (cached={}, api={})",
                            t.id, db_fingerprint, api_links_count
                        );
                        true
                    } else {
                        false
                    };
                    // Downloaded torrent with empty file info needs backfill
                    let backfill = !changed
                        && t.status == "downloaded"
                        && *db_fingerprint == 0
                        && api_links_count == 0;
                    (changed, backfill)
                }
                None => (true, false), // New torrent
            };

            if needs_update || needs_backfill {
                // Only fetch detailed file info for downloaded torrents that are
                // within the per-cycle cap.  Others get stored with an empty file
                // list so we still track status transitions; the scanner's filesystem
                // fallback handles file discovery for un-cached torrents.
                let should_fetch_files = t.status == "downloaded" && info_fetched < effective_limit;

                if should_fetch_files {
                    info_fetched += 1;
                    progress.update(format!(
                        "Fetching file info {}/{}  ({})",
                        info_fetched, fetch_limit, t.filename
                    ));

                    match self.rd.get_torrent_info(&t.id).await {
                        Ok(info) => {
                            let files_json = serde_json::to_string(&CachedTorrentFiles {
                                files: info.files.clone(),
                            })?;

                            self.db
                                .upsert_rd_torrent(
                                    &t.id,
                                    &t.hash,
                                    &t.filename,
                                    &t.status,
                                    &files_json,
                                )
                                .await?;

                            if db_map.contains_key(&t.id) {
                                updated += 1;
                            } else {
                                added += 1;
                            }
                        }
                        Err(e) => {
                            warn!("Failed to fetch info for torrent {}: {}", t.id, e);
                        }
                    }

                    // Explicit delay between batches (10 items) to prevent 429 timeouts
                    // even with the token bucket rate limiter, since RD is very strict on long sustained bursts.
                    if info_fetched.is_multiple_of(10) {
                        debug!("Fetched 10 infos, pausing 5s to respect RD limits...");
                        tokio::time::sleep(std::time::Duration::from_millis(5000)).await;
                    }
                } else if needs_update {
                    // Store without file details — either non-downloaded or beyond
                    // the fetch cap.  Status transitions will be caught next cycle.
                    // Skip for backfill-only candidates (already in DB with empty files).
                    self.db
                        .upsert_rd_torrent(
                            &t.id,
                            &t.hash,
                            &t.filename,
                            &t.status,
                            r#"{"files":[]}"#,
                        )
                        .await?;

                    if db_map.contains_key(&t.id) {
                        updated += 1;
                    } else {
                        added += 1;
                    }
                }
            }
        }

        // 4. Process deletions (In DB but not in API)
        for (id, _, _, _, _) in db_torrents {
            if !api_ids.contains(&id) {
                self.db.delete_rd_torrent(&id).await?;
                removed += 1;
            }
        }

        progress.finish(format!(
            "+{} added, ~{} updated, -{} removed ({} API calls)",
            added, updated, removed, info_fetched
        ));

        info!(
            "Cache sync complete: +{} added, ~{} updated, -{} removed ({} info fetches)",
            added, updated, removed, info_fetched
        );

        Ok(())
    }

    /// Retrieve all files from the cache, mapped to the local mount path.
    /// Returns a list of (full_path, size_bytes).
    pub async fn get_files(&self, mount_path: &Path) -> Result<Vec<(PathBuf, u64)>> {
        let torrents = self.db.get_rd_torrents().await?;
        let mut all_files = Vec::new();

        for (_, _, torrent_filename, status, files_json) in torrents {
            // Only consider downloaded torrents, as others might not appear on mount yet
            if status != "downloaded" {
                continue;
            }

            let cached: CachedTorrentFiles = match serde_json::from_str(&files_json) {
                Ok(c) => c,
                Err(e) => {
                    warn!("Failed to deserialize files for torrent: {}", e);
                    continue;
                }
            };

            for file in cached.files {
                // Only selected files appear on mount (usually)
                if file.selected != 1 {
                    continue;
                }

                let full_path = cached_mount_path(mount_path, &torrent_filename, &file.path);

                all_files.push((full_path, file.bytes as u64));
            }
        }

        Ok(all_files)
    }
}

fn cached_mount_path(mount_path: &Path, torrent_filename: &str, rd_file_path: &str) -> PathBuf {
    // Mount structure is usually /mount/TorrentName/path/inside/torrent.
    // RD can report single-file torrents with filename="Movie.mkv" and
    // path="/Movie.mkv", while the mount folder is actually /mount/Movie/Movie.mkv.
    // In that case we need to drop the video extension from the folder segment.
    let relative_path = rd_file_path.trim_start_matches('/');
    let relative = Path::new(relative_path);

    let mount_folder = if is_single_file_torrent_path(torrent_filename, relative) {
        Path::new(torrent_filename)
            .file_stem()
            .map(|stem| stem.to_owned())
            .unwrap_or_else(|| torrent_filename.into())
    } else {
        torrent_filename.into()
    };

    mount_path.join(mount_folder).join(relative)
}

fn is_single_file_torrent_path(torrent_filename: &str, relative: &Path) -> bool {
    let Some(file_name) = relative.file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    if file_name != torrent_filename {
        return false;
    }

    relative
        .parent()
        .is_none_or(|parent| parent.as_os_str().is_empty())
        && Path::new(torrent_filename)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "mkv" | "mp4" | "avi" | "mov" | "wmv" | "m4v" | "ts" | "m2ts" | "webm"
                )
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::realdebrid::RdFile;

    #[tokio::test]
    async fn test_get_files_from_cache() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        // Create a fake cached torrent
        let files = vec![
            RdFile {
                id: 1,
                path: "/Show/S01/Episode.mkv".to_string(),
                bytes: 1024,
                selected: 1,
            },
            RdFile {
                id: 2,
                path: "/Show/S01/Sample.mkv".to_string(),
                bytes: 1024,
                selected: 0, // Should be ignored
            },
        ];
        let files_json = serde_json::to_string(&CachedTorrentFiles { files }).unwrap();

        db.upsert_rd_torrent(
            "ID123",
            "hash123",
            "MyTorrentName",
            "downloaded",
            &files_json,
        )
        .await
        .unwrap();

        // Initialize cache (client not needed for get_files)
        // We need a dummy client or we can't construct TorrentCache.
        // But TorrentCache holds &RealDebridClient.
        // We can create a dummy one if we pass a token.
        let rd_client = RealDebridClient::new("dummy_token");
        let cache = TorrentCache::new(&db, &rd_client);

        let mount = PathBuf::from("/mnt/realdebrid");
        let results = cache.get_files(&mount).await.unwrap();

        assert_eq!(results.len(), 1);
        let (path, size) = &results[0];

        // Expected: /mnt/realdebrid/MyTorrentName/Show/S01/Episode.mkv
        // Note: MyTorrentName is the folder on RD mount?
        // Usually RD mount flattens or keeps structure?
        // The implementation assumes: mount_path.join(torrent_filename).join(relative_path)

        let expected = mount.join("MyTorrentName").join("Show/S01/Episode.mkv");
        assert_eq!(path, &expected);
        assert_eq!(*size, 1024);
    }

    #[tokio::test]
    async fn test_get_files_excludes_incomplete_and_unselected() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        // 1. Torrent that is NOT downloaded
        let files1 = vec![RdFile {
            id: 1,
            path: "/Incomplete/File.mkv".to_string(),
            bytes: 500,
            selected: 1,
        }];
        db.upsert_rd_torrent(
            "ID_INC",
            "hash_inc",
            "IncompleteTorrent",
            "downloading",
            &serde_json::to_string(&CachedTorrentFiles { files: files1 }).unwrap(),
        )
        .await
        .unwrap();

        // 2. Torrent that IS downloaded but file is unselected
        let files2 = vec![RdFile {
            id: 1,
            path: "/Selected/File.mkv".to_string(),
            bytes: 500,
            selected: 0,
        }];
        db.upsert_rd_torrent(
            "ID_UNSEL",
            "hash_unsel",
            "UnselectedTorrent",
            "downloaded",
            &serde_json::to_string(&CachedTorrentFiles { files: files2 }).unwrap(),
        )
        .await
        .unwrap();

        // 3. Valid torrent
        let files3 = vec![RdFile {
            id: 1,
            path: "/Valid/File.mkv".to_string(),
            bytes: 500,
            selected: 1,
        }];
        db.upsert_rd_torrent(
            "ID_VALID",
            "hash_valid",
            "ValidTorrent",
            "downloaded",
            &serde_json::to_string(&CachedTorrentFiles { files: files3 }).unwrap(),
        )
        .await
        .unwrap();

        let rd_client = RealDebridClient::new("dummy");
        let cache = TorrentCache::new(&db, &rd_client);

        let mount = PathBuf::from("/mnt/rd");
        let results = cache.get_files(&mount).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].0,
            mount.join("ValidTorrent").join("Valid/File.mkv")
        );
    }

    #[tokio::test]
    async fn test_get_files_maps_single_file_torrent_without_extension_folder() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("test.db").to_str().unwrap())
            .await
            .unwrap();

        let files = vec![RdFile {
            id: 1,
            path: "/Monarch.Legacy.of.Monsters.S02E02.Risonanza.ITA.ENG.2160p.ATVP.WEB-DL.DDP5.1.Atmos.DV.HDR.H.25-MeM.GP.mkv".to_string(),
            bytes: 1024,
            selected: 1,
        }];
        db.upsert_rd_torrent(
            "ID_MONARCH",
            "hash_monarch",
            "Monarch.Legacy.of.Monsters.S02E02.Risonanza.ITA.ENG.2160p.ATVP.WEB-DL.DDP5.1.Atmos.DV.HDR.H.25-MeM.GP.mkv",
            "downloaded",
            &serde_json::to_string(&CachedTorrentFiles { files }).unwrap(),
        )
        .await
        .unwrap();

        let rd_client = RealDebridClient::new("dummy");
        let cache = TorrentCache::new(&db, &rd_client);

        let mount = PathBuf::from("/mnt/decypharr/realdebrid/__all__");
        let results = cache.get_files(&mount).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].0,
            mount
                .join("Monarch.Legacy.of.Monsters.S02E02.Risonanza.ITA.ENG.2160p.ATVP.WEB-DL.DDP5.1.Atmos.DV.HDR.H.25-MeM.GP")
                .join("Monarch.Legacy.of.Monsters.S02E02.Risonanza.ITA.ENG.2160p.ATVP.WEB-DL.DDP5.1.Atmos.DV.HDR.H.25-MeM.GP.mkv")
        );
    }

    #[test]
    fn test_selected_files_fingerprint() {
        // Two selected, one not → fingerprint is 2
        let files = vec![
            RdFile {
                id: 1,
                path: "/a.mkv".into(),
                bytes: 100,
                selected: 1,
            },
            RdFile {
                id: 2,
                path: "/b.mkv".into(),
                bytes: 100,
                selected: 1,
            },
            RdFile {
                id: 3,
                path: "/c.mkv".into(),
                bytes: 100,
                selected: 0,
            },
        ];
        let json = serde_json::to_string(&CachedTorrentFiles { files }).unwrap();
        assert_eq!(selected_files_fingerprint(&json), 2);

        // All unselected → fingerprint is 0
        let files_none = vec![RdFile {
            id: 1,
            path: "/a.mkv".into(),
            bytes: 100,
            selected: 0,
        }];
        let json_none = serde_json::to_string(&CachedTorrentFiles { files: files_none }).unwrap();
        assert_eq!(selected_files_fingerprint(&json_none), 0);

        // Invalid JSON → fingerprint is 0 (triggers re-fetch)
        assert_eq!(selected_files_fingerprint("not valid json"), 0);
    }

    #[test]
    fn test_fingerprint_detects_deselection() {
        // Simulate: 2 files selected in cache, but API now shows 1 link (one was deselected)
        let files = vec![
            RdFile {
                id: 1,
                path: "/ep1.mkv".into(),
                bytes: 500,
                selected: 1,
            },
            RdFile {
                id: 2,
                path: "/ep2.mkv".into(),
                bytes: 500,
                selected: 1,
            },
        ];
        let json = serde_json::to_string(&CachedTorrentFiles { files }).unwrap();
        let cached_fingerprint = selected_files_fingerprint(&json);
        assert_eq!(cached_fingerprint, 2);

        // After deselection, API reports only 1 link
        let api_links_count = 1_usize;
        assert_ne!(
            cached_fingerprint, api_links_count,
            "should detect file selection change"
        );
    }
}

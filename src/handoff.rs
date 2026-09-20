//! Import of handoff markers written by an acquisition tool (backfill-buddy's
//! `symlinkarr.queue_dir`). A marker is one JSON file per RD torrent naming the folder the
//! torrent materialised as on the mount and the tvdb/tmdb id it was acquired for. Each
//! marker becomes a source pin so the matcher binds that folder to that library item
//! instead of guessing from the release title.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::{debug, warn};

use crate::db::Database;
use crate::models::MediaId;

#[derive(Debug, Deserialize)]
struct Marker {
    /// Path of the first matched file relative to the fuse root; its first component is the
    /// torrent folder.
    #[serde(default)]
    materialized_relative_path: String,
    #[serde(default)]
    tvdb_id: Option<u64>,
    #[serde(default)]
    tmdb_id: Option<u64>,
    #[serde(default)]
    media_title: Option<String>,
    #[serde(default)]
    rd_id: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct MarkerImport {
    /// Markers that yielded a pin.
    pub imported: usize,
    /// Pins whose folder was new or moved to another id.
    pub changed: usize,
    /// Markers without a tvdb/tmdb id (written by an older backfill-buddy, or the Arr lookup
    /// failed there); left in place so a later handoff can complete them.
    pub without_id: usize,
    /// Files that could not be read or parsed.
    pub unreadable: usize,
    /// Markers deleted after their pin was stored.
    pub consumed: usize,
    /// Folder names of imported pins.
    pub imported_folders: Vec<String>,
}

impl MarkerImport {
    pub fn summary_line(&self) -> String {
        let mut parts = vec![format!("{} pin(s) imported", self.imported)];
        if self.changed > 0 {
            parts.push(format!("{} new", self.changed));
        }
        if self.without_id > 0 {
            parts.push(format!("{} without id", self.without_id));
        }
        if self.unreadable > 0 {
            parts.push(format!("{} unreadable", self.unreadable));
        }
        if self.consumed > 0 {
            parts.push(format!("{} consumed", self.consumed));
        }
        parts.join(", ")
    }
}

/// One parsed marker: (folder, media id, note, file path).
type ParsedMarker = (String, MediaId, Option<String>, PathBuf);

/// Read every `*.json` marker in `dir`. Returns the parsed pins plus the counts for
/// markers that were skipped.
fn read_markers(dir: &Path) -> (Vec<ParsedMarker>, MarkerImport) {
    let mut import = MarkerImport::default();
    let mut pins = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            warn!("Handoff markers dir {} unreadable: {}", dir.display(), err);
            return (pins, import);
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_marker = path.extension().is_some_and(|ext| ext == "json")
            && !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'));
        if !is_marker || !path.is_file() {
            continue;
        }
        let marker: Marker = match fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
        {
            Ok(marker) => marker,
            Err(err) => {
                debug!("Skipping handoff marker {}: {}", path.display(), err);
                import.unreadable += 1;
                continue;
            }
        };
        let folder = marker
            .materialized_relative_path
            .split('/')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        if folder.is_empty() {
            import.unreadable += 1;
            continue;
        }
        let media_id = match (marker.tvdb_id, marker.tmdb_id) {
            (Some(id), _) => MediaId::Tvdb(id),
            (None, Some(id)) => MediaId::Tmdb(id),
            (None, None) => {
                import.without_id += 1;
                continue;
            }
        };
        let note = match (marker.media_title, marker.rd_id) {
            (Some(title), Some(rd)) => Some(format!("{title} ({rd})")),
            (Some(title), None) => Some(title),
            (None, Some(rd)) => Some(rd),
            (None, None) => None,
        };
        pins.push((folder, media_id, note, path));
    }
    (pins, import)
}

/// Import markers from `dir` into `source_pins`, then delete the imported files when
/// `consume` is set. Never fails the caller: unreadable markers and a missing directory are
/// counted and logged.
pub async fn import_markers(db: &Database, dir: &Path, consume: bool) -> MarkerImport {
    let (parsed, mut import) = read_markers(dir);
    if parsed.is_empty() {
        return import;
    }
    // Two markers for the same folder (a reinsert wrote a second one) must agree; the last
    // one read wins, which is fine because reinserts keep the hash and therefore the id.
    let mut by_folder: BTreeMap<String, (MediaId, Option<String>, Vec<PathBuf>)> = BTreeMap::new();
    for (folder, media_id, note, path) in parsed {
        let entry = by_folder
            .entry(folder)
            .or_insert_with(|| (media_id.clone(), note.clone(), Vec::new()));
        entry.0 = media_id;
        entry.1 = note;
        entry.2.push(path);
    }
    let rows: Vec<(String, String, String, Option<String>)> = by_folder
        .iter()
        .map(|(folder, (media_id, note, _))| {
            (
                folder.clone(),
                media_id.to_string(),
                "handoff".to_string(),
                note.clone(),
            )
        })
        .collect();
    import.imported = rows.len();
    import.imported_folders = by_folder.keys().cloned().collect();
    match db.upsert_source_pins(&rows).await {
        Ok(changed) => import.changed = changed,
        Err(err) => {
            warn!("Storing handoff pins failed: {}", err);
            import.imported = 0;
            return import;
        }
    }
    if consume {
        for path in by_folder.into_values().flat_map(|(_, _, paths)| paths) {
            match fs::remove_file(&path) {
                Ok(()) => import.consumed += 1,
                Err(err) => warn!(
                    "Handoff marker {} imported but could not be removed: {}",
                    path.display(),
                    err
                ),
            }
        }
    }
    import
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).unwrap();
    }

    #[tokio::test]
    async fn markers_become_pins_and_are_consumed() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::new(tmp.path().join("t.db").to_str().unwrap())
            .await
            .unwrap();
        let dir = tmp.path().join("markers");
        fs::create_dir(&dir).unwrap();
        write(
            &dir,
            "aaa.json",
            r#"{"materialized_relative_path":"Show.S01.Pack/Show.S01E01.mkv","tvdb_id":449988,"tmdb_id":null,"media_title":"Show S01","rd_id":"RD1"}"#,
        );
        write(
            &dir,
            "bbb.json",
            r#"{"materialized_relative_path":"Movie.2014/Movie.2014.mkv","tmdb_id":603}"#,
        );
        write(
            &dir,
            "old.json",
            r#"{"materialized_relative_path":"Legacy/x.mkv","media_title":"Legacy"}"#,
        );
        write(&dir, "broken.json", "{not json");
        write(&dir, ".hidden.json.tmp", "{}");
        write(&dir, "notes.txt", "ignored");

        let import = import_markers(&db, &dir, true).await;
        assert_eq!(
            import,
            MarkerImport {
                imported: 2,
                changed: 2,
                without_id: 1,
                unreadable: 1,
                consumed: 2,
                imported_folders: vec!["Movie.2014".to_string(), "Show.S01.Pack".to_string()],
            }
        );
        let pins = db.list_source_pins().await.unwrap();
        assert_eq!(pins.len(), 2);
        assert_eq!(pins[0].source_folder, "Movie.2014");
        assert_eq!(pins[0].media_id, "tmdb-603");
        assert_eq!(pins[1].media_id, "tvdb-449988");
        assert_eq!(pins[1].note.as_deref(), Some("Show S01 (RD1)"));
        assert_eq!(pins[1].origin, "handoff");
        // Consumed markers are gone; the id-less and broken ones stay for a later pass.
        assert!(!dir.join("aaa.json").exists());
        assert!(!dir.join("bbb.json").exists());
        assert!(dir.join("old.json").exists());
        assert!(dir.join("broken.json").exists());

        // Second pass: nothing left to import, nothing changes.
        let again = import_markers(&db, &dir, true).await;
        assert_eq!(again.imported, 0);
        assert_eq!(again.without_id, 1);
    }

    #[tokio::test]
    async fn keep_markers_when_not_consuming() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::new(tmp.path().join("t.db").to_str().unwrap())
            .await
            .unwrap();
        let dir = tmp.path().join("markers");
        fs::create_dir(&dir).unwrap();
        write(
            &dir,
            "aaa.json",
            r#"{"materialized_relative_path":"Show.S01.Pack/Show.S01E01.mkv","tvdb_id":1}"#,
        );
        let first = import_markers(&db, &dir, false).await;
        assert_eq!((first.imported, first.changed, first.consumed), (1, 1, 0));
        let second = import_markers(&db, &dir, false).await;
        assert_eq!((second.imported, second.changed), (1, 0));
        assert!(dir.join("aaa.json").exists());
    }

    #[tokio::test]
    async fn missing_dir_is_reported_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::new(tmp.path().join("t.db").to_str().unwrap())
            .await
            .unwrap();
        let import = import_markers(&db, &tmp.path().join("nope"), true).await;
        assert_eq!(import, MarkerImport::default());
    }
}

use anyhow::Result;
use tracing::{debug, info};

use crate::db::Database;
use crate::models::LibraryItem;
use crate::utils::normalize;

/// An item discovered in the RD cache that doesn't exist in the local library
#[derive(Debug, Clone)]
pub struct DiscoveredItem {
    /// RD torrent ID
    pub rd_torrent_id: String,
    /// Torrent name from RD
    #[allow(dead_code)]
    pub torrent_name: String,
    /// RD status (usually "downloaded")
    pub status: String,
    /// Size in bytes
    pub size: i64,
    /// Parsed title from the torrent name
    pub parsed_title: String,
}

/// Gap analysis: find content in RD that's not in the local library.
pub struct Discovery;

impl Discovery {
    pub fn new() -> Self {
        Self
    }

    /// Compare cached RD torrents against the library items.
    /// Returns items that exist in RD but have no corresponding library folder.
    /// Uses the local SQLite cache (populated by `TorrentCache::sync()`) instead
    /// of hitting the RD API, avoiding redundant requests and 429 errors.
    pub async fn find_gaps(
        &self,
        db: &Database,
        library_items: &[LibraryItem],
    ) -> Result<Vec<DiscoveredItem>> {
        info!("Loading RD torrents from cache...");
        let db_torrents = db.get_rd_torrents().await?;
        info!("Cache: {} torrents total", db_torrents.len());

        // Only consider downloaded/completed torrents
        let completed: Vec<_> = db_torrents
            .iter()
            .filter(|(_, _, _, status, _)| status == "downloaded")
            .collect();
        info!("Cache: {} completed torrents", completed.len());

        // Build a set of normalized library titles for comparison
        let library_titles: Vec<String> = library_items
            .iter()
            .map(|item| normalize(&item.title))
            .collect();

        let mut gaps = Vec::new();

        for (torrent_id, _hash, filename, status, files_json) in &completed {
            let parsed_title = self.parse_torrent_title(filename);
            let normalized = normalize(&parsed_title);

            // Check if this title matches any library item
            let found = library_titles
                .iter()
                .any(|lib_title| titles_match(lib_title, &normalized));

            if !found {
                // Compute total size from cached file list
                let size: i64 = serde_json::from_str::<serde_json::Value>(files_json)
                    .ok()
                    .and_then(|v| v.get("files")?.as_array().cloned())
                    .map(|files| files.iter().filter_map(|f| f.get("bytes")?.as_i64()).sum())
                    .unwrap_or(0);

                debug!(
                    "Gap found: '{}' (parsed: '{}') missing from library",
                    filename, parsed_title
                );
                gaps.push(DiscoveredItem {
                    rd_torrent_id: torrent_id.clone(),
                    torrent_name: filename.clone(),
                    status: status.clone(),
                    size,
                    parsed_title,
                });
            }
        }

        info!(
            "Discovery: {} of {} RD torrents missing from library",
            gaps.len(),
            completed.len()
        );

        Ok(gaps)
    }

    /// Parse a torrent name into a human-readable title.
    /// Strips quality tags, release groups, etc.
    fn parse_torrent_title(&self, torrent_name: &str) -> String {
        // Reuse source scanner's title extraction logic
        let cleaned = torrent_name.replace(['.', '_'], " ");

        // Cut at known quality/release markers
        let markers = [
            "1080p", "2160p", "720p", "480p", "4K", "BluRay", "WEB-DL", "WEBRip", "HDTV", "BDRip",
            "x264", "x265", "HEVC", "H 264", "H 265", "REMUX", "PROPER", "REPACK", "DTS", "AAC",
            "S01", "S02", "S03", "S04", "S05", "S06", "S07", "S08", "S09", "S10", "Season",
            "Complete",
        ];

        let mut cutoff = cleaned.len();
        for marker in &markers {
            if let Some(pos) = cleaned.to_lowercase().find(&marker.to_lowercase()) {
                cutoff = cutoff.min(pos);
            }
        }

        cleaned[..cutoff]
            .trim()
            .trim_end_matches(|c: char| c == '-' || c == '(' || c.is_whitespace())
            .to_string()
    }

    /// Print a summary of discovered gaps.
    pub fn print_summary(gaps: &[DiscoveredItem]) {
        if gaps.is_empty() {
            println!("✅ No new content found in RD cache that is missing from library.");
            return;
        }

        println!(
            "\n📦 {} items in RD cache missing from library:\n",
            gaps.len()
        );
        println!("{:<10} {:<45} {:>10} STATUS", "ID", "TITLE", "SIZE");
        println!("{}", "-".repeat(80));

        for item in gaps {
            let size_gb = item.size as f64 / 1_073_741_824.0;
            println!(
                "{:<10} {:<45} {:>8.1} GB {}",
                item.rd_torrent_id,
                truncate(&item.parsed_title, 43),
                size_gb,
                item.status,
            );
        }

        println!("\nUse 'symlinkarr discover add <id>' to add to Decypharr.");
    }
}

/// Check if two normalized titles match (exact or containment)
fn titles_match(lib_title: &str, rd_title: &str) -> bool {
    if lib_title == rd_title {
        return true;
    }
    // Empty strings should not match anything
    if lib_title.is_empty() || rd_title.is_empty() {
        return false;
    }
    // Check if one contains the other (for partial matches like "Breaking Bad" vs "Breaking Bad S01")
    lib_title.contains(rd_title) || rd_title.contains(lib_title)
}

/// Truncate a string to max_len characters with ellipsis
fn truncate(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len - 1).collect();
        format!("{truncated}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize() {
        assert_eq!(normalize("Breaking Bad"), "breaking bad");
        assert_eq!(normalize("The Matrix (1999)"), "the matrix 1999");
    }

    #[test]
    fn test_titles_match_exact() {
        assert!(titles_match("breaking bad", "breaking bad"));
    }

    #[test]
    fn test_titles_match_containment() {
        assert!(titles_match("breaking bad", "breaking bad s01"));
        assert!(titles_match("breaking bad s01", "breaking bad"));
    }

    #[test]
    fn test_titles_no_match() {
        assert!(!titles_match("breaking bad", "game of thrones"));
    }

    #[test]
    fn test_parse_torrent_title() {
        let discovery = Discovery::new();
        assert_eq!(
            discovery.parse_torrent_title("Breaking.Bad.S01.1080p.BluRay.x264"),
            "Breaking Bad"
        );
        assert_eq!(
            discovery.parse_torrent_title("The.Matrix.1999.2160p.WEB-DL"),
            "The Matrix 1999"
        );
        assert_eq!(
            discovery.parse_torrent_title("Dune.Part.Two.2024.REMUX.2160p"),
            "Dune Part Two 2024"
        );
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("Hello", 10), "Hello");
        assert_eq!(truncate("Hello World", 6), "Hello…");
    }

    #[test]
    fn test_titles_match_empty() {
        assert!(titles_match("", ""));
        assert!(!titles_match("", "something"));
        assert!(!titles_match("something", ""));
    }


    #[test]
    fn test_truncate_exact_boundary() {
        assert_eq!(truncate("Hello", 5), "Hello");
        assert_eq!(truncate("Hello", 4), "Hel…");
    }

    #[test]
    fn test_truncate_unicode() {
        // Unicode characters (each counts as one char in the truncate logic)
        assert_eq!(truncate("日本語テスト", 3), "日本…");
    }

    #[test]
    fn test_parse_torrent_title_empty() {
        let discovery = Discovery::new();
        assert_eq!(discovery.parse_torrent_title(""), "");
    }
}

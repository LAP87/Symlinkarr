use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use quick_xml::de::from_str;
use reqwest::Client;
use serde::Deserialize;

use crate::api::http;

pub struct PlexClient {
    client: Client,
    base_url: String,
    token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlexSection {
    pub key: String,
    pub title: String,
    pub locations: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlexRefreshBatch {
    pub section_key: String,
    pub section_title: String,
    pub refresh_path: PathBuf,
    pub covered_paths: usize,
    pub coalesced_to_root: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlexRefreshPlan {
    pub requested_paths: usize,
    pub unique_paths: usize,
    pub coalesced_paths: usize,
    pub coalesced_batches: usize,
    pub unresolved_paths: Vec<PathBuf>,
    pub batches: Vec<PlexRefreshBatch>,
}

#[derive(Debug, Deserialize)]
struct PlexSectionsResponse {
    #[serde(rename = "Directory", default)]
    directories: Vec<PlexSectionXml>,
}

#[derive(Debug, Deserialize)]
struct PlexSectionXml {
    #[serde(rename = "@key")]
    key: String,
    #[serde(rename = "@title", default)]
    title: String,
    #[serde(rename = "Location", default)]
    locations: Vec<PlexLocationXml>,
}

#[derive(Debug, Deserialize)]
struct PlexLocationXml {
    #[serde(rename = "@path")]
    path: String,
}

impl PlexClient {
    pub fn new(url: &str, token: &str) -> Self {
        Self {
            client: http::build_client(),
            base_url: url.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    pub async fn get_sections(&self) -> Result<Vec<PlexSection>> {
        let url = format!("{}/library/sections", self.base_url);
        let req = self.client.get(&url).header("X-Plex-Token", &self.token);
        let resp = http::send_with_retry(req).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Plex sections error {}: {}", status, body);
        }

        let xml = resp.text().await?;
        parse_sections_xml(&xml)
    }

    pub async fn refresh_path(&self, section_key: &str, path: &Path) -> Result<()> {
        let path = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Path is not valid UTF-8: {:?}", path))?;
        let url = format!("{}/library/sections/{}/refresh", self.base_url, section_key);
        let req = self
            .client
            .get(&url)
            .query(&[("path", path)])
            .header("X-Plex-Token", &self.token);
        let resp = http::send_with_retry(req).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Plex refresh error {}: {}", status, body);
        }

        Ok(())
    }
}

pub fn find_section_location_for_path<'a>(
    sections: &'a [PlexSection],
    path: &Path,
) -> Option<(&'a PlexSection, &'a PathBuf)> {
    sections
        .iter()
        .flat_map(|section| {
            section.locations.iter().filter_map(move |location| {
                path.starts_with(location).then_some((
                    location.components().count(),
                    section,
                    location,
                ))
            })
        })
        .max_by_key(|(len, _, _)| *len)
        .map(|(_, section, location)| (section, location))
}

pub fn plan_refresh_batches(
    sections: &[PlexSection],
    refresh_paths: &[PathBuf],
    max_paths_per_location: usize,
) -> PlexRefreshPlan {
    let mut unique_paths = refresh_paths.to_vec();
    unique_paths.sort();
    unique_paths.dedup();

    let mut plan = PlexRefreshPlan {
        requested_paths: refresh_paths.len(),
        unique_paths: unique_paths.len(),
        ..PlexRefreshPlan::default()
    };

    let threshold = max_paths_per_location.max(1);
    let mut grouped: HashMap<(String, PathBuf), (String, Vec<PathBuf>)> = HashMap::new();

    for path in unique_paths {
        let Some((section, location)) = find_section_location_for_path(sections, &path) else {
            plan.unresolved_paths.push(path);
            continue;
        };

        let entry = grouped
            .entry((section.key.clone(), location.clone()))
            .or_insert_with(|| (section.title.clone(), Vec::new()));
        entry.1.push(path);
    }

    let mut grouped_entries: Vec<_> = grouped.into_iter().collect();
    grouped_entries.sort_by(|a, b| {
        let ((section_key_a, location_a), (section_title_a, _paths_a)) = a;
        let ((section_key_b, location_b), (section_title_b, _paths_b)) = b;
        section_title_a
            .cmp(section_title_b)
            .then_with(|| section_key_a.cmp(section_key_b))
            .then_with(|| location_a.cmp(location_b))
    });

    for ((section_key, location), (section_title, mut paths)) in grouped_entries {
        paths.sort();
        paths.dedup();

        if paths.len() > threshold {
            plan.coalesced_batches += 1;
            plan.coalesced_paths += paths.len();
            plan.batches.push(PlexRefreshBatch {
                section_key,
                section_title,
                refresh_path: location,
                covered_paths: paths.len(),
                coalesced_to_root: true,
            });
            continue;
        }

        for path in paths {
            plan.batches.push(PlexRefreshBatch {
                section_key: section_key.clone(),
                section_title: section_title.clone(),
                refresh_path: path,
                covered_paths: 1,
                coalesced_to_root: false,
            });
        }
    }

    plan
}

fn parse_sections_xml(xml: &str) -> Result<Vec<PlexSection>> {
    let parsed: PlexSectionsResponse = from_str(xml)?;
    Ok(parsed
        .directories
        .into_iter()
        .map(|section| PlexSection {
            key: section.key,
            title: section.title,
            locations: section
                .locations
                .into_iter()
                .map(|location| PathBuf::from(location.path))
                .collect(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sections_xml_extracts_locations() {
        let xml = r#"
<MediaContainer size="2">
  <Directory key="1" title="Movies">
    <Location id="10" path="/mnt/storage/plex/film" />
  </Directory>
  <Directory key="2" title="Series">
    <Location id="11" path="/mnt/storage/plex/serier" />
    <Location id="12" path="/mnt/storage/plex/anime" />
  </Directory>
</MediaContainer>
"#;

        let sections = parse_sections_xml(xml).unwrap();
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].key, "1");
        assert_eq!(
            sections[0].locations,
            vec![PathBuf::from("/mnt/storage/plex/film")]
        );
        assert_eq!(sections[1].locations.len(), 2);
    }

    #[test]
    fn find_section_location_for_path_prefers_longest_matching_location() {
        let sections = vec![
            PlexSection {
                key: "1".to_string(),
                title: "Movies".to_string(),
                locations: vec![PathBuf::from("/mnt/storage/plex")],
            },
            PlexSection {
                key: "2".to_string(),
                title: "Anime".to_string(),
                locations: vec![PathBuf::from("/mnt/storage/plex/anime")],
            },
        ];

        let (section, location) = find_section_location_for_path(
            &sections,
            Path::new("/mnt/storage/plex/anime/Show {tvdb-1}"),
        )
        .unwrap();
        assert_eq!(section.key, "2");
        assert_eq!(location, &PathBuf::from("/mnt/storage/plex/anime"));
    }

    #[test]
    fn find_section_location_for_path_prefers_longest_location() {
        let sections = vec![
            PlexSection {
                key: "1".to_string(),
                title: "Series".to_string(),
                locations: vec![PathBuf::from("/mnt/storage/plex/serier")],
            },
            PlexSection {
                key: "2".to_string(),
                title: "Anime".to_string(),
                locations: vec![PathBuf::from("/mnt/storage/plex/serier/anime")],
            },
        ];

        let (_section, location) = find_section_location_for_path(
            &sections,
            Path::new("/mnt/storage/plex/serier/anime/Show {tvdb-1}"),
        )
        .unwrap();

        assert_eq!(location, &PathBuf::from("/mnt/storage/plex/serier/anime"));
    }

    #[test]
    fn plan_refresh_batches_keeps_small_groups_targeted() {
        let sections = vec![PlexSection {
            key: "7".to_string(),
            title: "Movies".to_string(),
            locations: vec![PathBuf::from("/mnt/storage/plex/film")],
        }];
        let refresh_paths = vec![
            PathBuf::from("/mnt/storage/plex/film/Movie A {tmdb-1}"),
            PathBuf::from("/mnt/storage/plex/film/Movie B {tmdb-2}"),
        ];

        let plan = plan_refresh_batches(&sections, &refresh_paths, 4);
        assert_eq!(plan.requested_paths, 2);
        assert_eq!(plan.unique_paths, 2);
        assert_eq!(plan.coalesced_batches, 0);
        assert_eq!(plan.unresolved_paths.len(), 0);
        assert_eq!(plan.batches.len(), 2);
        assert!(!plan.batches.iter().any(|batch| batch.coalesced_to_root));
    }

    #[test]
    fn plan_refresh_batches_coalesces_large_groups_to_root() {
        let sections = vec![PlexSection {
            key: "7".to_string(),
            title: "Movies".to_string(),
            locations: vec![PathBuf::from("/mnt/storage/plex/film")],
        }];
        let refresh_paths = vec![
            PathBuf::from("/mnt/storage/plex/film/Movie A {tmdb-1}"),
            PathBuf::from("/mnt/storage/plex/film/Movie B {tmdb-2}"),
            PathBuf::from("/mnt/storage/plex/film/Movie C {tmdb-3}"),
        ];

        let plan = plan_refresh_batches(&sections, &refresh_paths, 2);
        assert_eq!(plan.coalesced_batches, 1);
        assert_eq!(plan.coalesced_paths, 3);
        assert_eq!(plan.batches.len(), 1);
        assert_eq!(
            plan.batches[0],
            PlexRefreshBatch {
                section_key: "7".to_string(),
                section_title: "Movies".to_string(),
                refresh_path: PathBuf::from("/mnt/storage/plex/film"),
                covered_paths: 3,
                coalesced_to_root: true,
            }
        );
    }

    #[test]
    fn plan_refresh_batches_keeps_multi_location_sections_separate() {
        let sections = vec![PlexSection {
            key: "2".to_string(),
            title: "Series".to_string(),
            locations: vec![
                PathBuf::from("/mnt/storage/plex/serier"),
                PathBuf::from("/mnt/storage/plex/anime"),
            ],
        }];
        let refresh_paths = vec![
            PathBuf::from("/mnt/storage/plex/serier/Show A {tvdb-1}"),
            PathBuf::from("/mnt/storage/plex/serier/Show B {tvdb-2}"),
            PathBuf::from("/mnt/storage/plex/anime/Anime A {tvdb-3}"),
            PathBuf::from("/mnt/storage/plex/anime/Anime B {tvdb-4}"),
        ];

        let plan = plan_refresh_batches(&sections, &refresh_paths, 1);
        assert_eq!(plan.batches.len(), 2);
        assert_eq!(plan.coalesced_batches, 2);
        assert_eq!(
            plan.batches
                .iter()
                .map(|batch| batch.refresh_path.clone())
                .collect::<Vec<_>>(),
            vec![
                PathBuf::from("/mnt/storage/plex/anime"),
                PathBuf::from("/mnt/storage/plex/serier"),
            ]
        );
    }
}

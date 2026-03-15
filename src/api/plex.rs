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

pub fn find_section_for_path<'a>(
    sections: &'a [PlexSection],
    path: &Path,
) -> Option<&'a PlexSection> {
    sections
        .iter()
        .filter_map(|section| {
            let best_len = section
                .locations
                .iter()
                .filter(|location| path.starts_with(location))
                .map(|location| location.components().count())
                .max()?;
            Some((best_len, section))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, section)| section)
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
    fn find_section_for_path_prefers_longest_matching_location() {
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

        let section = find_section_for_path(
            &sections,
            Path::new("/mnt/storage/plex/anime/Show {tvdb-1}"),
        )
        .unwrap();
        assert_eq!(section.key, "2");
    }
}

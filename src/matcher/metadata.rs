use std::sync::Arc;

use anyhow::Result;
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::warn;

use crate::api::tmdb::TmdbClient;
use crate::api::tvdb::TvdbClient;
use crate::config::MetadataMode;
use crate::db::Database;
use crate::models::{ContentMetadata, LibraryItem, MediaId, MediaType};

#[derive(Debug, Deserialize)]
struct CachedMetadataEnvelope {
    #[serde(default)]
    _symlinkarr_not_found: bool,
    title: String,
    #[serde(default)]
    aliases: Vec<String>,
    year: Option<u32>,
    #[serde(default)]
    seasons: Vec<crate::models::SeasonInfo>,
}

enum MetadataCacheState {
    Miss,
    Hit(ContentMetadata),
    NegativeHit,
}

pub(crate) async fn fetch_metadata_static(
    tmdb: &Option<TmdbClient>,
    tvdb: Option<&Arc<Mutex<TvdbClient>>>,
    metadata_mode: MetadataMode,
    item: &LibraryItem,
    db: &Database,
) -> Result<Option<ContentMetadata>> {
    match metadata_mode {
        MetadataMode::Off => Ok(None),
        MetadataMode::CacheOnly => match fetch_cached_metadata_static(item, db).await? {
            MetadataCacheState::Hit(metadata) => Ok(Some(metadata)),
            MetadataCacheState::Miss | MetadataCacheState::NegativeHit => Ok(None),
        },
        MetadataMode::Full => {
            match fetch_cached_metadata_static(item, db).await? {
                MetadataCacheState::Hit(metadata) => return Ok(Some(metadata)),
                MetadataCacheState::NegativeHit => return Ok(None),
                MetadataCacheState::Miss => {}
            }
            fetch_remote_metadata_static(tmdb, tvdb, metadata_mode, item, db).await
        }
    }
}

async fn fetch_cached_metadata_static(
    item: &LibraryItem,
    db: &Database,
) -> Result<MetadataCacheState> {
    let cache_key = match (&item.id, item.media_type) {
        (MediaId::Tmdb(id), MediaType::Tv) => format!("tmdb:tv:{}", id),
        (MediaId::Tmdb(id), MediaType::Movie) => format!("tmdb:movie:{}", id),
        (MediaId::Tvdb(id), _) => format!("tvdb:series:{}", id),
    };

    let Some(cached) = db.get_cached(&cache_key).await? else {
        return Ok(MetadataCacheState::Miss);
    };

    match serde_json::from_str::<CachedMetadataEnvelope>(&cached) {
        Ok(envelope) if envelope._symlinkarr_not_found => Ok(MetadataCacheState::NegativeHit),
        Ok(envelope) => Ok(MetadataCacheState::Hit(ContentMetadata {
            title: envelope.title,
            aliases: envelope.aliases,
            year: envelope.year,
            seasons: envelope.seasons,
        })),
        Err(err) => {
            warn!(
                "Metadata cache decode failed for key {} ({}); ignoring cache entry",
                cache_key, err
            );
            let _ = db.invalidate_cached(&cache_key).await;
            Ok(MetadataCacheState::Miss)
        }
    }
}

async fn fetch_remote_metadata_static(
    tmdb: &Option<TmdbClient>,
    tvdb: Option<&Arc<Mutex<TvdbClient>>>,
    metadata_mode: MetadataMode,
    item: &LibraryItem,
    db: &Database,
) -> Result<Option<ContentMetadata>> {
    if !metadata_mode.allows_network() {
        return Ok(None);
    }

    match &item.id {
        MediaId::Tmdb(id) => {
            if let Some(ref tmdb) = tmdb {
                let metadata = match item.media_type {
                    MediaType::Tv => tmdb.get_tv_metadata(*id, db).await?,
                    MediaType::Movie => tmdb.get_movie_metadata(*id, db).await?,
                };
                return Ok(Some(metadata));
            }
        }
        MediaId::Tvdb(tvdb_id) => {
            if let Some(tvdb_mutex) = tvdb {
                let mut tvdb = tvdb_mutex.lock().await;
                let metadata = tvdb.get_series_metadata(*tvdb_id, db).await?;
                return Ok(Some(metadata));
            } else {
                warn!(
                    "TVDB metadata requested for {} but no TVDB client configured",
                    tvdb_id
                );
            }
        }
    }

    Ok(None)
}

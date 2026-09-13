use anyhow::Result;
use serde::Serialize;

use crate::config::Config;
use crate::db::Database;
use crate::models::MediaId;
use crate::{OutputFormat, PinAction};

#[derive(Serialize)]
struct PinRow<'a> {
    folder: &'a str,
    media_id: &'a str,
    origin: &'a str,
    note: Option<&'a str>,
    updated_at: &'a str,
}

pub(crate) async fn run_pin(cfg: &Config, db: &Database, action: PinAction) -> Result<()> {
    match action {
        PinAction::List { output } => {
            let pins = db.list_source_pins().await?;
            match output {
                OutputFormat::Json => {
                    let rows: Vec<PinRow<'_>> = pins
                        .iter()
                        .map(|pin| PinRow {
                            folder: &pin.source_folder,
                            media_id: &pin.media_id,
                            origin: &pin.origin,
                            note: pin.note.as_deref(),
                            updated_at: &pin.updated_at,
                        })
                        .collect();
                    super::print_json(&rows);
                }
                OutputFormat::Text => {
                    if pins.is_empty() {
                        println!("No source pins stored.");
                    }
                    for pin in &pins {
                        let note = pin
                            .note
                            .as_deref()
                            .map(|n| format!("  # {n}"))
                            .unwrap_or_default();
                        println!(
                            "{:<12} {:<8} {}{}",
                            pin.media_id, pin.origin, pin.source_folder, note
                        );
                    }
                }
            }
        }
        PinAction::Add {
            folder,
            media_id,
            note,
        } => {
            let folder = folder.trim().trim_matches('/').to_string();
            if folder.is_empty() || folder.contains('/') {
                anyhow::bail!(
                    "folder must be a single directory name under the source root, not a path"
                );
            }
            let Some(id) = MediaId::parse(&media_id) else {
                anyhow::bail!("media id must look like tvdb-123 or tmdb-123, got {media_id:?}");
            };
            let changed = db
                .upsert_source_pins(&[(folder.clone(), id.to_string(), "manual".to_string(), note)])
                .await?;
            if changed > 0 {
                println!("Pinned {folder:?} → {id}. It applies on the next scan.");
            } else {
                println!("{folder:?} was already pinned to {id}.");
            }
        }
        PinAction::Remove { folder } => {
            if db.delete_source_pin(folder.trim()).await? {
                println!("Removed pin for {folder:?}.");
            } else {
                println!("No pin stored for {folder:?}.");
            }
        }
        PinAction::Import { dir, keep } => {
            let Some(dir) = dir.or_else(|| cfg.handoff.markers_dir.clone()) else {
                anyhow::bail!("no directory given and handoff.markers_dir is not configured");
            };
            let consume = cfg.handoff.consume_markers && !keep;
            let import = crate::handoff::import_markers(db, &dir, consume).await;
            println!(
                "Handoff markers in {}: {}",
                dir.display(),
                import.summary_line()
            );
        }
    }
    Ok(())
}

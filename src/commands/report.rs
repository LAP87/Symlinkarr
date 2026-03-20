use anyhow::Result;
use chrono::Utc;
use serde::Serialize;

use crate::commands::{panel_border, panel_kv_row, panel_title};
use crate::config::Config;
use crate::db::{Database, LibraryStats, MediaTypeStats};
use crate::models::MediaType;
use crate::OutputFormat;

// ─── Data models for report output ─────────────────────────────────

#[derive(Serialize)]
struct Summary {
    total_library_items: i64,
    items_with_symlinks: i64,
    broken_symlinks: i64,
    missing_from_rd: i64,
}

#[derive(Serialize)]
struct MediaTypeInfo {
    library_items: i64,
    linked: i64,
    broken: i64,
}

#[derive(Serialize)]
struct LibraryInfo {
    name: String,
    items: i64,
    linked: i64,
    broken: i64,
}

#[derive(Serialize)]
struct ReportOutput {
    generated_at: String,
    summary: Summary,
    by_media_type: std::collections::BTreeMap<String, MediaTypeInfo>,
    top_libraries: Vec<LibraryInfo>,
}

// ─── Core logic ───────────────────────────────────────────────────

pub(crate) async fn run_report(
    cfg: &Config,
    db: &Database,
    output_format: OutputFormat,
    filter: Option<MediaType>,
    pretty: bool,
) -> Result<()> {
    let (active, dead, total) = db.get_stats().await?;
    let media_type_stats = db.get_stats_by_media_type().await?;
    let library_stats = db.get_stats_by_library().await?;

    let generated_at = Utc::now().to_rfc3339();

    let summary = Summary {
        total_library_items: total,
        items_with_symlinks: active,
        broken_symlinks: dead,
        missing_from_rd: total.saturating_sub(active).saturating_sub(dead),
    };

    let mut by_media_type: std::collections::BTreeMap<String, MediaTypeInfo> =
        std::collections::BTreeMap::new();
    for stat in &media_type_stats {
        let key = match stat.media_type.as_str() {
            "movie" => "movie".to_string(),
            _ => "series".to_string(),
        };
        // Apply filter
        if let Some(ref f) = filter {
            let stat_type = if stat.media_type == "movie" {
                MediaType::Movie
            } else {
                MediaType::Tv
            };
            if stat_type != *f {
                continue;
            }
        }
        by_media_type.insert(
            key,
            MediaTypeInfo {
                library_items: stat.library_items,
                linked: stat.linked,
                broken: stat.broken,
            },
        );
    }

    let mut top_libraries: Vec<LibraryInfo> = library_stats
        .iter()
        .map(|stat| LibraryInfo {
            name: stat.name.clone(),
            items: stat.library_items,
            linked: stat.linked,
            broken: stat.broken,
        })
        .collect();

    top_libraries.sort_by(|a, b| b.items.cmp(&a.items));
    top_libraries.truncate(10);

    let report = ReportOutput {
        generated_at,
        summary,
        by_media_type,
        top_libraries,
    };

    match output_format {
        OutputFormat::Json => {
            if pretty {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .unwrap_or_else(|e| format!(r#"{{"error":"{}"}}"#, e))
                );
            } else {
                println!("{}", serde_json::to_string(&report).unwrap_or_default());
            }
        }
        OutputFormat::Text => {
            emit_text_report(&report);
        }
    }

    Ok(())
}

fn emit_text_report(report: &ReportOutput) {
    println!();
    panel_border('╔', '═', '╗');
    panel_title("Symlinkarr Report");
    panel_border('╠', '═', '╣');
    panel_kv_row("  Total library items:", report.summary.total_library_items);
    panel_kv_row("  Items with symlinks:", report.summary.items_with_symlinks);
    panel_kv_row("  Broken symlinks:", report.summary.broken_symlinks);
    panel_kv_row("  Missing from RD:", report.summary.missing_from_rd);

    if !report.by_media_type.is_empty() {
        panel_border('╠', '═', '╣');
        panel_title("By Media Type");
        panel_border('╠', '═', '╣');
        for (media_type, info) in &report.by_media_type {
            let label = format!("  {}:", capitalize(media_type));
            let value = format!("{} items ({} linked, {} broken)", info.library_items, info.linked, info.broken);
            panel_kv_row(&label, value);
        }
    }

    if !report.top_libraries.is_empty() {
        panel_border('╠', '═', '╣');
        panel_title("Top Libraries");
        panel_border('╠', '═', '╣');
        for lib in &report.top_libraries {
            let label = format!("  {}:", lib.name);
            panel_kv_row(&label, format!("{} items ({} linked, {} broken)", lib.items, lib.linked, lib.broken));
        }
    }

    panel_border('╚', '═', '╝');
    println!();
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().chain(c).collect(),
    }
}

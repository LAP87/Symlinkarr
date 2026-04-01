use std::future::Future;

use anyhow::Result;

use crate::api::bazarr::BazarrClient;
use crate::api::prowlarr::ProwlarrClient;
use crate::api::tautulli::TautulliClient;
use crate::commands::{panel_border, panel_kv_row, panel_title, print_json};
use crate::config::Config;
use crate::db::Database;
use crate::media_servers::{
    configured_media_servers, configured_refresh_backends, display_server_list, probe_media_server,
    MediaServerKind,
};
use crate::OutputFormat;

pub(crate) async fn run_status(
    cfg: &Config,
    db: &Database,
    health: bool,
    output: OutputFormat,
) -> Result<()> {
    let (active, dead, total) = db.get_stats().await?;
    let acquisition = db.get_acquisition_job_counts().await?;
    let acquisition_json = serde_json::json!({
        "active": acquisition.active_total(),
        "queued": acquisition.queued,
        "downloading": acquisition.downloading,
        "relinking": acquisition.relinking,
        "blocked": acquisition.blocked,
        "no_result": acquisition.no_result,
        "failed": acquisition.failed,
        "completed_unlinked": acquisition.completed_unlinked,
    });
    let emit_text = output != OutputFormat::Json;

    if !health && !emit_text {
        print_json(&serde_json::json!({
            "active": active,
            "dead": dead,
            "total": total,
            "acquisition": acquisition_json,
        }));
        return Ok(());
    }

    if emit_text {
        panel_border('╔', '═', '╗');
        panel_title("Symlinkarr Status");
        panel_border('╠', '═', '╣');
        panel_kv_row("  Active symlinks:", active);
        panel_kv_row("  Dead links:", dead);
        panel_kv_row("  Total:", total);
        panel_border('╠', '═', '╣');
        panel_kv_row("  Auto-acquire active:", acquisition.active_total());
        if acquisition.active_total() > 0 {
            panel_kv_row("  Queued:", acquisition.queued);
            panel_kv_row("  Downloading:", acquisition.downloading);
            panel_kv_row("  Relinking:", acquisition.relinking);
            panel_kv_row("  Blocked:", acquisition.blocked);
            panel_kv_row("  No result:", acquisition.no_result);
            panel_kv_row("  Failed:", acquisition.failed);
            panel_kv_row("  Unlinked:", acquisition.completed_unlinked);
        }
        panel_border('╚', '═', '╝');
    }

    if health {
        let refresh_backends = configured_refresh_backends(cfg);
        let refresh_backend_keys = refresh_backends
            .iter()
            .map(|server| server.service_key().to_string())
            .collect::<Vec<_>>();
        let mut health_json = Vec::new();
        if emit_text {
            println!("\n🏥 Health Check:");
            if refresh_backends.is_empty() {
                println!("   🔁 Active refresh backends: none");
            } else {
                println!(
                    "   🔁 Active refresh backends: {}",
                    display_server_list(&refresh_backends)
                );
            }
        }

        check_tautulli(cfg, emit_text, &mut health_json).await;
        check_media_servers(cfg, emit_text, &mut health_json).await;
        check_service(
            "Prowlarr",
            cfg.has_prowlarr(),
            emit_text,
            &mut health_json,
            || async {
                ProwlarrClient::new(&cfg.prowlarr)
                    .get_system_status()
                    .await
                    .map(|_| ())
            },
        )
        .await;
        check_service(
            "Bazarr",
            cfg.has_bazarr(),
            emit_text,
            &mut health_json,
            || async { BazarrClient::new(&cfg.bazarr).health_check().await },
        )
        .await;
        check_service(
            "Radarr",
            cfg.has_radarr(),
            emit_text,
            &mut health_json,
            || async {
                crate::api::radarr::RadarrClient::new(&cfg.radarr.url, &cfg.radarr.api_key)
                    .get_system_status()
                    .await
                    .map(|_| ())
            },
        )
        .await;
        check_service(
            "Sonarr",
            cfg.has_sonarr(),
            emit_text,
            &mut health_json,
            || async {
                crate::api::sonarr::SonarrClient::new(&cfg.sonarr.url, &cfg.sonarr.api_key)
                    .get_system_status()
                    .await
                    .map(|_| ())
            },
        )
        .await;
        check_service(
            "Sonarr-Anime",
            cfg.has_sonarr_anime(),
            emit_text,
            &mut health_json,
            || async {
                crate::api::sonarr::SonarrClient::new(
                    &cfg.sonarr_anime.url,
                    &cfg.sonarr_anime.api_key,
                )
                .get_system_status()
                .await
                .map(|_| ())
            },
        )
        .await;

        if !emit_text {
            print_json(&serde_json::json!({
                "active": active,
                "dead": dead,
                "total": total,
                "acquisition": acquisition_json,
                "refresh_backends": refresh_backend_keys,
                "health": health_json,
            }));
        }
    }

    Ok(())
}

async fn check_service<F, Fut>(
    name: &str,
    configured: bool,
    emit_text: bool,
    health_json: &mut Vec<serde_json::Value>,
    probe: F,
) where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<()>>,
{
    if !configured {
        if emit_text {
            println!("   ⚪ {}: Not configured", name);
        }
        health_json.push(serde_json::json!({ "service": name.to_lowercase().replace('-', "_"), "configured": false }));
        return;
    }

    let svc_key = name.to_lowercase().replace('-', "_");
    match probe().await {
        Ok(()) => {
            if emit_text {
                println!("   ✅ {}: Connected", name);
            }
            health_json.push(serde_json::json!({ "service": svc_key, "ok": true }));
        }
        Err(e) => {
            if emit_text {
                println!("   ❌ {}: Connection error ({})", name, e);
            }
            health_json.push(serde_json::json!({
                "service": svc_key, "ok": false, "error": e.to_string(),
            }));
        }
    }
}

async fn check_tautulli(cfg: &Config, emit_text: bool, health_json: &mut Vec<serde_json::Value>) {
    if !cfg.has_tautulli() {
        if emit_text {
            println!("   ⚪ Tautulli: Not configured");
        }
        health_json.push(serde_json::json!({ "service": "tautulli", "configured": false }));
        return;
    }

    let tautulli = TautulliClient::new(&cfg.tautulli);
    match tautulli.get_activity().await {
        Ok(response) => {
            let stream_count = response.stream_count;
            if emit_text {
                println!(
                    "   ✅ Tautulli: Connected ({} active streams)",
                    stream_count
                );
            }
            health_json.push(serde_json::json!({
                "service": "tautulli", "ok": true, "streams": stream_count,
            }));
            if emit_text {
                if let Ok(history) = tautulli.get_history(10, None).await {
                    println!(
                        "      Recent activity: {} entries fetched",
                        history.data.len()
                    );
                }
            }
        }
        Err(e) => {
            if emit_text {
                println!("   ❌ Tautulli: Connection error ({})", e);
            }
            health_json.push(serde_json::json!({
                "service": "tautulli", "ok": false, "error": e.to_string(),
            }));
        }
    }
}

async fn check_media_servers(
    cfg: &Config,
    emit_text: bool,
    health_json: &mut Vec<serde_json::Value>,
) {
    let configured = configured_media_servers(cfg);
    for server in [
        MediaServerKind::Plex,
        MediaServerKind::Emby,
        MediaServerKind::Jellyfin,
    ] {
        if !configured.contains(&server) {
            if emit_text {
                println!("   ⚪ {}: Not configured", server);
            }
            health_json
                .push(serde_json::json!({ "service": server.service_key(), "configured": false }));
            continue;
        }

        match probe_media_server(cfg, server).await {
            Some(Ok(collections)) => {
                if emit_text {
                    println!(
                        "   ✅ {}: Connected ({} {})",
                        server,
                        collections,
                        collection_label(server, collections)
                    );
                }
                health_json.push(serde_json::json!({
                    "service": server.service_key(),
                    "ok": true,
                    "collections": collections,
                }));
            }
            Some(Err(e)) => {
                if emit_text {
                    println!("   ❌ {}: Connection error ({})", server, e);
                }
                health_json.push(serde_json::json!({
                    "service": server.service_key(), "ok": false, "error": e.to_string(),
                }));
            }
            None => {
                if emit_text {
                    println!("   ⚪ {}: Not configured", server);
                }
                health_json.push(
                    serde_json::json!({ "service": server.service_key(), "configured": false }),
                );
            }
        }
    }
}

fn collection_label(server: MediaServerKind, count: usize) -> &'static str {
    match server {
        MediaServerKind::Plex => {
            if count == 1 {
                "section"
            } else {
                "sections"
            }
        }
        MediaServerKind::Emby | MediaServerKind::Jellyfin => {
            if count == 1 {
                "library"
            } else {
                "libraries"
            }
        }
    }
}

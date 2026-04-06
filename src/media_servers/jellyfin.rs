use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Result};
use reqwest::{Client, RequestBuilder};
use serde::Serialize;
use tokio::time::sleep;

use crate::api::http::{build_client, send_with_retry};
use crate::config::{Config, MediaBrowserConfig};
use crate::utils::user_println;

use super::LibraryRefreshTelemetry;

const CONSECUTIVE_FAILURES_BEFORE_ABORT: usize = 2;

#[derive(Debug, Serialize)]
struct MediaUpdateInfo {
    #[serde(rename = "Updates")]
    updates: Vec<MediaUpdatePath>,
}

#[derive(Debug, Serialize)]
struct MediaUpdatePath {
    #[serde(rename = "Path")]
    path: String,
    #[serde(rename = "UpdateType")]
    update_type: &'static str,
}

fn emit_refresh_line(emit_text: bool, message: impl AsRef<str>) {
    if emit_text {
        user_println(message);
    }
}

fn authenticated_request(builder: RequestBuilder, api_key: &str) -> RequestBuilder {
    builder
        .query(&[("api_key", api_key)])
        .header("X-Emby-Token", api_key)
        .header("X-MediaBrowser-Token", api_key)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("MediaBrowser Token=\"{}\"", api_key),
        )
}

fn trim_base(url: &str) -> &str {
    url.trim_end_matches('/')
}

async fn request_library_count(client: &Client, cfg: &MediaBrowserConfig) -> Result<usize> {
    let url = format!("{}/Library/MediaFolders", trim_base(&cfg.url));
    let resp = send_with_retry(authenticated_request(
        client.get(&url).query(&[("isHidden", "false")]),
        &cfg.api_key,
    ))
    .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("Jellyfin error {}: {}", status, body);
    }
    let value: serde_json::Value = resp.json().await?;
    Ok(parse_folder_count(&value))
}

fn parse_folder_count(value: &serde_json::Value) -> usize {
    value
        .as_array()
        .map(std::vec::Vec::len)
        .or_else(|| {
            value
                .get("Items")
                .and_then(serde_json::Value::as_array)
                .map(std::vec::Vec::len)
        })
        .unwrap_or(0)
}

fn build_update_batches(refresh_paths: &[PathBuf], batch_size: usize) -> Vec<Vec<PathBuf>> {
    let mut unique = refresh_paths
        .iter()
        .filter(|path| !path.as_os_str().is_empty())
        .cloned()
        .collect::<Vec<_>>();
    unique.sort();
    unique.dedup();

    let batch_size = batch_size.max(1);
    unique
        .chunks(batch_size)
        .map(|chunk| chunk.to_vec())
        .collect()
}

fn update_payload(paths: &[PathBuf]) -> MediaUpdateInfo {
    MediaUpdateInfo {
        updates: paths
            .iter()
            .map(|path| MediaUpdatePath {
                path: path.display().to_string(),
                update_type: "Modified",
            })
            .collect(),
    }
}

fn enforce_batch_limit(
    mut batches: Vec<Vec<PathBuf>>,
    max_batches: usize,
) -> (Vec<Vec<PathBuf>>, usize) {
    if max_batches == 0 || batches.len() <= max_batches {
        return (batches, 0);
    }

    let dropped = batches.len().saturating_sub(max_batches);
    batches.truncate(max_batches);
    (batches, dropped)
}

async fn post_media_updates(
    client: &Client,
    cfg: &MediaBrowserConfig,
    paths: &[PathBuf],
) -> Result<()> {
    let url = format!("{}/Library/Media/Updated", trim_base(&cfg.url));
    let payload = update_payload(paths);
    let resp = send_with_retry(authenticated_request(
        client.post(&url).json(&payload),
        &cfg.api_key,
    ))
    .await?;
    if resp.status().is_success() {
        return Ok(());
    }

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    bail!("Jellyfin error {}: {}", status, body);
}

pub(crate) async fn probe_libraries(cfg: &Config) -> Result<usize> {
    let client = build_client();
    request_library_count(&client, &cfg.jellyfin).await
}

pub(crate) async fn refresh_library_paths(
    cfg: &Config,
    refresh_paths: &[PathBuf],
    emit_text: bool,
) -> Result<LibraryRefreshTelemetry> {
    let mut telemetry = LibraryRefreshTelemetry {
        requested_paths: refresh_paths.len(),
        ..LibraryRefreshTelemetry::default()
    };
    if refresh_paths.is_empty() || !cfg.has_jellyfin_refresh() {
        return Ok(telemetry);
    }

    let batches = build_update_batches(refresh_paths, cfg.jellyfin.refresh_batch_size);
    let (batches, dropped_batches) =
        enforce_batch_limit(batches, cfg.jellyfin.max_refresh_batches_per_run);

    telemetry.unique_paths = batches.iter().map(std::vec::Vec::len).sum::<usize>();
    telemetry.planned_batches = batches.len() + dropped_batches;
    telemetry.capped_batches = dropped_batches;

    if dropped_batches > 0 {
        if cfg.jellyfin.abort_refresh_when_capped {
            telemetry.aborted_due_to_cap = true;
            telemetry.skipped_batches += telemetry.planned_batches;
            emit_refresh_line(
                emit_text,
                format!(
                    "   ⚠️  Jellyfin: invalidation plan needed {} request(s), exceeding cap {}. Aborted all invalidation requests.",
                    telemetry.planned_batches, cfg.jellyfin.max_refresh_batches_per_run
                ),
            );
            return Ok(telemetry);
        }

        telemetry.skipped_batches += dropped_batches;
        emit_refresh_line(
            emit_text,
            format!(
                "   ⚠️  Jellyfin: capped invalidation plan at {} request(s); {} request(s) skipped",
                cfg.jellyfin.max_refresh_batches_per_run, dropped_batches
            ),
        );
    }

    let client = build_client();
    let delay = Duration::from_millis(cfg.jellyfin.refresh_delay_ms);
    let batch_count = batches.len();
    let mut consecutive_failures = 0usize;

    for (idx, batch) in batches.into_iter().enumerate() {
        match post_media_updates(&client, &cfg.jellyfin, &batch).await {
            Ok(()) => {
                consecutive_failures = 0;
                telemetry.refreshed_batches += 1;
                telemetry.refreshed_paths_covered += batch.len();
            }
            Err(err) => {
                emit_refresh_line(
                    emit_text,
                    format!(
                        "   ⚠️  Jellyfin: invalidation failed for {} path(s): {}",
                        batch.len(),
                        err
                    ),
                );
                consecutive_failures += 1;
                telemetry.failed_batches += 1;
                telemetry.skipped_batches += 1;

                if consecutive_failures >= CONSECUTIVE_FAILURES_BEFORE_ABORT
                    && idx + 1 < batch_count
                {
                    let remaining = batch_count - idx - 1;
                    telemetry.skipped_batches += remaining;
                    emit_refresh_line(
                        emit_text,
                        format!(
                            "   ⚠️  Jellyfin: stopping remaining invalidation after {} consecutive batch failures; {} request(s) left unqueued",
                            consecutive_failures, remaining
                        ),
                    );
                    break;
                }
            }
        }

        if delay > Duration::ZERO && idx + 1 < batch_count {
            sleep(delay).await;
        }
    }

    if telemetry.refreshed_batches > 0 {
        emit_refresh_line(
            emit_text,
            format!(
                "   📺 Jellyfin: targeted invalidation queued for {} request(s) covering {} path(s)",
                telemetry.refreshed_batches, telemetry.refreshed_paths_covered
            ),
        );
    }
    if telemetry.skipped_batches > 0 {
        emit_refresh_line(
            emit_text,
            format!(
                "   ⚠️  Jellyfin: {} invalidation request(s) were not queued",
                telemetry.skipped_batches
            ),
        );
    }

    Ok(telemetry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_update_batches_dedupes_and_chunks_paths() {
        let batches = build_update_batches(
            &[
                PathBuf::from("/library/a"),
                PathBuf::from("/library/a"),
                PathBuf::from("/library/b"),
                PathBuf::from("/library/c"),
            ],
            2,
        );

        assert_eq!(batches.len(), 2);
        assert_eq!(
            batches[0],
            vec![PathBuf::from("/library/a"), PathBuf::from("/library/b")]
        );
        assert_eq!(batches[1], vec![PathBuf::from("/library/c")]);
    }

    #[test]
    fn enforce_batch_limit_truncates_large_plans() {
        let batches = vec![
            vec![PathBuf::from("/library/a")],
            vec![PathBuf::from("/library/b")],
            vec![PathBuf::from("/library/c")],
        ];

        let (limited, dropped) = enforce_batch_limit(batches, 2);
        assert_eq!(limited.len(), 2);
        assert_eq!(dropped, 1);
    }

    #[test]
    fn parse_folder_count_handles_array_and_items_object() {
        let array = serde_json::json!([{"Name": "Movies"}, {"Name": "Series"}]);
        assert_eq!(parse_folder_count(&array), 2);

        let object = serde_json::json!({"Items": [{"Name": "Movies"}]});
        assert_eq!(parse_folder_count(&object), 1);
    }
}

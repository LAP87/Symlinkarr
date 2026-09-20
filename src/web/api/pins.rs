use super::*;
use crate::models::MediaId;
use axum::body::Bytes;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ApiPinItem {
    pub folder: String,
    pub media_id: String,
    pub origin: String,
    pub note: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ApiCreatePinRequest {
    pub folder: String,
    pub media_id: String,
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct ApiCreatePinResponse {
    pub folder: String,
    pub media_id: String,
    pub note: Option<String>,
    pub changed: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct ApiDeletePinResponse {
    pub folder: String,
    pub deleted: bool,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ApiImportPinsRequest {
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub keep: bool,
    #[serde(default)]
    pub link_now: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct ApiImportPinsResponse {
    pub directory: String,
    pub imported: usize,
    pub changed: usize,
    pub without_id: usize,
    pub unreadable: usize,
    pub consumed: usize,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scan_job: Option<String>,
}

/// GET /api/v1/pins
pub(super) async fn api_get_pins(State(state): State<WebState>) -> Response {
    match state.database.list_source_pins().await {
        Ok(pins) => {
            let items: Vec<ApiPinItem> = pins
                .into_iter()
                .map(|p| ApiPinItem {
                    folder: p.source_folder,
                    media_id: p.media_id,
                    origin: p.origin,
                    note: p.note,
                    updated_at: p.updated_at,
                })
                .collect();
            Json(items).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /api/v1/pins
pub(super) async fn api_post_pin(
    State(state): State<WebState>,
    Json(body): Json<ApiCreatePinRequest>,
) -> Response {
    let folder = body.folder.trim().trim_matches('/').to_string();
    if folder.is_empty() || folder.contains('/') {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "folder must be a single directory name under the source root, not a path"
            })),
        )
            .into_response();
    }
    let Some(id) = MediaId::parse(&body.media_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("media id must look like tvdb-123 or tmdb-123, got {:?}", body.media_id)
            })),
        )
            .into_response();
    };
    let note = body
        .note
        .as_ref()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    match state
        .database
        .upsert_source_pins(&[(
            folder.clone(),
            id.to_string(),
            "manual".to_string(),
            note.clone(),
        )])
        .await
    {
        Ok(changed) => Json(ApiCreatePinResponse {
            folder,
            media_id: id.to_string(),
            note,
            changed: changed > 0,
        })
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// DELETE /api/v1/pins/{folder}
pub(super) async fn api_delete_pin(
    State(state): State<WebState>,
    Path(folder): Path<String>,
) -> Response {
    let folder = folder.trim();
    match state.database.delete_source_pin(folder).await {
        Ok(deleted) => Json(ApiDeletePinResponse {
            folder: folder.to_string(),
            deleted,
        })
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /api/v1/pins/import
pub(super) async fn api_post_pins_import(State(state): State<WebState>, bytes: Bytes) -> Response {
    let req: ApiImportPinsRequest = if bytes.is_empty() {
        ApiImportPinsRequest::default()
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(r) => r,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": format!("Invalid JSON request body: {}", e)
                    })),
                )
                    .into_response();
            }
        }
    };
    let dir = match req
        .dir
        .map(PathBuf::from)
        .or_else(|| state.config.handoff.markers_dir.clone())
    {
        Some(d) => d,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "no directory given and handoff.markers_dir is not configured"
                })),
            )
                .into_response();
        }
    };
    let consume = state.config.handoff.consume_markers && !req.keep;
    let import = crate::handoff::import_markers(&state.database, &dir, consume).await;
    let scan_job = if req.link_now {
        match state.start_scan(false, false, None).await {
            Ok(job) => Some(job.scope_label),
            Err(e) => {
                tracing::warn!("Import pins requested link_now, but scan could not start: {e}");
                None
            }
        }
    } else {
        None
    };
    Json(ApiImportPinsResponse {
        directory: dir.display().to_string(),
        imported: import.imported,
        changed: import.changed,
        without_id: import.without_id,
        unreadable: import.unreadable,
        consumed: import.consumed,
        summary: import.summary_line(),
        scan_job,
    })
    .into_response()
}

use axum::{
    extract::{Path, Query, State},
    http::{header::CONTENT_TYPE, StatusCode},
    response::IntoResponse,
    Json,
};
use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::scheduler::{ScheduleRule, SchedulerRuleView};

use super::{ApiErrorResponse, WebState};

#[derive(Debug, Deserialize)]
pub struct SchedulerEnableRequest {
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct SchedulerPreviewQuery {
    pub id: Option<i64>,
    pub count: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct SchedulerPreviewResponse {
    pub id: Option<i64>,
    pub name: String,
    pub next: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SchedulerExportSnapshot {
    pub version: u32,
    pub rules: Vec<ScheduleRule>,
}

pub async fn api_get_scheduler_rules(State(state): State<WebState>) -> impl IntoResponse {
    match state.database.list_scheduler_rules().await {
        Ok(rules) => {
            let now = Local::now();
            let views: Vec<_> = rules
                .into_iter()
                .map(|rule| SchedulerRuleView {
                    next_due: rule
                        .next_after(now)
                        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S %Z").to_string()),
                    rule,
                })
                .collect();
            Json(views).into_response()
        }
        Err(err) => api_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub async fn api_post_scheduler_rule(
    State(state): State<WebState>,
    Json(rule): Json<ScheduleRule>,
) -> impl IntoResponse {
    match state.database.create_scheduler_rule(&rule).await {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(err) => api_error(StatusCode::BAD_REQUEST, err.to_string()),
    }
}

pub async fn api_put_scheduler_rule(
    State(state): State<WebState>,
    Path(id): Path<i64>,
    Json(rule): Json<ScheduleRule>,
) -> impl IntoResponse {
    match state.database.update_scheduler_rule(id, &rule).await {
        Ok(()) => Json(serde_json::json!({ "id": id, "updated": true })).into_response(),
        Err(err) => api_error(StatusCode::BAD_REQUEST, err.to_string()),
    }
}

pub async fn api_post_scheduler_rule_enable(
    State(state): State<WebState>,
    Path(id): Path<i64>,
    Json(request): Json<SchedulerEnableRequest>,
) -> impl IntoResponse {
    match state
        .database
        .set_scheduler_rule_enabled(id, request.enabled)
        .await
    {
        Ok(()) => Json(serde_json::json!({ "id": id, "enabled": request.enabled })).into_response(),
        Err(err) => api_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub async fn api_post_scheduler_rule_run_now(
    State(state): State<WebState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let rule = match state.database.get_scheduler_rule(id).await {
        Ok(Some(rule)) => rule,
        Ok(None) => return api_error(StatusCode::NOT_FOUND, "Scheduler rule not found"),
        Err(err) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };
    match crate::scheduler::run_rule_now(&state.config, &state.database, &rule).await {
        Ok(run_id) => Json(serde_json::json!({ "run_id": run_id })).into_response(),
        Err(err) => api_error(StatusCode::BAD_REQUEST, err.to_string()),
    }
}

pub async fn api_get_scheduler_runs(State(state): State<WebState>) -> impl IntoResponse {
    match state.database.list_scheduler_runs(100).await {
        Ok(runs) => Json(runs).into_response(),
        Err(err) => api_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub async fn api_get_scheduler_preview(
    State(state): State<WebState>,
    Query(query): Query<SchedulerPreviewQuery>,
) -> impl IntoResponse {
    let rules = match state.database.list_scheduler_rules().await {
        Ok(rules) => rules,
        Err(err) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };
    let count = query.count.unwrap_or(5).clamp(1, 20);
    let mut responses = Vec::new();
    for rule in rules
        .into_iter()
        .filter(|rule| query.id.is_none_or(|id| rule.id == Some(id)))
    {
        let mut cursor = Local::now();
        let mut next = Vec::new();
        for _ in 0..count {
            let Some(dt) = rule.next_after(cursor) else {
                break;
            };
            next.push(dt.format("%Y-%m-%d %H:%M:%S %Z").to_string());
            cursor = dt;
        }
        responses.push(SchedulerPreviewResponse {
            id: rule.id,
            name: rule.name,
            next,
        });
    }
    Json(responses).into_response()
}

pub async fn api_get_scheduler_export(State(state): State<WebState>) -> impl IntoResponse {
    match state.database.list_scheduler_rules().await {
        Ok(rules) => {
            let snapshot = SchedulerExportSnapshot { version: 1, rules };
            match serde_yml::to_string(&snapshot) {
                Ok(body) => ([(CONTENT_TYPE, "application/yaml")], body).into_response(),
                Err(err) => api_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
            }
        }
        Err(err) => api_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub async fn api_post_scheduler_import(
    State(state): State<WebState>,
    body: String,
) -> impl IntoResponse {
    let snapshot: SchedulerExportSnapshot = match serde_yml::from_str(&body) {
        Ok(snapshot) => snapshot,
        Err(err) => return api_error(StatusCode::BAD_REQUEST, err.to_string()),
    };
    let mut ids = Vec::new();
    for mut rule in snapshot.rules {
        rule.id = None;
        match state.database.create_scheduler_rule(&rule).await {
            Ok(id) => ids.push(id),
            Err(err) => return api_error(StatusCode::BAD_REQUEST, err.to_string()),
        }
    }
    Json(serde_json::json!({ "created": ids.len(), "ids": ids })).into_response()
}

fn api_error(status: StatusCode, error: impl Into<String>) -> axum::response::Response {
    (
        status,
        Json(ApiErrorResponse {
            error: error.into(),
        }),
    )
        .into_response()
}

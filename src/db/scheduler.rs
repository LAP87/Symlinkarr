use anyhow::Result;
use chrono::{DateTime, Local};
use sqlx::Row;

use super::Database;
use crate::scheduler::{
    JobRunStatus, ScheduleRule, ScheduleTrigger, ScheduleWindow, ScheduledEvent, SchedulerRunRecord,
};

impl Database {
    pub async fn scheduler_rule_count(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) AS count FROM scheduler_rules")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get("count"))
    }

    pub async fn list_scheduler_rules(&self) -> Result<Vec<ScheduleRule>> {
        let rows = sqlx::query(
            "SELECT id, name, event_type, enabled, trigger_json, run_window_json,
                    event_args_json, priority, misfire_grace_minutes, allow_destructive_auto,
                    max_delete, safety_backup, created_at, updated_at
             FROM scheduler_rules
             ORDER BY enabled DESC, priority DESC, name ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(rule_from_row).collect()
    }

    pub async fn get_scheduler_rule(&self, id: i64) -> Result<Option<ScheduleRule>> {
        let row = sqlx::query(
            "SELECT id, name, event_type, enabled, trigger_json, run_window_json,
                    event_args_json, priority, misfire_grace_minutes, allow_destructive_auto,
                    max_delete, safety_backup, created_at, updated_at
             FROM scheduler_rules
             WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(rule_from_row).transpose()
    }

    pub async fn create_scheduler_rule(&self, rule: &ScheduleRule) -> Result<i64> {
        rule.validate()?;
        let trigger_json = serde_json::to_string(&rule.trigger)?;
        let run_window_json = serde_json::to_string(&rule.run_window)?;
        let event_args_json = serde_json::to_string(&rule.event_args)?;
        let result = sqlx::query(
            "INSERT INTO scheduler_rules (
                name, event_type, enabled, trigger_json, run_window_json, event_args_json,
                priority, misfire_grace_minutes, allow_destructive_auto, max_delete, safety_backup
             )
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&rule.name)
        .bind(rule.event_type.as_str())
        .bind(rule.enabled)
        .bind(trigger_json)
        .bind(run_window_json)
        .bind(event_args_json)
        .bind(rule.priority)
        .bind(rule.misfire_grace_minutes)
        .bind(rule.allow_destructive_auto)
        .bind(rule.max_delete)
        .bind(rule.safety_backup)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn update_scheduler_rule(&self, id: i64, rule: &ScheduleRule) -> Result<()> {
        rule.validate()?;
        let trigger_json = serde_json::to_string(&rule.trigger)?;
        let run_window_json = serde_json::to_string(&rule.run_window)?;
        let event_args_json = serde_json::to_string(&rule.event_args)?;
        sqlx::query(
            "UPDATE scheduler_rules
             SET name = ?, event_type = ?, enabled = ?, trigger_json = ?, run_window_json = ?,
                 event_args_json = ?, priority = ?, misfire_grace_minutes = ?,
                 allow_destructive_auto = ?, max_delete = ?, safety_backup = ?,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?",
        )
        .bind(&rule.name)
        .bind(rule.event_type.as_str())
        .bind(rule.enabled)
        .bind(trigger_json)
        .bind(run_window_json)
        .bind(event_args_json)
        .bind(rule.priority)
        .bind(rule.misfire_grace_minutes)
        .bind(rule.allow_destructive_auto)
        .bind(rule.max_delete)
        .bind(rule.safety_backup)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_scheduler_rule_enabled(&self, id: i64, enabled: bool) -> Result<()> {
        sqlx::query(
            "UPDATE scheduler_rules
             SET enabled = ?, updated_at = CURRENT_TIMESTAMP
             WHERE id = ?",
        )
        .bind(enabled)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn create_scheduler_run(
        &self,
        rule_id: Option<i64>,
        event_type: ScheduledEvent,
        planned_at: DateTime<Local>,
        status: JobRunStatus,
        message: Option<&str>,
    ) -> Result<i64> {
        let now = Local::now().to_rfc3339();
        let planned_at = planned_at.to_rfc3339();
        let started_at = matches!(status, JobRunStatus::Running).then_some(now.as_str());
        let finished_at = matches!(
            status,
            JobRunStatus::Succeeded | JobRunStatus::Failed | JobRunStatus::Skipped
        )
        .then_some(now.as_str());
        let result = sqlx::query(
            "INSERT INTO scheduler_runs (
                rule_id, event_type, planned_at, started_at, finished_at, status, message
             )
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(rule_id)
        .bind(event_type.as_str())
        .bind(planned_at)
        .bind(started_at)
        .bind(finished_at)
        .bind(status.as_str())
        .bind(message)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn finish_scheduler_run(
        &self,
        id: i64,
        status: JobRunStatus,
        message: Option<&str>,
        output_refs_json: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE scheduler_runs
             SET finished_at = ?, status = ?, message = ?, output_refs_json = ?
             WHERE id = ?",
        )
        .bind(Local::now().to_rfc3339())
        .bind(status.as_str())
        .bind(message)
        .bind(output_refs_json)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_scheduler_runs(&self, limit: i64) -> Result<Vec<SchedulerRunRecord>> {
        let rows = sqlx::query(
            "SELECT id, rule_id, event_type, planned_at, started_at, finished_at, status, message,
                    output_refs_json
             FROM scheduler_runs
             ORDER BY planned_at DESC, id DESC
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(run_from_row).collect()
    }

    pub async fn latest_scheduler_run_planned_at(&self, rule_id: i64) -> Result<Option<String>> {
        let row = sqlx::query(
            "SELECT planned_at
             FROM scheduler_runs
             WHERE rule_id = ?
             ORDER BY planned_at DESC, id DESC
             LIMIT 1",
        )
        .bind(rule_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| row.get("planned_at")))
    }

    pub async fn scheduler_run_exists(
        &self,
        rule_id: i64,
        planned_at: DateTime<Local>,
    ) -> Result<bool> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS count
             FROM scheduler_runs
             WHERE rule_id = ? AND planned_at = ?",
        )
        .bind(rule_id)
        .bind(planned_at.to_rfc3339())
        .fetch_one(&self.pool)
        .await?;
        let count: i64 = row.get("count");
        Ok(count > 0)
    }

    pub async fn set_scheduler_state(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO scheduler_state (key, value, updated_at)
             VALUES (?, ?, CURRENT_TIMESTAMP)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = CURRENT_TIMESTAMP",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn rule_from_row(row: sqlx::sqlite::SqliteRow) -> Result<ScheduleRule> {
    let event_type = ScheduledEvent::from_strict(row.get::<String, _>("event_type").as_str())?;
    let trigger: ScheduleTrigger =
        serde_json::from_str(row.get::<String, _>("trigger_json").as_str())?;
    let run_window: ScheduleWindow =
        serde_json::from_str(row.get::<String, _>("run_window_json").as_str())?;
    let event_args = serde_json::from_str(row.get::<String, _>("event_args_json").as_str())?;
    Ok(ScheduleRule {
        id: Some(row.get("id")),
        name: row.get("name"),
        event_type,
        enabled: row.get("enabled"),
        trigger,
        run_window,
        event_args,
        priority: row.get("priority"),
        misfire_grace_minutes: row.get("misfire_grace_minutes"),
        allow_destructive_auto: row.get("allow_destructive_auto"),
        max_delete: row.get("max_delete"),
        safety_backup: row.get("safety_backup"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

fn run_from_row(row: sqlx::sqlite::SqliteRow) -> Result<SchedulerRunRecord> {
    Ok(SchedulerRunRecord {
        id: row.get("id"),
        rule_id: row.get("rule_id"),
        event_type: ScheduledEvent::from_strict(row.get::<String, _>("event_type").as_str())?,
        planned_at: row.get("planned_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
        status: row.get("status"),
        message: row.get("message"),
        output_refs_json: row.get("output_refs_json"),
    })
}

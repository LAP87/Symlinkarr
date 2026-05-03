use std::str::FromStr;

use anyhow::{Context, Result};
use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, Local, LocalResult, NaiveDate, NaiveDateTime,
    NaiveTime, TimeZone, Timelike, Utc, Weekday,
};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::config::Config;
use crate::db::{Database, ScanRunOrigin};
use crate::OutputFormat;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledEvent {
    Scan,
    Backup,
    HousekeepingVacuum,
    CacheRefresh,
    CleanupAudit,
    RepairAuto,
    CleanupPruneApply,
    AnimeRemediationApply,
}

impl ScheduledEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::Backup => "backup",
            Self::HousekeepingVacuum => "housekeeping_vacuum",
            Self::CacheRefresh => "cache_refresh",
            Self::CleanupAudit => "cleanup_audit",
            Self::RepairAuto => "repair_auto",
            Self::CleanupPruneApply => "cleanup_prune_apply",
            Self::AnimeRemediationApply => "anime_remediation_apply",
        }
    }

    pub fn from_strict(value: &str) -> Result<Self> {
        match value {
            "scan" => Ok(Self::Scan),
            "backup" => Ok(Self::Backup),
            "housekeeping_vacuum" => Ok(Self::HousekeepingVacuum),
            "cache_refresh" => Ok(Self::CacheRefresh),
            "cleanup_audit" => Ok(Self::CleanupAudit),
            "repair_auto" => Ok(Self::RepairAuto),
            "cleanup_prune_apply" => Ok(Self::CleanupPruneApply),
            "anime_remediation_apply" => Ok(Self::AnimeRemediationApply),
            _ => anyhow::bail!("Unsupported scheduled event type: {}", value),
        }
    }

    pub fn is_destructive(self) -> bool {
        matches!(self, Self::CleanupPruneApply | Self::AnimeRemediationApply)
    }

    pub fn default_grace_minutes(self) -> i64 {
        match self {
            Self::Backup | Self::HousekeepingVacuum | Self::CleanupPruneApply => 12 * 60,
            _ => 60,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum MisfirePolicy {
    GraceThenSkip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobPriority {
    Low,
    Normal,
    High,
    Critical,
}

impl JobPriority {
    pub fn score(self) -> i64 {
        match self {
            Self::Low => 25,
            Self::Normal => 50,
            Self::High => 75,
            Self::Critical => 100,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobRunStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Skipped,
}

impl JobRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScheduleWindow {
    #[default]
    Anytime,
    OnlyBetween {
        start: String,
        end: String,
    },
    OutsideActiveHours {
        quiet_start: String,
        quiet_end: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScheduleTrigger {
    Once {
        at: String,
    },
    Interval {
        every: u64,
        unit: IntervalUnit,
        #[serde(default)]
        start: Option<String>,
    },
    Daily {
        times: Vec<String>,
    },
    Weekly {
        weekdays: Vec<String>,
        times: Vec<String>,
    },
    Monthly {
        days: Vec<u32>,
        times: Vec<String>,
    },
    RRule {
        start: String,
        frequency: RRuleFrequency,
        interval: Option<u64>,
        count: Option<u64>,
        until: Option<String>,
        weekdays: Option<Vec<String>>,
        month_days: Option<Vec<u32>>,
        times: Option<Vec<String>>,
    },
    Cron {
        expression: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntervalUnit {
    Minutes,
    Hours,
    Days,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RRuleFrequency {
    Daily,
    Weekly,
    Monthly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRule {
    pub id: Option<i64>,
    pub name: String,
    pub event_type: ScheduledEvent,
    pub enabled: bool,
    pub trigger: ScheduleTrigger,
    #[serde(default)]
    pub run_window: ScheduleWindow,
    #[serde(default)]
    pub event_args: Value,
    pub priority: i64,
    pub misfire_grace_minutes: i64,
    #[serde(default)]
    pub allow_destructive_auto: bool,
    pub max_delete: Option<i64>,
    #[serde(default)]
    pub safety_backup: bool,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl ScheduleRule {
    pub fn new_bootstrap(
        name: impl Into<String>,
        event_type: ScheduledEvent,
        trigger: ScheduleTrigger,
        event_args: Value,
        priority: JobPriority,
    ) -> Self {
        Self {
            id: None,
            name: name.into(),
            event_type,
            enabled: true,
            trigger,
            run_window: ScheduleWindow::Anytime,
            event_args,
            priority: priority.score(),
            misfire_grace_minutes: event_type.default_grace_minutes(),
            allow_destructive_auto: false,
            max_delete: None,
            safety_backup: false,
            created_at: None,
            updated_at: None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        validate_trigger(&self.trigger)?;
        validate_window(&self.run_window)?;
        if self.event_type.is_destructive() {
            if !self.allow_destructive_auto {
                anyhow::bail!("Destructive scheduled jobs require allow_destructive_auto=true");
            }
            if self.max_delete.unwrap_or(0) <= 0 {
                anyhow::bail!("Destructive scheduled jobs require a positive max_delete cap");
            }
        }
        Ok(())
    }

    pub fn next_after(&self, after: DateTime<Local>) -> Option<DateTime<Local>> {
        let next = next_for_trigger(&self.trigger, after)?;
        apply_window(&self.run_window, next)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerRunRecord {
    pub id: i64,
    pub rule_id: Option<i64>,
    pub event_type: ScheduledEvent,
    pub planned_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub status: String,
    pub message: Option<String>,
    pub output_refs_json: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerRuleView {
    pub rule: ScheduleRule,
    pub next_due: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct SchedulerSummary {
    pub enabled_rules: usize,
    pub next_due: Option<String>,
    pub recent_runs: Vec<SchedulerRunRecord>,
}

pub fn bootstrap_rules_from_config(cfg: &Config) -> Vec<ScheduleRule> {
    let mut rules = Vec::new();
    if cfg.daemon.interval_minutes > 0 {
        rules.push(ScheduleRule::new_bootstrap(
            "Legacy daemon scan",
            ScheduledEvent::Scan,
            ScheduleTrigger::Interval {
                every: cfg.daemon.interval_minutes,
                unit: IntervalUnit::Minutes,
                start: None,
            },
            json!({
                "dry_run": false,
                "search_missing": cfg.daemon.search_missing,
                "library": null
            }),
            JobPriority::Normal,
        ));
    }
    if cfg.backup.enabled && cfg.backup.interval_hours > 0 {
        rules.push(ScheduleRule::new_bootstrap(
            "Legacy scheduled backup",
            ScheduledEvent::Backup,
            ScheduleTrigger::Interval {
                every: cfg.backup.interval_hours,
                unit: IntervalUnit::Hours,
                start: None,
            },
            json!({ "label": "scheduled" }),
            JobPriority::High,
        ));
    }
    if cfg.daemon.vacuum_enabled {
        rules.push(ScheduleRule::new_bootstrap(
            "Legacy VACUUM housekeeping",
            ScheduledEvent::HousekeepingVacuum,
            ScheduleTrigger::Daily {
                times: vec![format!("{:02}:00", cfg.daemon.vacuum_hour_local)],
            },
            json!({ "vacuum": true }),
            JobPriority::Low,
        ));
    }
    rules
}

pub async fn ensure_bootstrap_rules(cfg: &Config, db: &Database) -> Result<()> {
    if db.scheduler_rule_count().await? > 0 {
        return Ok(());
    }
    for rule in bootstrap_rules_from_config(cfg) {
        db.create_scheduler_rule(&rule).await?;
    }
    db.set_scheduler_state("bootstrap_status", "created_from_legacy_config")
        .await?;
    Ok(())
}

#[allow(dead_code)]
pub async fn scheduler_summary(db: &Database) -> Result<SchedulerSummary> {
    let rules = db.list_scheduler_rules().await?;
    let now = Local::now();
    let mut nexts: Vec<_> = rules
        .iter()
        .filter(|rule| rule.enabled)
        .filter_map(|rule| rule.next_after(now))
        .collect();
    nexts.sort();
    Ok(SchedulerSummary {
        enabled_rules: rules.iter().filter(|rule| rule.enabled).count(),
        next_due: nexts.first().map(format_local),
        recent_runs: db.list_scheduler_runs(5).await?,
    })
}

pub async fn run_scheduler_tick(cfg: &Config, db: &Database) -> Result<()> {
    ensure_bootstrap_rules(cfg, db).await?;
    let now = Local::now();
    db.set_scheduler_state("last_tick", &now.to_rfc3339())
        .await?;

    let mut due = due_rules(db, now).await?;
    due.sort_by(|a, b| {
        b.0.priority
            .cmp(&a.0.priority)
            .then_with(|| a.0.name.cmp(&b.0.name))
    });

    for (rule, planned_at) in due {
        let age = now.signed_duration_since(planned_at).num_minutes();
        if age > rule.misfire_grace_minutes {
            db.create_scheduler_run(
                rule.id,
                rule.event_type,
                planned_at,
                JobRunStatus::Skipped,
                Some("Missed schedule exceeded grace window"),
            )
            .await?;
            continue;
        }
        run_rule(cfg, db, &rule, planned_at).await?;
    }
    Ok(())
}

pub async fn run_rule_now(cfg: &Config, db: &Database, rule: &ScheduleRule) -> Result<i64> {
    run_rule(cfg, db, rule, Local::now()).await
}

async fn run_rule(
    cfg: &Config,
    db: &Database,
    rule: &ScheduleRule,
    planned_at: DateTime<Local>,
) -> Result<i64> {
    rule.validate()?;
    let run_id = db
        .create_scheduler_run(
            rule.id,
            rule.event_type,
            planned_at,
            JobRunStatus::Running,
            Some("Started"),
        )
        .await?;
    let result = execute_event(cfg, db, rule).await;
    match result {
        Ok(message) => {
            db.finish_scheduler_run(run_id, JobRunStatus::Succeeded, Some(&message), None)
                .await?;
            Ok(run_id)
        }
        Err(err) => {
            let message = err.to_string();
            db.finish_scheduler_run(run_id, JobRunStatus::Failed, Some(&message), None)
                .await?;
            Err(err)
        }
    }
}

async fn execute_event(cfg: &Config, db: &Database, rule: &ScheduleRule) -> Result<String> {
    match rule.event_type {
        ScheduledEvent::Scan => {
            if cfg.backup.enabled {
                let bm = crate::backup::BackupManager::new(&cfg.backup);
                if let Err(err) = bm.create_safety_snapshot(db, "scheduled-scan").await {
                    warn!("Scheduled pre-scan safety snapshot failed: {}", err);
                }
            }
            let dry_run = rule
                .event_args
                .get("dry_run")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let search_missing = rule
                .event_args
                .get("search_missing")
                .and_then(Value::as_bool)
                .unwrap_or(cfg.daemon.search_missing);
            let library = rule.event_args.get("library").and_then(Value::as_str);
            let (added, removed) = crate::commands::scan::run_scan_with_origin(
                cfg,
                db,
                ScanRunOrigin::Daemon,
                dry_run,
                search_missing,
                OutputFormat::Text,
                library,
            )
            .await?;
            Ok(format!(
                "Scan completed: {} added/updated, {} removed",
                added, removed
            ))
        }
        ScheduledEvent::Backup => {
            if !cfg.backup.enabled {
                anyhow::bail!("Backup is disabled in config.yaml");
            }
            let label = rule
                .event_args
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or("scheduled");
            let bm = crate::backup::BackupManager::new(&cfg.backup);
            let path = bm.create_backup(cfg, db, label).await?;
            Ok(format!("Backup created: {}", path.display()))
        }
        ScheduledEvent::HousekeepingVacuum => {
            let vacuum = rule
                .event_args
                .get("vacuum")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let stats = db.housekeeping_with_vacuum(vacuum).await?;
            Ok(format!(
                "Housekeeping completed: removed {} scan runs, {} link events, {} jobs, {} cache entries{}",
                stats.scan_runs_deleted,
                stats.link_events_deleted,
                stats.old_jobs_deleted,
                stats.expired_api_cache_deleted,
                if vacuum { ", VACUUM ran" } else { "" }
            ))
        }
        ScheduledEvent::CacheRefresh => {
            crate::commands::cache::run_cache(cfg, db, crate::CacheAction::Build).await?;
            Ok("Cache refresh completed".to_string())
        }
        ScheduledEvent::CleanupAudit => {
            let scope = match rule
                .event_args
                .get("scope")
                .and_then(Value::as_str)
                .unwrap_or("all")
            {
                "all" => crate::cleanup_audit::CleanupScope::All,
                "tv" => crate::cleanup_audit::CleanupScope::Tv,
                "movie" | "movies" => crate::cleanup_audit::CleanupScope::Movie,
                "anime" => crate::cleanup_audit::CleanupScope::Anime,
                other => anyhow::bail!("Unsupported cleanup audit scope: {}", other),
            };
            let auditor = crate::cleanup_audit::CleanupAuditor::new_with_progress(cfg, db, false);
            let output_path = crate::web::cleanup_audit_output_path(
                cfg,
                scope,
                &[],
                Utc::now().format("%Y%m%d-%H%M%S").to_string(),
            );
            let report_path = auditor
                .run_audit_filtered(scope, None, Some(&output_path))
                .await?;
            Ok(format!(
                "Cleanup audit completed: {}",
                report_path.display()
            ))
        }
        ScheduledEvent::RepairAuto => {
            let results =
                crate::commands::repair::execute_repair_auto(cfg, db, None, false, false).await?;
            let (repaired, failed, skipped, stale) =
                crate::commands::repair::summarize_repair_results(&results);
            Ok(format!(
                "Repair completed: {} repaired, {} failed, {} skipped, {} stale",
                repaired, failed, skipped, stale
            ))
        }
        ScheduledEvent::CleanupPruneApply | ScheduledEvent::AnimeRemediationApply => {
            anyhow::bail!(
                "{} scheduled execution requires a saved report selector and is not wired in this build",
                rule.event_type.as_str()
            )
        }
    }
}

async fn due_rules(
    db: &Database,
    now: DateTime<Local>,
) -> Result<Vec<(ScheduleRule, DateTime<Local>)>> {
    let rules = db.list_scheduler_rules().await?;
    let mut due = Vec::new();
    for rule in rules.into_iter().filter(|rule| rule.enabled) {
        let Some(rule_id) = rule.id else { continue };
        let last_planned = db.latest_scheduler_run_planned_at(rule_id).await?;
        if last_planned.is_none()
            && matches!(&rule.trigger, ScheduleTrigger::Interval { start: None, .. })
        {
            due.push((rule, now));
            continue;
        }
        let cursor = last_planned
            .and_then(|value| parse_local_datetime(&value).ok())
            .unwrap_or_else(|| now - ChronoDuration::minutes(rule.misfire_grace_minutes.max(1)));
        if let Some(next) = rule.next_after(cursor) {
            if next <= now && !db.scheduler_run_exists(rule_id, next).await? {
                due.push((rule, next));
            }
        }
    }
    Ok(due)
}

fn validate_trigger(trigger: &ScheduleTrigger) -> Result<()> {
    match trigger {
        ScheduleTrigger::Once { at } => {
            parse_local_datetime(at)?;
        }
        ScheduleTrigger::Interval { every, .. } if *every == 0 => {
            anyhow::bail!("Interval trigger requires every > 0");
        }
        ScheduleTrigger::Daily { times } => validate_times(times)?,
        ScheduleTrigger::Weekly { weekdays, times } => {
            validate_weekdays(weekdays)?;
            validate_times(times)?;
        }
        ScheduleTrigger::Monthly { days, times } => {
            if days.iter().any(|day| !(1..=31).contains(day)) {
                anyhow::bail!("Monthly day must be between 1 and 31");
            }
            validate_times(times)?;
        }
        ScheduleTrigger::RRule {
            start,
            times,
            weekdays,
            month_days,
            ..
        } => {
            parse_local_datetime(start)?;
            if let Some(times) = times {
                validate_times(times)?;
            }
            if let Some(weekdays) = weekdays {
                validate_weekdays(weekdays)?;
            }
            if let Some(days) = month_days {
                if days.iter().any(|day| !(1..=31).contains(day)) {
                    anyhow::bail!("RRule month day must be between 1 and 31");
                }
            }
        }
        ScheduleTrigger::Cron { expression } => {
            Schedule::from_str(expression).context("Invalid cron expression")?;
        }
        _ => {}
    }
    Ok(())
}

fn validate_window(window: &ScheduleWindow) -> Result<()> {
    match window {
        ScheduleWindow::Anytime => Ok(()),
        ScheduleWindow::OnlyBetween { start, end }
        | ScheduleWindow::OutsideActiveHours {
            quiet_start: start,
            quiet_end: end,
        } => {
            parse_time(start)?;
            parse_time(end)?;
            Ok(())
        }
    }
}

fn validate_times(times: &[String]) -> Result<()> {
    if times.is_empty() {
        anyhow::bail!("At least one time is required");
    }
    for time in times {
        parse_time(time)?;
    }
    Ok(())
}

fn validate_weekdays(weekdays: &[String]) -> Result<()> {
    if weekdays.is_empty() {
        anyhow::bail!("At least one weekday is required");
    }
    for day in weekdays {
        parse_weekday(day)?;
    }
    Ok(())
}

fn next_for_trigger(trigger: &ScheduleTrigger, after: DateTime<Local>) -> Option<DateTime<Local>> {
    match trigger {
        ScheduleTrigger::Once { at } => parse_local_datetime(at).ok().filter(|dt| *dt > after),
        ScheduleTrigger::Interval { every, unit, start } => {
            let step = match unit {
                IntervalUnit::Minutes => ChronoDuration::minutes(*every as i64),
                IntervalUnit::Hours => ChronoDuration::hours(*every as i64),
                IntervalUnit::Days => ChronoDuration::days(*every as i64),
            };
            let mut next = start
                .as_deref()
                .and_then(|value| parse_local_datetime(value).ok())
                .unwrap_or(after + step);
            while next <= after {
                next += step;
            }
            Some(next)
        }
        ScheduleTrigger::Daily { times } => next_by_days(after, 370, |date| {
            times
                .iter()
                .filter_map(|time| local_on_date(date, parse_time(time).ok()?))
                .collect()
        }),
        ScheduleTrigger::Weekly { weekdays, times } => {
            let weekdays: Vec<_> = weekdays
                .iter()
                .filter_map(|day| parse_weekday(day).ok())
                .collect();
            next_by_days(after, 370, |date| {
                if !weekdays.contains(&date.weekday()) {
                    return Vec::new();
                }
                times
                    .iter()
                    .filter_map(|time| local_on_date(date, parse_time(time).ok()?))
                    .collect()
            })
        }
        ScheduleTrigger::Monthly { days, times } => next_by_days(after, 800, |date| {
            if !days.contains(&date.day()) {
                return Vec::new();
            }
            times
                .iter()
                .filter_map(|time| local_on_date(date, parse_time(time).ok()?))
                .collect()
        }),
        ScheduleTrigger::RRule {
            start,
            frequency,
            interval,
            count,
            until,
            weekdays,
            month_days,
            times,
        } => {
            let start = parse_local_datetime(start).ok()?;
            let until = until
                .as_deref()
                .and_then(|value| parse_local_datetime(value).ok());
            let times = times
                .clone()
                .unwrap_or_else(|| vec![format!("{:02}:{:02}", start.hour(), start.minute())]);
            let weekdays = weekdays.as_ref().map(|days| {
                days.iter()
                    .filter_map(|day| parse_weekday(day).ok())
                    .collect::<Vec<_>>()
            });
            let month_days = month_days.clone();
            let interval = interval.unwrap_or(1).max(1) as i64;
            let mut seen = 0u64;
            next_by_days(
                after.max(start - ChronoDuration::seconds(1)),
                3660,
                |date| {
                    let candidate_midnight =
                        local_on_date(date, NaiveTime::from_hms_opt(0, 0, 0).unwrap());
                    let Some(candidate_midnight) = candidate_midnight else {
                        return Vec::new();
                    };
                    if candidate_midnight < start {
                        return Vec::new();
                    }
                    if let Some(until) = until {
                        if candidate_midnight > until {
                            return Vec::new();
                        }
                    }
                    let elapsed = candidate_midnight
                        .signed_duration_since(start)
                        .num_days()
                        .max(0);
                    let matches_frequency = match frequency {
                        RRuleFrequency::Daily => elapsed % interval == 0,
                        RRuleFrequency::Weekly => (elapsed / 7) % interval == 0,
                        RRuleFrequency::Monthly => {
                            let months = (date.year() - start.year()) as i64 * 12
                                + date.month() as i64
                                - start.month() as i64;
                            months >= 0 && months % interval == 0
                        }
                    };
                    if !matches_frequency {
                        return Vec::new();
                    }
                    if let Some(days) = weekdays.as_ref() {
                        if !days.contains(&date.weekday()) {
                            return Vec::new();
                        }
                    }
                    if let Some(days) = month_days.as_ref() {
                        if !days.contains(&date.day()) {
                            return Vec::new();
                        }
                    }
                    let candidates: Vec<_> = times
                        .iter()
                        .filter_map(|time| local_on_date(date, parse_time(time).ok()?))
                        .filter(|dt| *dt >= start)
                        .collect();
                    seen += candidates.len() as u64;
                    if count.is_some_and(|limit| seen > limit) {
                        Vec::new()
                    } else {
                        candidates
                    }
                },
            )
        }
        ScheduleTrigger::Cron { expression } => Schedule::from_str(expression)
            .ok()?
            .upcoming(Local)
            .find(|dt| *dt > after),
    }
}

fn next_by_days<F>(after: DateTime<Local>, max_days: i64, mut build: F) -> Option<DateTime<Local>>
where
    F: FnMut(NaiveDate) -> Vec<DateTime<Local>>,
{
    for offset in 0..=max_days {
        let date = (after + ChronoDuration::days(offset)).date_naive();
        let mut candidates = build(date);
        candidates.sort();
        if let Some(next) = candidates.into_iter().find(|dt| *dt > after) {
            return Some(next);
        }
    }
    None
}

fn apply_window(window: &ScheduleWindow, candidate: DateTime<Local>) -> Option<DateTime<Local>> {
    match window {
        ScheduleWindow::Anytime => Some(candidate),
        ScheduleWindow::OnlyBetween { start, end } => {
            shift_into_window(candidate, parse_time(start).ok()?, parse_time(end).ok()?)
        }
        ScheduleWindow::OutsideActiveHours {
            quiet_start,
            quiet_end,
        } => shift_into_window(
            candidate,
            parse_time(quiet_start).ok()?,
            parse_time(quiet_end).ok()?,
        ),
    }
}

fn shift_into_window(
    candidate: DateTime<Local>,
    start: NaiveTime,
    end: NaiveTime,
) -> Option<DateTime<Local>> {
    let time = candidate.time();
    let in_window = if start <= end {
        time >= start && time <= end
    } else {
        time >= start || time <= end
    };
    if in_window {
        return Some(candidate);
    }
    let mut date = candidate.date_naive();
    if start <= end && time > end {
        date = date.succ_opt()?;
    }
    if start > end && time > end && time < start {
        // same date at quiet_start
    }
    local_on_date(date, start)
}

pub fn parse_local_datetime(value: &str) -> Result<DateTime<Local>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(value) {
        return Ok(dt.with_timezone(&Local));
    }
    let naive = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S"))
        .or_else(|_| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d").map(|date| {
                date.and_time(NaiveTime::from_hms_opt(0, 0, 0).expect("valid midnight"))
            })
        })
        .with_context(|| format!("Invalid local date/time: {}", value))?;
    resolve_local(naive).with_context(|| format!("Local date/time does not exist: {}", value))
}

fn parse_time(value: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(value, "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(value, "%H:%M"))
        .with_context(|| format!("Invalid time: {}", value))
}

fn parse_weekday(value: &str) -> Result<Weekday> {
    match value.to_ascii_lowercase().as_str() {
        "mon" | "monday" => Ok(Weekday::Mon),
        "tue" | "tues" | "tuesday" => Ok(Weekday::Tue),
        "wed" | "wednesday" => Ok(Weekday::Wed),
        "thu" | "thur" | "thurs" | "thursday" => Ok(Weekday::Thu),
        "fri" | "friday" => Ok(Weekday::Fri),
        "sat" | "saturday" => Ok(Weekday::Sat),
        "sun" | "sunday" => Ok(Weekday::Sun),
        _ => anyhow::bail!("Invalid weekday: {}", value),
    }
}

fn local_on_date(date: NaiveDate, time: NaiveTime) -> Option<DateTime<Local>> {
    resolve_local(date.and_time(time))
}

fn resolve_local(naive: NaiveDateTime) -> Option<DateTime<Local>> {
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Some(dt),
        LocalResult::Ambiguous(first, _) => Some(first),
        LocalResult::None => None,
    }
}

#[allow(dead_code)]
fn format_local(dt: &DateTime<Local>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S %Z").to_string()
}

pub async fn run_scheduler_loop(cfg: &Config, db: &Database) -> Result<()> {
    ensure_bootstrap_rules(cfg, db).await?;
    info!("Scheduler loop starting (tick: 30 seconds)");
    loop {
        if let Err(err) = run_scheduler_tick(cfg, db).await {
            warn!("Scheduler tick failed: {}", err);
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
            _ = tokio::signal::ctrl_c() => {
                info!("Shutdown signal received; stopping scheduler loop");
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(value: &str) -> DateTime<Local> {
        parse_local_datetime(value).unwrap()
    }

    fn test_rule(trigger: ScheduleTrigger) -> ScheduleRule {
        ScheduleRule::new_bootstrap(
            "test",
            ScheduledEvent::Scan,
            trigger,
            json!({}),
            JobPriority::Normal,
        )
    }

    #[test]
    fn interval_next_run_advances_from_start() {
        let rule = test_rule(ScheduleTrigger::Interval {
            every: 15,
            unit: IntervalUnit::Minutes,
            start: Some("2026-05-03 10:00:00".to_string()),
        });

        let next = rule.next_after(local("2026-05-03 10:44:00")).unwrap();
        assert_eq!(
            next.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-05-03 10:45:00"
        );
    }

    #[test]
    fn weekly_next_run_uses_configured_weekday_and_time() {
        let rule = test_rule(ScheduleTrigger::Weekly {
            weekdays: vec!["mon".to_string(), "wed".to_string()],
            times: vec!["02:30".to_string()],
        });

        let next = rule.next_after(local("2026-05-03 12:00:00")).unwrap();
        assert_eq!(next.weekday(), Weekday::Mon);
        assert_eq!(next.format("%H:%M").to_string(), "02:30");
    }

    #[test]
    fn monthly_skips_invalid_calendar_days() {
        let rule = test_rule(ScheduleTrigger::Monthly {
            days: vec![31],
            times: vec!["01:00".to_string()],
        });

        let next = rule.next_after(local("2026-04-30 12:00:00")).unwrap();
        assert_eq!(
            next.format("%Y-%m-%d %H:%M").to_string(),
            "2026-05-31 01:00"
        );
    }

    #[test]
    fn cron_expression_preview_uses_cron_crate() {
        let rule = test_rule(ScheduleTrigger::Cron {
            expression: "0 0 4 * * * *".to_string(),
        });

        let next = rule.next_after(local("2026-05-03 03:59:00")).unwrap();
        assert_eq!(next.format("%H:%M").to_string(), "04:00");
    }

    #[test]
    fn run_window_moves_to_quiet_slot() {
        let mut rule = test_rule(ScheduleTrigger::Once {
            at: "2026-05-03 12:00:00".to_string(),
        });
        rule.run_window = ScheduleWindow::OutsideActiveHours {
            quiet_start: "23:00".to_string(),
            quiet_end: "06:00".to_string(),
        };

        let next = rule.next_after(local("2026-05-03 11:00:00")).unwrap();
        assert_eq!(
            next.format("%Y-%m-%d %H:%M").to_string(),
            "2026-05-03 23:00"
        );
    }

    #[test]
    fn destructive_rules_require_explicit_safety_cap() {
        let mut rule = ScheduleRule::new_bootstrap(
            "prune",
            ScheduledEvent::CleanupPruneApply,
            ScheduleTrigger::Daily {
                times: vec!["03:00".to_string()],
            },
            json!({}),
            JobPriority::High,
        );

        assert!(rule.validate().is_err());
        rule.allow_destructive_auto = true;
        assert!(rule.validate().is_err());
        rule.max_delete = Some(10);
        assert!(rule.validate().is_ok());
    }
}

use anyhow::Result;

use crate::commands::print_json;
use crate::db::{AcquisitionJobStatus, Database};
use crate::{OutputFormat, QueueAction, QueueRetryScope};

pub(crate) async fn run_queue(db: &Database, action: QueueAction, output: OutputFormat) -> Result<()> {
    match action {
        QueueAction::List { status, limit } => {
            let statuses = status.map(|value| vec![value.into_job_status()]);
            let jobs = db
                .list_acquisition_jobs(statuses.as_deref(), limit.max(1))
                .await?;

            if output == OutputFormat::Json {
                let rows = jobs
                    .iter()
                    .map(|job| {
                        serde_json::json!({
                            "id": job.id,
                            "status": job.status.as_str(),
                            "label": job.label,
                            "query": job.query,
                            "arr": job.arr,
                            "attempts": job.attempts,
                            "error": job.error,
                            "next_retry_at": job.next_retry_at.map(|dt| dt.to_rfc3339()),
                            "release_title": job.release_title,
                        })
                    })
                    .collect::<Vec<_>>();
                print_json(&serde_json::json!({
                    "count": rows.len(),
                    "jobs": rows,
                }));
            } else if jobs.is_empty() {
                println!("No queue jobs found.");
            } else {
                println!("\n🧾 Auto-Acquire Queue ({})", jobs.len());
                for job in jobs {
                    println!(
                        "   #{} [{}] {}",
                        job.id,
                        format!("{:?}", job.status).to_lowercase(),
                        job.label
                    );
                    println!("      query: {}", job.query);
                    println!("      arr: {}, attempts: {}", job.arr, job.attempts);
                    if let Some(next_retry_at) = job.next_retry_at {
                        println!("      next retry: {}", next_retry_at.to_rfc3339());
                    }
                    if let Some(error) = &job.error {
                        println!("      error: {}", error);
                    }
                }
            }
        }
        QueueAction::Retry { scope } => {
            let statuses = match scope {
                QueueRetryScope::All => vec![
                    AcquisitionJobStatus::Blocked,
                    AcquisitionJobStatus::NoResult,
                    AcquisitionJobStatus::Failed,
                    AcquisitionJobStatus::CompletedUnlinked,
                ],
                QueueRetryScope::Blocked => vec![AcquisitionJobStatus::Blocked],
                QueueRetryScope::NoResult => vec![AcquisitionJobStatus::NoResult],
                QueueRetryScope::Failed => vec![AcquisitionJobStatus::Failed],
                QueueRetryScope::CompletedUnlinked => vec![AcquisitionJobStatus::CompletedUnlinked],
            };
            let reset = db.retry_acquisition_jobs(&statuses).await?;

            if output == OutputFormat::Json {
                print_json(&serde_json::json!({
                    "reset": reset,
                    "scope": format!("{:?}", scope).to_lowercase(),
                }));
            } else {
                println!(
                    "Reset {} queue job(s) to queued for scope '{}'.",
                    reset,
                    format!("{:?}", scope).to_lowercase()
                );
            }
        }
    }

    Ok(())
}

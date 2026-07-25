use anyhow::Result;
use serde_json::json;

use crate::config::Config;
use crate::media_servers::{deferred_refresh_summary, drain_deferred_refreshes};
use crate::OutputFormat;

pub(crate) async fn run_refresh_drain(cfg: &Config, output: OutputFormat) -> Result<()> {
    let outcome = drain_deferred_refreshes(cfg, output != OutputFormat::Json).await?;
    let summary = deferred_refresh_summary(cfg)?;

    match output {
        OutputFormat::Json => crate::commands::print_json(&json!({
            "drained": outcome,
            "remaining": summary,
        })),
        OutputFormat::Text => {
            println!(
                "Media-server refresh drain: {} request(s), {} remaining target(s)",
                outcome.aggregate.refreshed_batches, summary.pending_targets
            );
            if outcome.aggregate.deferred_due_to_lock {
                println!("Refresh was deferred because another Symlinkarr process holds the media refresh lock.");
            }
        }
    }

    Ok(())
}

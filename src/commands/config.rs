use anyhow::Result;

use crate::commands::print_json;
use crate::config::Config;
use crate::{ConfigAction, OutputFormat};

pub(crate) async fn run_config(cfg: &Config, action: ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Validate { output } => {
            let report = cfg.validate();
            if output == OutputFormat::Json {
                print_json(&serde_json::json!({
                    "ok": report.errors.is_empty(),
                    "errors": report.errors,
                    "warnings": report.warnings,
                }));
                if !report.errors.is_empty() {
                    anyhow::bail!("Configuration validation failed");
                }
                return Ok(());
            } else if report.errors.is_empty() {
                println!("✅ Configuration validation passed");
            } else {
                println!("❌ Configuration validation failed:");
                for err in &report.errors {
                    println!("   - {}", err);
                }
            }

            if output == OutputFormat::Text && !report.warnings.is_empty() {
                println!("⚠️  Warnings:");
                for w in &report.warnings {
                    println!("   - {}", w);
                }
            }

            if !report.errors.is_empty() {
                anyhow::bail!("Configuration validation failed");
            }
        }
    }
    Ok(())
}

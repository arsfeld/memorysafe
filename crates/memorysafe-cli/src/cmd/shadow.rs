use crate::cmd::portable;
use crate::config::MsafeConfig;
use crate::render;
use anyhow::{Context, Result, bail};
use clap::Args;
use memorysafe_policy::{BaselineConfig, BaselinePolicy};
use memorysafe_shadow::{TraceDiff, diff, replay, run};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Args)]
pub struct ShadowArgs {
    /// An export archive directory, or a bare `.ndjson` export stream.
    pub archive: PathBuf,
    /// The policy implementation to replay with. `baseline` is the only one in
    /// this build; a closed policy crate registers its own name.
    #[arg(long, default_value = "baseline", value_parser = ["baseline"])]
    pub policy: String,
    /// JSON `BaselineConfig` for the candidate policy. Without it, the archive's
    /// recorded decisions are compared against a replay under the configured
    /// policy.
    #[arg(long)]
    pub candidate_config: Option<PathBuf>,
    /// JSON `BaselineConfig` for the baseline side. Defaults to the policy in
    /// the configuration file.
    #[arg(long)]
    pub baseline_config: Option<PathBuf>,
    /// Write the full report here as JSON.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(Serialize)]
struct ShadowReport {
    scenario: String,
    replayed: usize,
    unreplayable: usize,
    coverage: f32,
    baseline_policy: String,
    candidate_policy: String,
    diff: TraceDiff,
}

fn load_config(path: &PathBuf) -> Result<BaselineConfig> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub async fn shadow(config: &MsafeConfig, json: bool, args: ShadowArgs) -> Result<()> {
    let ndjson = portable::read_stream(&args.archive)?;
    let name = args
        .archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".into());
    let replayed = replay::from_export_ndjson(&name, &ndjson)?;

    let baseline_config = match &args.baseline_config {
        Some(path) => load_config(path)?,
        None => config.policy.clone(),
    };
    let baseline = Arc::new(BaselinePolicy::new(baseline_config));

    let (before, after) = match &args.candidate_config {
        Some(path) => {
            let candidate = Arc::new(BaselinePolicy::new(load_config(path)?));
            (
                run(&replayed.scenario, baseline).await?,
                run(&replayed.scenario, candidate).await?,
            )
        }
        None => {
            let Some(recorded) = replayed.recorded else {
                bail!(
                    "this archive carries no recorded decisions, so there is nothing to compare a \
                     replay against; export it with --include-audit, or pass --candidate-config to \
                     diff two policy configurations instead"
                );
            };
            let replayed_trace = run(&replayed.scenario, baseline).await?;
            (recorded, replayed_trace)
        }
    };

    let report = ShadowReport {
        scenario: replayed.scenario.name.clone(),
        replayed: replayed.scenario.writes.len(),
        unreplayable: replayed.scenario.unreplayable.len(),
        coverage: replayed.scenario.coverage(),
        baseline_policy: before.policy.to_string(),
        candidate_policy: after.policy.to_string(),
        diff: diff(&before, &after)?,
    };

    if let Some(path) = &args.out {
        std::fs::write(path, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("writing {}", path.display()))?;
    }

    render::emit(json, &report, || {
        println!(
            "{}: {} decision(s) replayed, {} unreplayable, coverage {:.0}%",
            report.scenario,
            report.replayed,
            report.unreplayable,
            report.coverage * 100.0
        );
        println!(
            "{} identical, {} changed, out of {}",
            report.diff.identical,
            report.diff.changed.len(),
            report.diff.total
        );
        for (transition, count) in &report.diff.transitions {
            println!("  {transition}: {count}");
        }
        for change in report.diff.changed.iter().take(20) {
            println!(
                "  #{} {} -> {}",
                change.seq,
                change.before.action.label(),
                change.after.action.label()
            );
        }
        if report.diff.changed.len() > 20 {
            println!(
                "  ... {} more (use --out for the full report)",
                report.diff.changed.len() - 20
            );
        }
    })
}

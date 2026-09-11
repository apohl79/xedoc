//! Command-line entry point for offline model-router calibration.

use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "xedoc-model-router")]
#[command(about = "Evaluate a frozen privacy-preserving model-router calibration corpus")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Evaluate supplied deterministic outputs against a frozen held-out input.
    Calibrate {
        /// Path to the versioned reproducibility manifest JSON.
        #[arg(long)]
        manifest: PathBuf,
        /// Path to an explicit local labelled JSON or Markdown corpus.
        #[arg(long)]
        corpus: PathBuf,
        /// Checked local Arctic artifact directory.
        #[arg(long)]
        artifact: PathBuf,
        /// Path to write privacy-preserving frozen JSON input.
        #[arg(long)]
        frozen_input: PathBuf,
        /// Path to write aggregate report JSON.
        #[arg(long)]
        report: PathBuf,
        /// Path to write a non-active classifier proposal JSON.
        #[arg(long)]
        proposal: PathBuf,
    },
    /// Inspect local artifact health without printing prompt data.
    Inspect {
        /// Local artifact directory containing manifest.json.
        #[arg(long)]
        artifact: PathBuf,
    },
    /// Stage and assess a policy revision without changing the active mapping.
    TunePropose {
        /// Frozen reproducibility manifest JSON.
        #[arg(long)]
        manifest: PathBuf,
        /// Frozen embedding input JSON produced by `calibrate`.
        #[arg(long)]
        frozen_input: PathBuf,
        /// Aggregate held-out report produced by `calibrate`.
        #[arg(long)]
        frozen_report: PathBuf,
        /// Bounded aggregate A/B, cost, latency, fallback, and override evidence.
        #[arg(long)]
        evidence: PathBuf,
        /// Bounded newer class-drift samples without prompts or embeddings.
        #[arg(long)]
        drift_samples: PathBuf,
        /// Candidate standalone model-router TOML policy.
        #[arg(long)]
        candidate_policy: PathBuf,
        /// Active standalone model-router TOML policy.
        #[arg(long)]
        active_policy: PathBuf,
        /// Optional local directory for immutable staged revisions.
        #[arg(long)]
        revisions_dir: Option<PathBuf>,
        /// Path to write the reviewable proposal JSON.
        #[arg(long)]
        proposal: PathBuf,
    },
    /// Print a proposal with staged and active revision identities.
    TuneInspect {
        /// Reviewable proposal JSON produced by `tune-propose`.
        #[arg(long)]
        proposal: PathBuf,
        /// Active standalone model-router TOML policy.
        #[arg(long)]
        active_policy: PathBuf,
        /// Optional local directory for immutable staged revisions.
        #[arg(long)]
        revisions_dir: Option<PathBuf>,
    },
    /// Record explicit review and atomically activate an eligible proposal.
    TuneActivate {
        /// Reviewable proposal JSON produced by `tune-propose`.
        #[arg(long)]
        proposal: PathBuf,
        /// Frozen reproducibility manifest JSON.
        #[arg(long)]
        manifest: PathBuf,
        /// Frozen embedding input JSON produced by `calibrate`.
        #[arg(long)]
        frozen_input: PathBuf,
        /// Aggregate held-out report produced by `calibrate`.
        #[arg(long)]
        frozen_report: PathBuf,
        /// Bounded aggregate A/B, cost, latency, fallback, and override evidence.
        #[arg(long)]
        evidence: PathBuf,
        /// Bounded newer class-drift samples without prompts or embeddings.
        #[arg(long)]
        drift_samples: PathBuf,
        /// Active standalone model-router TOML policy.
        #[arg(long)]
        active_policy: PathBuf,
        /// Optional local directory for immutable staged revisions.
        #[arg(long)]
        revisions_dir: Option<PathBuf>,
        /// Bounded reviewer identity recorded beside the staged policy.
        #[arg(long)]
        reviewed_by: String,
        /// Bounded reviewer note recorded beside the staged policy.
        #[arg(long)]
        review_note: String,
    },
    /// Record explicit review and atomically restore a staged revision.
    TuneRollback {
        /// Active standalone model-router TOML policy.
        #[arg(long)]
        active_policy: PathBuf,
        /// Optional local directory for immutable staged revisions.
        #[arg(long)]
        revisions_dir: Option<PathBuf>,
        /// Previously staged policy revision to restore.
        #[arg(long)]
        revision: String,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Calibrate {
            manifest,
            corpus,
            artifact,
            frozen_input,
            report,
            proposal,
        } => {
            xedoc_model_router::calibrate(
                &manifest,
                &corpus,
                &artifact,
                &frozen_input,
                &report,
                &proposal,
            )?;
            Ok(())
        }
        Command::Inspect { artifact } => {
            let artifact = xedoc_model_router::LocalArtifact::load(&artifact)
                .map_err(|error| format!("artifact unhealthy: {error}"))?;
            let embedder = xedoc_model_router::FastEmbedder::from_local_artifact(&artifact)
                .map_err(|error| format!("embedding runtime unhealthy: {error}"))?;
            let embedding = xedoc_model_router::SyncEmbedder::embed_sync(
                &embedder,
                "model-router health check",
            )
            .map_err(|error| format!("embedding health check failed: {error}"))?;
            if embedding.len() != artifact.descriptor.dimensions {
                return Err("embedding health check returned unexpected dimensions".into());
            }
            println!(
                "{}",
                serde_json::json!({
                    "healthy": true,
                    "revision": artifact.descriptor.revision,
                    "sha256": artifact.descriptor.sha256,
                    "dimensions": artifact.descriptor.dimensions,
                    "decisions": 0,
                    "fallbacks": 0,
                })
            );
            Ok(())
        }
        Command::TunePropose {
            manifest,
            frozen_input,
            frozen_report,
            evidence,
            drift_samples,
            candidate_policy,
            active_policy,
            revisions_dir,
            proposal,
        } => xedoc_model_router::propose_tuning(
            &manifest,
            &frozen_input,
            &frozen_report,
            &evidence,
            &drift_samples,
            &candidate_policy,
            &active_policy,
            revisions_dir.as_deref(),
            &proposal,
        )
        .map_err(Into::into),
        Command::TuneInspect {
            proposal,
            active_policy,
            revisions_dir,
        } => {
            println!(
                "{}",
                xedoc_model_router::inspect_tuning_proposal(
                    &proposal,
                    &active_policy,
                    revisions_dir.as_deref(),
                )?
            );
            Ok(())
        }
        Command::TuneActivate {
            proposal,
            manifest,
            frozen_input,
            frozen_report,
            evidence,
            drift_samples,
            active_policy,
            revisions_dir,
            reviewed_by,
            review_note,
        } => {
            println!(
                "{}",
                xedoc_model_router::activate_tuning_proposal(
                    &proposal,
                    &manifest,
                    &frozen_input,
                    &frozen_report,
                    &evidence,
                    &drift_samples,
                    &active_policy,
                    revisions_dir.as_deref(),
                    reviewed_by,
                    review_note,
                )?
            );
            Ok(())
        }
        Command::TuneRollback {
            active_policy,
            revisions_dir,
            revision,
        } => {
            println!(
                "{}",
                xedoc_model_router::rollback_tuning_policy(
                    &active_policy,
                    revisions_dir.as_deref(),
                    &revision,
                )?
            );
            Ok(())
        }
    }
}

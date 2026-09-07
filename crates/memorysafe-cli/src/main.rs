use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use memorysafe_cli::config::MsafeConfig;
use memorysafe_cli::{build, cmd};
use memorysafe_core::Scope;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "msafe", version, about = "Governed memory for AI agents")]
struct Cli {
    /// Configuration file. Defaults to ./msafe.toml when present.
    #[arg(long, global = true, env = "MSAFE_CONFIG")]
    config: Option<PathBuf>,
    #[arg(long, global = true, env = "MSAFE_TENANT")]
    tenant: Option<String>,
    #[arg(long, global = true, env = "MSAFE_SUBJECT")]
    subject: Option<String>,
    #[arg(long, global = true, env = "MSAFE_NAMESPACE")]
    namespace: Option<String>,
    /// Print the engine's own JSON instead of a human summary.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Store a memory and print the governance decision.
    Remember(cmd::memory::RememberArgs),
    /// Compose a working set under a token budget.
    Recall(cmd::memory::RecallArgs),
    /// List what is stored in this scope.
    Review(cmd::memory::ReviewArgs),
}

fn main() -> Result<()> {
    // stderr, never stdout: `msafe serve --transport stdio` uses stdout as the
    // MCP transport, and one stray log line there corrupts the session.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("MSAFE_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();
    let config = MsafeConfig::load(cli.config.as_deref())?;

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?
        .block_on(run(cli, config))
}

async fn run(cli: Cli, config: MsafeConfig) -> Result<()> {
    let engine = build::build_engine(&config)?;
    let scope = resolve_scope(&cli, &config)?;

    match cli.command {
        Command::Remember(args) => cmd::memory::remember(&engine, scope, cli.json, args).await,
        Command::Recall(args) => cmd::memory::recall(&engine, scope, cli.json, args).await,
        Command::Review(args) => cmd::memory::review(&engine, scope, cli.json, args).await,
    }
}

/// Flag, then environment (clap folds those together), then config file. There
/// is no default: writing into a scope the caller did not name is how memories
/// end up in the wrong place.
fn resolve_scope(cli: &Cli, config: &MsafeConfig) -> Result<Scope> {
    let pick =
        |flag: &Option<String>, from_config: &Option<String>, name: &str| -> Result<String> {
            flag.clone()
                .or_else(|| from_config.clone())
                .with_context(|| {
                    format!(
                        "no {name}: pass --{name}, set MSAFE_{}, or put it in msafe.toml",
                        name.to_uppercase()
                    )
                })
        };

    let tenant = pick(&cli.tenant, &config.tenant, "tenant")?;
    let subject = pick(&cli.subject, &config.subject, "subject")?;
    let namespace = pick(&cli.namespace, &config.namespace, "namespace")?;

    // `memorysafe_auth::check_reserved`, not a hand-rolled `== ADMIN_COMPONENT`
    // comparison: standing constraint #2 (this plan's `standing-constraints.md`)
    // is explicit that the reserved set is `_admin` **and** `_purged`, and that
    // hand-rolling the check against `ADMIN_COMPONENT` alone has already
    // drifted twice. This CLI has no `Authenticated` to route through (there is
    // no API key — the tenant/subject/namespace come from configuration, the
    // same footing `memorysafe-mcp`'s stdio transport stands on), so it calls
    // the same reserved-word check that transport uses directly.
    memorysafe_auth::check_reserved(Some(&subject), Some(&namespace))?;
    Ok(Scope::new(&tenant, &subject, &namespace)?)
}

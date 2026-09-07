use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use memorysafe_cli::config::MsafeConfig;
use memorysafe_cli::{build, cmd};
use memorysafe_core::{Scope, SubjectId, TenantId};
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
    /// Delete memories by id, tag, or kind.
    Forget(cmd::curate::ForgetArgs),
    /// Pin or protect a memory.
    Protect(cmd::curate::ProtectArgs),
    /// Show the governance decisions recorded for this scope.
    Audit(cmd::curate::AuditArgs),
    /// Run maintenance: expiry, decay, consolidation, reclaim.
    Maintain(cmd::curate::MaintainArgs),
    /// Delete everything belonging to one subject.
    PurgeSubject(cmd::curate::PurgeArgs),
    /// Write a portable archive of this scope.
    Export(cmd::portable::ExportArgs),
    /// Read a portable archive back in.
    Import(cmd::portable::ImportArgs),
    /// Manage API keys for the HTTP and MCP-over-HTTP servers.
    Keys {
        #[command(subcommand)]
        command: cmd::keys::KeysCommand,
    },
    /// Run the MCP server (stdio) or the HTTP API plus MCP transport.
    Serve(cmd::serve::ServeArgs),
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
    // `keys` only ever reads the tenant (see `cmd::keys::run`'s signature) —
    // it manages configuration, not a governed scope. Resolving the full
    // triple here used to make `msafe keys list` demand a subject and
    // namespace nothing on that path reads at all.
    if let Command::Keys { command } = cli.command {
        let tenant = resolve_tenant(&cli.tenant, &config.tenant)?;
        return cmd::keys::run(command, &tenant, &config, cli.config.as_deref(), cli.json);
    }

    let engine = build::build_engine(&config)?;

    // `serve` resolves only what its own transport actually reads: `stdio`
    // fixes the tenant and subject for the session, with the namespace
    // coming from the working directory rather than a flag (see
    // `resolve_tenant_and_subject`'s doc); `http` reads no scope component
    // at all, since `ScopeSource::Http` derives tenant, subject and
    // namespace per request from the presented API key. Both used to go
    // through the full three-field `resolve_scope` before dispatch, which
    // made `serve --transport stdio` reject on a missing `--namespace`
    // before it ever reached the cwd-derived default it exists to use —
    // Task 14's headline feature was unreachable — and made `--transport
    // http` demand a subject and namespace nothing on that path reads.
    if let Command::Serve(args) = cli.command {
        return match args.transport.as_str() {
            "stdio" => {
                let (tenant, subject) = resolve_tenant_and_subject(
                    &cli.tenant,
                    &config.tenant,
                    &cli.subject,
                    &config.subject,
                )?;
                cmd::serve::serve_stdio_transport(engine, tenant, subject).await
            }
            "http" => cmd::serve::serve_http_transport(engine, &config, &args).await,
            other => anyhow::bail!("unknown transport '{other}'; expected stdio or http"),
        };
    }

    // Every remaining command genuinely reads subject and namespace too.
    let scope = resolve_scope(&cli, &config)?;

    match cli.command {
        Command::Remember(args) => cmd::memory::remember(&engine, scope, cli.json, args).await,
        Command::Recall(args) => cmd::memory::recall(&engine, scope, cli.json, args).await,
        Command::Review(args) => cmd::memory::review(&engine, scope, cli.json, args).await,
        Command::Forget(args) => cmd::curate::forget(&engine, scope, cli.json, args).await,
        Command::Protect(args) => cmd::curate::protect(&engine, scope, cli.json, args).await,
        Command::Audit(args) => cmd::curate::audit(&engine, scope, cli.json, args).await,
        Command::Maintain(args) => cmd::curate::maintain(&engine, scope, cli.json, args).await,
        Command::PurgeSubject(args) => {
            cmd::curate::purge_subject(&engine, &scope.tenant, cli.json, args).await
        }
        Command::Export(args) => cmd::portable::export(&engine, scope, cli.json, args).await,
        Command::Import(args) => cmd::portable::import(&engine, scope, cli.json, args).await,
        Command::Keys { .. } | Command::Serve(_) => unreachable!(),
    }
}

/// Flag, then environment (clap folds those together), then config file. There
/// is no default: writing into a scope the caller did not name is how memories
/// end up in the wrong place.
fn pick(flag: &Option<String>, from_config: &Option<String>, name: &str) -> Result<String> {
    flag.clone()
        .or_else(|| from_config.clone())
        .with_context(|| {
            format!(
                "no {name}: pass --{name}, set MSAFE_{}, or put it in msafe.toml",
                name.to_uppercase()
            )
        })
}

/// `msafe keys ...` needs only the tenant a key belongs to.
fn resolve_tenant(
    tenant_flag: &Option<String>,
    tenant_config: &Option<String>,
) -> Result<TenantId> {
    let tenant = pick(tenant_flag, tenant_config, "tenant")?;
    Ok(TenantId::new(&tenant)?)
}

/// `msafe serve --transport stdio` needs the tenant and subject, never the
/// namespace: that transport's default namespace comes from the working
/// directory (`cmd::serve::namespace_from_cwd`), and a per-call override
/// happens inside the running MCP session, not at startup. Reserved-word
/// checking still applies to the subject — the one component resolved here
/// a caller could otherwise smuggle a reserved word through — the same way
/// `resolve_scope` checks both; `check_reserved` accepts `None` for a
/// component that is not being resolved at all.
fn resolve_tenant_and_subject(
    tenant_flag: &Option<String>,
    tenant_config: &Option<String>,
    subject_flag: &Option<String>,
    subject_config: &Option<String>,
) -> Result<(TenantId, SubjectId)> {
    let tenant = pick(tenant_flag, tenant_config, "tenant")?;
    let subject = pick(subject_flag, subject_config, "subject")?;
    memorysafe_auth::check_reserved(Some(&subject), None)?;
    Ok((TenantId::new(&tenant)?, SubjectId::new(&subject)?))
}

fn resolve_scope(cli: &Cli, config: &MsafeConfig) -> Result<Scope> {
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

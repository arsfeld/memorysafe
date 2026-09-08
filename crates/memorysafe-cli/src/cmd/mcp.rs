//! `msafe mcp` — write an MCP client entry, and supply its headers.
//!
//! The point of both subcommands is that one committed `.mcp.json` entry
//! works against a local `msafe` and a hosted MemorySafe, and that neither
//! form ever puts a credential in a file that gets committed.

use crate::cmd::serve::namespace_from_cwd;
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use memorysafe_auth::NAMESPACE_HEADER;
use std::path::Path;

const CONFIG_FILE: &str = ".mcp.json";
const KEY_ENV: &str = "MEMORYSAFE_API_KEY";
const DEFAULT_URL: &str = "https://api.memorysafe.dev";

#[derive(Debug, Subcommand)]
pub enum McpCommand {
    /// Write an MCP client entry for this project.
    Install(InstallArgs),
    /// Emit the headers an MCP client should connect with. Intended as a
    /// `headersHelper`, re-run by the client on every connection.
    Headers,
}

#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Point at a hosted MemorySafe rather than a local `msafe` subprocess.
    #[arg(long)]
    pub remote: bool,
    /// Emit a spec-portable entry using environment expansion instead of a
    /// `headersHelper`. The namespace is fixed at install time rather than
    /// following the working directory.
    #[arg(long)]
    pub portable: bool,
    /// The hosted base URL. Only meaningful with `--remote`.
    #[arg(long)]
    pub url: Option<String>,
}

pub fn run(command: McpCommand) -> Result<()> {
    match command {
        McpCommand::Install(args) => install(args, Path::new(CONFIG_FILE)),
        McpCommand::Headers => headers(),
    }
}

fn entry(args: &InstallArgs) -> serde_json::Value {
    if !args.remote {
        return serde_json::json!({
            "command": "msafe",
            "args": ["serve", "--transport", "stdio"]
        });
    }

    let base = args.url.as_deref().unwrap_or(DEFAULT_URL);
    if args.portable {
        // The namespace is baked now, because there is no helper to compute
        // it per connection. Same rule, evaluated once.
        serde_json::json!({
            "type": "http",
            "url": format!("{base}/mcp"),
            "headers": {
                "Authorization": format!("Bearer ${{{KEY_ENV}}}"),
                NAMESPACE_HEADER: namespace_from_cwd().as_str(),
            }
        })
    } else {
        // `${VAR:-default}` so a local hosted instance is one env var away,
        // and `headersHelper` so the namespace follows the directory the
        // client is actually working in.
        serde_json::json!({
            "type": "http",
            "url": format!("${{MEMORYSAFE_URL:-{base}}}/mcp"),
            "headersHelper": "msafe mcp headers"
        })
    }
}

fn install(args: InstallArgs, path: &Path) -> Result<()> {
    // Merge rather than overwrite: a project's `.mcp.json` usually already
    // names other servers, and clobbering them would be a hostile install.
    let mut doc: serde_json::Value = if path.exists() {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
    } else {
        serde_json::json!({})
    };

    doc.as_object_mut()
        .context("the MCP configuration file must hold a JSON object")?
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .context("`mcpServers` must be a JSON object")?
        .insert("memorysafe".to_owned(), entry(&args));

    let mut rendered = serde_json::to_string_pretty(&doc)?;
    rendered.push('\n');
    std::fs::write(path, rendered).with_context(|| format!("writing {}", path.display()))?;

    // stderr: `install` is occasionally piped, and this is advice, not output.
    eprintln!(
        "wrote {} ({})",
        path.display(),
        if args.remote { "hosted" } else { "local" }
    );
    if args.remote {
        eprintln!("set {KEY_ENV} in your environment; it is never written to {CONFIG_FILE}");
    }
    Ok(())
}

fn headers() -> Result<()> {
    let Ok(secret) = std::env::var(KEY_ENV) else {
        bail!("{KEY_ENV} is not set; `msafe keys add` mints one and prints it once");
    };
    if secret.trim().is_empty() {
        bail!("{KEY_ENV} is set but empty");
    }

    let out = serde_json::json!({
        "Authorization": format!("Bearer {}", secret.trim()),
        NAMESPACE_HEADER: namespace_from_cwd().as_str(),
    });
    println!("{out}");
    Ok(())
}

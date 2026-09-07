use crate::render;
use anyhow::{Context, Result, bail};
use clap::Args;
use memorysafe_backend::ScopeSelector;
use memorysafe_core::{Actor, ActorKind, Scope};
use memorysafe_engine::Engine;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use time::OffsetDateTime;

pub const NDJSON_FILE: &str = "memories.ndjson";
pub const MARKDOWN_FILE: &str = "memories.md";
pub const MANIFEST_FILE: &str = "manifest.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub exported_at: i64,
    pub tenant: String,
    pub subject: Option<String>,
    pub namespace: Option<String>,
    pub include_audit: bool,
    pub lines: usize,
    /// BLAKE3 of `memories.ndjson`, hex. Verified before an import writes
    /// anything: a truncated archive parses cleanly and would otherwise import
    /// as a smaller, silently wrong corpus.
    pub ndjson_blake3: String,
}

fn actor() -> Actor {
    Actor {
        kind: ActorKind::Cli,
        id: None,
    }
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Directory to write the archive into.
    pub out: PathBuf,
    /// Export every subject in the tenant, not just this one.
    #[arg(long)]
    pub all_subjects: bool,
    /// Export every namespace for the selected subject(s).
    #[arg(long)]
    pub all_namespaces: bool,
    /// Include the audit trail. Decisions and digests only; never bodies.
    #[arg(long)]
    pub include_audit: bool,
    /// Overwrite an existing archive.
    #[arg(long)]
    pub force: bool,
}

pub async fn export(
    engine: &Arc<Engine>,
    scope: Scope,
    json: bool,
    args: ExportArgs,
) -> Result<()> {
    if args.out.join(NDJSON_FILE).exists() && !args.force {
        bail!(
            "{} already contains an archive; pass --force to overwrite it",
            args.out.display()
        );
    }

    let selector = ScopeSelector {
        tenant: scope.tenant.clone(),
        // A subject-wide export cannot be narrowed to one namespace: the
        // selector's namespace only means anything under a named subject.
        subject: (!args.all_subjects).then(|| scope.subject.clone()),
        namespace: (!args.all_subjects && !args.all_namespaces).then(|| scope.namespace.clone()),
        include_audit: args.include_audit,
    };

    let ndjson = engine.export_ndjson_as(&selector, &actor()).await?;
    let markdown = engine.export_markdown(&selector).await?;

    std::fs::create_dir_all(&args.out)
        .with_context(|| format!("creating {}", args.out.display()))?;
    std::fs::write(args.out.join(NDJSON_FILE), &ndjson)?;
    std::fs::write(args.out.join(MARKDOWN_FILE), &markdown)?;

    let manifest = Manifest {
        format_version: 1,
        exported_at: OffsetDateTime::now_utc().unix_timestamp(),
        tenant: scope.tenant.to_string(),
        subject: selector.subject.as_ref().map(|s| s.to_string()),
        namespace: selector.namespace.as_ref().map(|n| n.to_string()),
        include_audit: args.include_audit,
        lines: ndjson.lines().count(),
        ndjson_blake3: blake3::hash(ndjson.as_bytes()).to_hex().to_string(),
    };
    std::fs::write(
        args.out.join(MANIFEST_FILE),
        serde_json::to_string_pretty(&manifest)?,
    )?;

    render::emit(json, &manifest, || {
        println!(
            "exported {} record line(s) to {}",
            manifest.lines,
            args.out.display()
        );
    })
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// An archive directory, or a bare `.ndjson` export stream.
    pub path: PathBuf,
}

pub async fn import(
    engine: &Arc<Engine>,
    scope: Scope,
    json: bool,
    args: ImportArgs,
) -> Result<()> {
    let ndjson = read_stream(&args.path)?;
    let report = engine
        .import_ndjson_as(&ndjson, &scope.tenant, &actor())
        .await?;
    render::emit(json, &report, || {
        println!(
            "imported {} · skipped {} already present · {} vector(s) · {} audit row(s)",
            report.items_imported,
            report.items_skipped_existing,
            report.vectors_imported,
            report.audit_imported
        );
    })
}

/// Reads and, when a manifest is present, verifies. Verification happens before
/// the caller touches the engine, so a corrupt archive changes nothing.
pub(crate) fn read_stream(path: &Path) -> Result<String> {
    if path.is_dir() {
        let ndjson_path = path.join(NDJSON_FILE);
        let ndjson = std::fs::read_to_string(&ndjson_path)
            .with_context(|| format!("reading {}", ndjson_path.display()))?;

        let manifest_path = path.join(MANIFEST_FILE);
        if manifest_path.exists() {
            let manifest: Manifest =
                serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)
                    .with_context(|| format!("parsing {}", manifest_path.display()))?;
            let actual = blake3::hash(ndjson.as_bytes()).to_hex().to_string();
            if actual != manifest.ndjson_blake3 {
                bail!(
                    "{} does not match the digest in {}: the archive is truncated or modified \
                     (expected {}, found {})",
                    ndjson_path.display(),
                    MANIFEST_FILE,
                    manifest.ndjson_blake3,
                    actual
                );
            }
        }
        Ok(ndjson)
    } else {
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
    }
}

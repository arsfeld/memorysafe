use crate::config::{DEFAULT_CONFIG_FILE, MsafeConfig};
use crate::render;
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use memorysafe_auth::{ApiKeyRecord, generate};
use memorysafe_core::{Namespace, SubjectId, TenantId};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Subcommand)]
pub enum KeysCommand {
    /// Create an API key for a tenant and subject. The secret is printed once and never stored.
    Add(AddArgs),
    /// List the key records this configuration holds. Never prints a secret.
    List,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    #[arg(long)]
    pub label: String,
    /// The subject this key acts as. Defaults to the configured subject.
    /// A key is bound to one subject: it cannot name another at call time.
    #[arg(long)]
    pub subject: Option<String>,
    /// The namespace this key falls back to when a request declares none.
    #[arg(long)]
    pub namespace: Option<String>,
}

#[derive(Serialize)]
struct KeysOutput {
    keys: Vec<ApiKeyRecord>,
}

#[derive(Serialize)]
struct AddedKey<'a> {
    secret: &'a str,
    id: &'a str,
    tenant: String,
    subject: &'a SubjectId,
    default_namespace: &'a Option<Namespace>,
    label: &'a str,
}

pub fn run(
    command: KeysCommand,
    tenant: &TenantId,
    subject: Option<&SubjectId>,
    config: &MsafeConfig,
    config_path: Option<&Path>,
    json: bool,
) -> Result<()> {
    match command {
        KeysCommand::List => {
            let output = KeysOutput {
                keys: config.keys.clone(),
            };
            render::emit(json, &output, || {
                if output.keys.is_empty() {
                    println!("no keys configured");
                }
                for key in &output.keys {
                    let state = if key.disabled { "disabled" } else { "active" };
                    println!(
                        "{}  {}  {}  {}  {}",
                        key.id, key.tenant, key.subject, state, key.label
                    );
                }
            })
        }
        KeysCommand::Add(args) => {
            // Writing a key into a config file the operator did not ask for is
            // how a credential ends up committed. Make them create it first.
            let path: PathBuf = match config_path {
                Some(path) => path.to_path_buf(),
                None => {
                    let candidate = PathBuf::from(DEFAULT_CONFIG_FILE);
                    if !candidate.exists() {
                        bail!(
                            "no configuration file to store the key record in; create {} first \
                             (or pass --config)",
                            DEFAULT_CONFIG_FILE
                        );
                    }
                    candidate
                }
            };

            let subject = match args.subject.as_deref() {
                Some(raw) => SubjectId::new(raw)?,
                None => subject
                    .cloned()
                    .context("resolving the configured subject for this key")?,
            };
            let mut generated = generate(tenant.clone(), subject, &args.label)?;
            if let Some(raw) = args.namespace.as_deref() {
                memorysafe_auth::check_reserved(None, Some(raw))?;
                generated.record.default_namespace = Some(Namespace::new(raw)?);
            }
            let mut updated = config.clone();
            updated.keys.push(generated.record.clone());
            updated.save(&path)?;

            let output = AddedKey {
                secret: &generated.secret,
                id: &generated.record.id,
                tenant: generated.record.tenant.to_string(),
                subject: &generated.record.subject,
                default_namespace: &generated.record.default_namespace,
                label: &generated.record.label,
            };
            render::emit(json, &output, || {
                println!("{}", generated.secret);
                println!(
                    "stored key {} for tenant {} in {}",
                    generated.record.id,
                    generated.record.tenant,
                    path.display()
                );
                println!("this secret is not recoverable — save it now");
            })
        }
    }
}

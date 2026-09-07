use crate::config::{MsafeConfig, ServeConfig};
use anyhow::{Context, Result};
use axum::Router;
use clap::Args;
use memorysafe_api::{AppState, router as api_router};
use memorysafe_auth::ApiKeyStore;
use memorysafe_core::{ADMIN_COMPONENT, Namespace, SubjectId, TenantId};
use memorysafe_engine::Engine;
use memorysafe_mcp::{HttpTransportConfig, ScopeSource, http_service_with, serve_stdio};
use std::sync::Arc;

const MAX_COMPONENT_BYTES: usize = 240;
const FALLBACK_NAMESPACE: &str = "default";

/// Turns a directory name into a legal `Namespace`. Lowercases, replaces every
/// character the component grammar forbids with `-`, truncates, and falls back
/// rather than refusing to start.
pub fn slug(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if out.len() >= MAX_COMPONENT_BYTES {
            break;
        }
        let lowered = ch.to_ascii_lowercase();
        if lowered.is_ascii_lowercase()
            || lowered.is_ascii_digit()
            || matches!(lowered, '-' | '_' | '.')
        {
            out.push(lowered);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }

    let trimmed = out.trim_matches(|c| c == '-' || c == '.').to_owned();
    if trimmed.is_empty() || trimmed == ADMIN_COMPONENT || Namespace::new(&trimmed).is_err() {
        return FALLBACK_NAMESPACE.to_owned();
    }
    trimmed
}

pub fn namespace_from_cwd() -> Namespace {
    let name = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    Namespace::new(&slug(&name)).unwrap_or_else(|_| {
        Namespace::new(FALLBACK_NAMESPACE).expect("the fallback namespace is legal")
    })
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    #[arg(long, default_value = "stdio", value_parser = ["stdio", "http"])]
    pub transport: String,
    /// Overrides `serve.bind` from the configuration file.
    #[arg(long)]
    pub bind: Option<String>,
}

pub fn http_router(engine: Arc<Engine>, keys: Arc<ApiKeyStore>, serve: &ServeConfig) -> Router {
    let mcp = http_service_with(
        engine.clone(),
        ScopeSource::Http { keys: keys.clone() },
        HttpTransportConfig {
            allowed_hosts: serve.allowed_hosts.clone(),
        },
    );
    api_router(AppState { engine, keys }).nest_service(&serve.mcp_path, mcp)
}

/// `msafe serve --transport stdio`. Tenant and subject are fixed for the
/// whole session; the namespace is never taken from a flag — it defaults
/// from the working directory (`namespace_from_cwd`) and a call may override
/// it once the session is running. Callers resolve only `tenant`/`subject`
/// before reaching here (see `main.rs::resolve_tenant_and_subject`): this
/// transport never reads a `--namespace`/`MSAFE_NAMESPACE`, so requiring one
/// up front would only ever block reaching the cwd-derived default this
/// transport exists to use — that was the actual, shipped defect (Tasks
/// 11-14 consolidated review, Fix 1).
pub async fn serve_stdio_transport(
    engine: Arc<Engine>,
    tenant: TenantId,
    subject: SubjectId,
) -> Result<()> {
    let source = ScopeSource::Stdio {
        tenant,
        subject,
        default_namespace: namespace_from_cwd(),
    };
    // Nothing is printed here: stdout is the transport.
    serve_stdio(engine, source).await
}

/// `msafe serve --transport http`. Reads no scope component at startup at
/// all — `ScopeSource::Http` derives tenant, subject and namespace per
/// request from the presented API key — so this takes no tenant/subject/
/// namespace argument either.
pub async fn serve_http_transport(
    engine: Arc<Engine>,
    config: &MsafeConfig,
    args: &ServeArgs,
) -> Result<()> {
    let keys = Arc::new(ApiKeyStore::new(config.keys.clone()));
    let bind = args.bind.as_deref().unwrap_or(&config.serve.bind);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    let address = listener.local_addr()?;
    // stderr, so this stays usable when stdout is piped somewhere.
    eprintln!(
        "msafe listening on http://{address} (MCP at {})",
        config.serve.mcp_path
    );

    axum::serve(listener, http_router(engine, keys, &config.serve))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("serving HTTP")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_name_becomes_a_legal_namespace() {
        assert_eq!(slug("checkout-service"), "checkout-service");
        assert_eq!(slug("Checkout Service"), "checkout-service");
        assert_eq!(slug("My_Project.v2"), "my_project.v2");
        assert_eq!(slug("café ☕"), "caf");
    }

    #[test]
    fn an_unusable_directory_name_falls_back_rather_than_failing_to_start() {
        // A server that will not start because the folder is called "☕" is
        // worse than one that uses a sensible default.
        assert_eq!(slug(""), "default");
        assert_eq!(slug("☕"), "default");
        assert_eq!(slug("..."), "default");
        assert_eq!(slug("---"), "default");
    }

    #[test]
    fn a_very_long_directory_name_is_truncated_to_a_legal_component() {
        let long = "a".repeat(500);
        let slugged = slug(&long);
        assert!(slugged.len() <= 240);
        assert!(memorysafe_core::Namespace::new(&slugged).is_ok());
    }

    #[test]
    fn every_slug_this_produces_is_a_legal_namespace() {
        for raw in [
            "checkout-service",
            "Checkout Service",
            "café ☕",
            "",
            "☕",
            "...",
            "_admin",
        ] {
            let slugged = slug(raw);
            assert!(
                memorysafe_core::Namespace::new(&slugged).is_ok(),
                "slug({raw:?}) = {slugged:?} is not a legal namespace"
            );
        }
    }

    #[test]
    fn the_reserved_component_is_never_produced_by_slugging() {
        assert_ne!(slug("_admin"), memorysafe_core::ADMIN_COMPONENT);
    }
}

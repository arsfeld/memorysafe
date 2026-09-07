use crate::render;
use anyhow::Result;
use clap::Args;
use memorysafe_backend::Page;
use memorysafe_core::{
    Actor, ActorKind, RecallBudget, RecallMode, RecallRequest, Scope, SensitivityLevel,
};
use memorysafe_engine::{Engine, RememberRequest};
use serde::Serialize;
use std::sync::Arc;
use time::Duration;

#[derive(Debug, Args)]
pub struct RememberArgs {
    /// The memory to store.
    pub body: String,
    #[arg(long, default_value = "fact")]
    pub kind: String,
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    #[arg(long)]
    pub ttl_seconds: Option<i64>,
    #[arg(long, value_parser = parse_sensitivity)]
    pub sensitivity: Option<SensitivityLevel>,
    #[arg(long)]
    pub idempotency_key: Option<String>,
}

fn parse_sensitivity(raw: &str) -> Result<SensitivityLevel, String> {
    serde_json::from_value(serde_json::Value::String(raw.to_owned())).map_err(|_| {
        format!("expected public, internal, personal, sensitive, or restricted, got '{raw}'")
    })
}

pub async fn remember(
    engine: &Arc<Engine>,
    scope: Scope,
    json: bool,
    args: RememberArgs,
) -> Result<()> {
    let mut req = RememberRequest::new(scope, &args.body);
    req.actor = Actor {
        kind: ActorKind::Cli,
        id: None,
    };
    req.kind = args.kind;
    req.tags = args.tags;
    req.ttl = args.ttl_seconds.map(Duration::seconds);
    req.sensitivity_hint = args.sensitivity;
    req.idempotency_key = args.idempotency_key;

    let outcome = engine.remember(req).await?;
    render::emit(json, &outcome, || render::write_outcome(&outcome))
}

#[derive(Debug, Args)]
pub struct RecallArgs {
    /// What to recall. Omit to ask "what do you remember here?".
    pub query: Option<String>,
    #[arg(long, default_value = "working-set", value_parser = ["working-set", "search"])]
    pub mode: String,
    #[arg(long)]
    pub max_tokens: Option<u32>,
    #[arg(long)]
    pub max_items: Option<usize>,
    #[arg(long = "tag")]
    pub tags_any: Vec<String>,
    #[arg(long = "kind")]
    pub kinds: Vec<String>,
    #[arg(long, value_parser = parse_sensitivity)]
    pub sensitivity_ceiling: Option<SensitivityLevel>,
}

pub async fn recall(
    engine: &Arc<Engine>,
    scope: Scope,
    json: bool,
    args: RecallArgs,
) -> Result<()> {
    let defaults = RecallBudget::default();
    let request = RecallRequest {
        scope,
        query: args.query,
        tags_any: args.tags_any,
        kinds: args.kinds,
        occurred_after: None,
        occurred_before: None,
        mode: if args.mode == "search" {
            RecallMode::Search
        } else {
            RecallMode::WorkingSet
        },
        budget: RecallBudget {
            max_tokens: args.max_tokens.or(defaults.max_tokens),
            max_items: args.max_items.or(defaults.max_items),
        },
        // Fail closed, matching `memorysafe_backend::query::HardFilters`'s own
        // documented default for this exact field and both network adapters'
        // identical default (`memorysafe-mcp`'s `tools_write.rs`,
        // `memorysafe-api`'s `memories.rs`): the cost of guessing too narrow
        // (a visible, reported annoyance a caller fixes by passing the
        // argument) is not symmetric with the cost of guessing too wide (a
        // silent over-disclosure nobody notices). A local operator who trusts
        // the machine and owns the database file can still ask for everything
        // with `--sensitivity-ceiling restricted`; a coding-agent harness
        // that shells out to `msafe recall` with no override should not get
        // every credential in scope by default, where the MCP path withholds
        // them.
        sensitivity_ceiling: args
            .sensitivity_ceiling
            .unwrap_or(SensitivityLevel::Internal),
    };

    let ws = engine.recall(request).await?;
    render::emit(json, &ws, || render::working_set(&ws))
}

#[derive(Debug, Args)]
pub struct ReviewArgs {
    #[arg(long)]
    pub limit: Option<usize>,
    #[arg(long)]
    pub offset: Option<usize>,
}

#[derive(Serialize)]
struct ReviewOutput {
    items: Vec<memorysafe_core::MemoryItem>,
    offset: usize,
    limit: usize,
}

pub async fn review(
    engine: &Arc<Engine>,
    scope: Scope,
    json: bool,
    args: ReviewArgs,
) -> Result<()> {
    let defaults = Page::default();
    let page = Page {
        offset: args.offset.unwrap_or(defaults.offset),
        limit: args.limit.unwrap_or(defaults.limit),
    };
    let items = engine.review(&scope, &page).await?;
    let output = ReviewOutput {
        items,
        offset: page.offset,
        // Not `page.limit`: the backend clamps to `Page::effective_limit()`
        // (`MAX_PAGE_LIMIT`) before running the query, and this echo is the
        // only exhaustion signal a caller has — no `truncated` flag exists,
        // by this workspace's own paging convention (`AuditFilter::limit`'s
        // doc states the same rule; `memorysafe-api::memories::review` and
        // `memorysafe-mcp::tools_curate::memory_review` carry the identical
        // fix and comment). Echoing the raw, unclamped request would make a
        // caller asking for more than the ceiling see fewer items than the
        // (wrong) limit it was told, and wrongly conclude the scope was
        // exhausted — silently hiding part of what is stored.
        limit: page.effective_limit(),
    };
    render::emit(json, &output, || render::items(&output.items))
}

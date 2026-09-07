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
        sensitivity_ceiling: args
            .sensitivity_ceiling
            .unwrap_or(SensitivityLevel::Restricted),
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
        limit: page.limit,
    };
    render::emit(json, &output, || render::items(&output.items))
}

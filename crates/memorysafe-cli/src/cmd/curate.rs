use crate::render;
use anyhow::{Result, bail};
use clap::Args;
use memorysafe_core::{
    Actor, ActorKind, AuditEvent, AuditFilter, AuditId, AuditRecord, ItemId, Scope, SubjectId,
    TenantId, parse_protection,
};
use memorysafe_engine::{Engine, ForgetSelector, MaintainCursor};
use serde::Serialize;
use std::sync::Arc;
use time::OffsetDateTime;

#[derive(Debug, Args)]
pub struct ForgetArgs {
    #[arg(long = "id")]
    pub ids: Vec<String>,
    #[arg(long)]
    pub tag: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
}

pub async fn forget(
    engine: &Arc<Engine>,
    scope: Scope,
    json: bool,
    args: ForgetArgs,
) -> Result<()> {
    let given = [
        !args.ids.is_empty(),
        args.tag.is_some(),
        args.kind.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    if given != 1 {
        bail!("pass exactly one of --id (repeatable), --tag, or --kind");
    }

    let selector = if !args.ids.is_empty() {
        ForgetSelector::Ids(
            args.ids
                .iter()
                .map(|raw| ItemId::parse(raw))
                .collect::<Result<_, _>>()?,
        )
    } else if let Some(tag) = args.tag {
        ForgetSelector::Tag(tag)
    } else {
        ForgetSelector::Kind(args.kind.expect("checked above"))
    };

    let outcome = engine.forget(&scope, selector).await?;
    render::emit(json, &outcome, || {
        if outcome.forgotten.is_empty() {
            println!("nothing matched");
        }
        for id in &outcome.forgotten {
            println!("forgot {id}");
        }
        println!("audit {}", outcome.audit_id);
    })
}

#[derive(Debug, Args)]
pub struct ProtectArgs {
    pub id: String,
    #[arg(long, default_value = "pinned", value_parser = ["normal", "protected", "pinned"])]
    pub level: String,
    /// Unix seconds. Required for --level protected.
    #[arg(long)]
    pub until: Option<i64>,
}

pub async fn protect(
    engine: &Arc<Engine>,
    scope: Scope,
    json: bool,
    args: ProtectArgs,
) -> Result<()> {
    let protection =
        parse_protection(&args.level, args.until).map_err(|e| anyhow::anyhow!("{e}"))?;

    let id = ItemId::parse(&args.id)?;
    let outcome = engine.protect(&scope, &id, protection).await?;
    render::emit(json, &outcome, || render::write_outcome(&outcome))
}

#[derive(Debug, Args)]
pub struct AuditArgs {
    #[arg(long = "event")]
    pub events: Vec<String>,
    #[arg(long)]
    pub item: Option<String>,
    #[arg(long)]
    pub since: Option<i64>,
    #[arg(long)]
    pub until: Option<i64>,
    #[arg(long)]
    pub after: Option<String>,
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct AuditOutput {
    pub records: Vec<AuditRecord>,
    pub truncated: bool,
}

pub async fn audit(engine: &Arc<Engine>, scope: Scope, json: bool, args: AuditArgs) -> Result<()> {
    let events = args
        .events
        .iter()
        .map(|raw| {
            serde_json::from_value::<AuditEvent>(serde_json::Value::String(raw.clone()))
                .map_err(|_| anyhow::anyhow!("unknown audit event '{raw}'"))
        })
        .collect::<Result<Vec<_>>>()?;

    let defaults = AuditFilter::default();
    let requested = args.limit.unwrap_or(defaults.limit);
    let filter = AuditFilter {
        events,
        subject: None,
        namespace: None,
        after: args.after.as_deref().map(AuditId::parse).transpose()?,
        item: args.item.as_deref().map(ItemId::parse).transpose()?,
        since: args
            .since
            .map(OffsetDateTime::from_unix_timestamp)
            .transpose()?,
        until: args
            .until
            .map(OffsetDateTime::from_unix_timestamp)
            .transpose()?,
        limit: requested,
    };

    let (records, truncated) = engine.audit_page(&scope, &filter, requested).await?;
    let output = AuditOutput { records, truncated };

    render::emit(json, &output, || {
        for record in &output.records {
            let codes: Vec<String> = record
                .decision
                .as_ref()
                .map(|d| {
                    d.reasons
                        .iter()
                        .map(|r| format!("{:?}", r.code).to_lowercase())
                        .collect()
                })
                .unwrap_or_default();
            let event_str = format!("{:?}", record.event).to_lowercase();
            println!(
                "{}  {}  {} item(s)  {}",
                record.at,
                event_str,
                record.items.len(),
                codes.join(", ")
            );
        }
        if output.truncated {
            println!("(truncated — continue with --after <id of the last record>)");
        }
    })
}

#[derive(Debug, Args)]
pub struct MaintainArgs {
    #[arg(long)]
    pub cursor: Option<usize>,
    /// Keep going until the cursor is exhausted.
    #[arg(long)]
    pub all: bool,
}

pub async fn maintain(
    engine: &Arc<Engine>,
    scope: Scope,
    json: bool,
    args: MaintainArgs,
) -> Result<()> {
    let mut cursor = args.cursor.map(|offset| MaintainCursor { offset });
    let mut report = engine.maintain(&scope, cursor.take()).await?;

    if args.all {
        while let Some(next) = report.next_cursor {
            let page = engine.maintain(&scope, Some(next)).await?;
            report.scanned += page.scanned;
            report.forgotten += page.forgotten;
            report.protection_released += page.protection_released;
            report.next_cursor = page.next_cursor;
        }
    }

    render::emit(json, &report, || {
        println!(
            "scanned {} · forgot {} · released {} protection window(s)",
            report.scanned, report.forgotten, report.protection_released
        );
        if let Some(next) = report.next_cursor {
            println!("more to do — resume with --cursor {}", next.offset);
        }
    })
}

#[derive(Debug, Args)]
pub struct PurgeArgs {
    pub subject: String,
    /// Required. This deletes everything the subject owns.
    #[arg(long)]
    pub yes: bool,
}

pub async fn purge_subject(
    engine: &Arc<Engine>,
    tenant: &TenantId,
    json: bool,
    args: PurgeArgs,
) -> Result<()> {
    memorysafe_auth::check_reserved(Some(&args.subject), None)?;
    if !args.yes {
        bail!(
            "purging removes every memory belonging to '{}' and cannot be undone; pass --yes to \
             confirm",
            args.subject
        );
    }
    let subject = SubjectId::new(&args.subject)?;
    let outcome = engine
        .purge_subject(
            tenant,
            &subject,
            // No id to supply — the CLI authenticates the process, not a
            // person — but the kind is real and is more than the anonymous
            // `Human` Plan 1 wrote. See `Engine::purge_subject`.
            &Actor {
                kind: ActorKind::Cli,
                id: None,
            },
        )
        .await?;
    render::emit(json, &outcome, || {
        println!(
            "removed {} item(s); audit rows removed {} preserved {}",
            outcome.items_removed, outcome.audit_rows_removed, outcome.audit_rows_preserved
        );
    })
}

use crate::Engine;
use crate::error::EngineError;
use memorysafe_backend::{
    ExportRecord, ExportStream, ImportReport, ImportStream, ScopeSelector, WriteTransaction,
};
use memorysafe_core::{
    ADMIN_COMPONENT, Actor, AuditEvent, AuditRecord, ItemRef, Namespace, Protection, Scope,
    SubjectId, TenantId,
};
use time::{Duration, OffsetDateTime};

impl Engine {
    pub async fn export(&self, sel: &ScopeSelector) -> Result<ExportStream, EngineError> {
        Ok(self.backend.export(sel).await?)
    }

    /// Imported items are re-assessed rather than trusted. An import stream is
    /// caller-supplied JSON, and `MemoryItem`'s fields are public — so a
    /// stream can claim `sensitivity: "public"` for a body full of
    /// credentials and, if believed, that body would then satisfy a
    /// Public-clearance recall. `Backend::import` trusts neither
    /// `protection` nor `sensitivity` — its own doc is explicit that the
    /// backend layer stores both exactly as the stream carries them, leaving
    /// the decision of whether a stream is trusted input to its caller. This
    /// is that caller.
    ///
    /// **Sensitivity.** Recomputed the same way a write would detect it, and
    /// kept only if it is *higher* than the payload's own claim — so an
    /// import can raise a stored classification but never lower one a
    /// legitimate write already earned (a hint-earned `Restricted` on an
    /// otherwise ordinary body must survive a round trip; see
    /// `an_import_cannot_use_fresh_detection_to_downgrade_an_already_high_classification`).
    ///
    /// This detection deliberately does **not** go through the configured
    /// `self.policy: Arc<dyn GovernancePolicy>` the way `remember` does.
    /// `GovernancePolicy::assess` requires an `AssessContext` — nearest
    /// neighbours and scope statistics — which costs a `Backend::neighbours`
    /// and a `Backend::scope_stats` round trip *per item*, and neighbour
    /// lookup additionally needs a real (not quantized) embedding, which an
    /// exported item does not carry. Paying that cost for every item in a
    /// bulk import was judged too expensive for this task, so the detector
    /// runs directly against `memorysafe_policy::sensitivity::assess` with
    /// `BaselineConfig::default()` — the pattern/lexicon floor, not whatever
    /// config or custom scorer this deployment actually runs. **The
    /// consequence is real and is stated here rather than left implicit:** a
    /// deployment that has *tightened* detection (for example lowered
    /// `credential_token_min_len` below its default of 20) gets the
    /// *looser* default classification on this one path — which is also the
    /// one path that takes untrusted input — and a deployment running any
    /// custom, non-`BaselinePolicy` `GovernancePolicy` gets no policy-driven
    /// classification benefit at all on import, only this baseline floor.
    /// `import_uses_the_baseline_floor_not_a_deployments_tightened_config`
    /// pins this as current, known behaviour rather than an assumption.
    ///
    /// **Protection.** Two claims are not trusted, and for different
    /// reasons — the earlier draft of this doc reasoned about this
    /// variant-by-variant ("which `Protection` values can a real admission
    /// decision produce") and that was the wrong question, because an import
    /// controls *values*, not just variants:
    ///
    /// - `Protection::Pinned` is downgraded to `Protection::Normal`
    ///   unconditionally. For `BaselinePolicy`, `admit::decide` never
    ///   produces `Pinned` on its own (see `memorysafe_policy::admit`) — the
    ///   only path to it is an explicit `Engine::protect` call, or a
    ///   `maintain`-time `Retain` routed through `protect`. **This is true
    ///   for `BaselinePolicy` specifically, not in general**: `Action::Retain
    ///   { protection }` is applied verbatim in `write.rs` regardless of
    ///   which policy produced it, `self.policy` is an
    ///   `Arc<dyn GovernancePolicy>`, and nothing stops a custom policy from
    ///   legally setting `Pinned` at admission. `Pinned` carries no bound to
    ///   clamp — it is a flag, not a window — so there is no value-level
    ///   defence available for it the way there is for `Protected`; the
    ///   engine cannot tell a policy-earned pin from a forged one at import
    ///   time without asking the policy, which is exactly the I/O this path
    ///   avoids. The accepted, documented trade-off for this plan (which
    ///   ships only `BaselinePolicy`) is that import strips every claimed
    ///   pin, including a hypothetical legitimate one from a future custom
    ///   policy; that stripping is silent, and is recorded here rather than
    ///   guarded against.
    /// - `Protection::Protected { until }` is preserved as a variant but its
    ///   *value* is clamped: `until` is capped to `now +
    ///   protection_window_days` (30 days by default). A genuine admission
    ///   decision only ever produces a window bounded by that same
    ///   configured span (`admit::decide`: `until: ctx.now +
    ///   Duration::days(cfg.protection_window_days)`), so the clamp is a
    ///   no-op on honest input — a real export's `until` is already within
    ///   bound from its own creation, and an older export's `until` is
    ///   further still (often already in the past, which the clamp also
    ///   leaves alone: only an `until` *exceeding* the bound is pulled down).
    ///   Without this clamp, a stream claiming
    ///   `"protection":{"kind":"protected","until":253402300799}` (the
    ///   year 9999) is indistinguishable from a forged pin to every consumer
    ///   in the codebase — `Protection::is_evictable` returns `false` until
    ///   then, `gather` never offers it as an eviction candidate,
    ///   `validate::decision` refuses to evict it, `maintain`'s merge
    ///   side-door refuses it, `maintain`'s release path only fires once
    ///   `until <= now` (never, in practice) — while also permanently
    ///   consuming the namespace's budget, since `portability::import` calls
    ///   `capacity::adjust` with no budget check of its own.
    ///
    /// **The import audits itself.** One `AuditEvent::Imported` record is
    /// written after the backend commits, filed under the destination tenant's
    /// reserved administrative scope (`memorysafe_core::ADMIN_COMPONENT` as
    /// both subject and namespace — an import is a tenant-level event and a
    /// subject spans namespaces, exactly the situation that constant exists
    /// for). Without it the one path that injects a whole corpus into a tenant
    /// left no trace of itself at all: the imported *items* are auditable, and
    /// the act of importing them was not.
    ///
    /// **`Actor::system()` here, but not always.** This method (`import`,
    /// called directly with no actor in hand) writes `Actor::system()`, the
    /// same placeholder `forget` and `protect` still use. The actor-attributed
    /// case — the caller identified in `Engine::import_ndjson_as` (`lib.rs`,
    /// Plan 3's Task 2) — reuses this same write path through `import_as`
    /// below rather than layering a second `Imported` row on top: exactly one
    /// row is written per import either way, naming whichever actor the
    /// caller had.
    ///
    /// **What the record carries, and the one thing it does not.** Its scope
    /// names the destination tenant; its `items` name every item record the
    /// stream carried into that tenant, so `items.len()` is an exact record
    /// count — but it is the count of items *offered*, which is
    /// `items_imported + items_skipped_existing`, since the backend decides
    /// which of them already existed and does not report *which*. The
    /// remaining `ImportReport` figures — `vectors_imported`, `audit_imported`
    /// and the imported/skipped split — are returned to the caller and are
    /// **not** in the row, because `AuditRecord` has no numeric field that is
    /// free to carry them: `decision` keys the row's `audit_aggregates` bucket
    /// on its `PolicyId` (`aggregates::increment`), and `assessment` adds a
    /// sample to that bucket's value and fragility histograms — so smuggling
    /// counts through either would file a non-policy event under a fabricated
    /// policy name, or push zeros into the score distributions. A dedicated
    /// field on `AuditRecord` is the right home and is a `memorysafe-core`
    /// wire-format change; ledgered, not made here.
    ///
    /// **The record is not atomic with the import it describes.**
    /// `Backend::import` takes no `AuditRecord` (unlike `purge_subject`, which
    /// does), so the row is a second transaction: a crash between them leaves
    /// an import that happened with no record of itself. Closing that is a
    /// `Backend` signature change, which belongs with the next contract batch.
    ///
    /// **Audit rows in the *stream* are not part of either defence.** This function maps
    /// only `ExportRecord::Item`; `ExportRecord::Header` and
    /// `ExportRecord::Audit` pass through the `other => other` arm
    /// untouched. That asymmetry — items re-assessed, audit rows trusted
    /// outright — is real, and it is **unexamined, not deliberate**: nothing
    /// in this task's design considered whether an import stream's audit
    /// rows should be trusted before storing them. A crafted stream can
    /// inject arbitrary audit history into the destination tenant,
    /// including a fabricated `SubjectPurged` row manufacturing evidence
    /// that an erasure occurred that never did, and `Reason::detail` (a free
    /// `String` reachable through `Decision::reasons`, itself embedded in
    /// `AuditRecord`) is unconstrained text an attacker-controlled import
    /// could use to smuggle an item body into an audit row — a route around
    /// the "audit rows never contain item bodies" constraint that nothing
    /// here validates against. Whether audit content should be validated on
    /// import is a design decision beyond this task's scope; it is recorded
    /// here rather than fixed.
    pub async fn import(
        &self,
        destination: &TenantId,
        stream: ImportStream,
    ) -> Result<ImportReport, EngineError> {
        self.import_as(destination, stream, &Actor::system()).await
    }

    /// `import`'s body, parameterised on the actor its one `Imported` row
    /// names. `import` itself supplies `Actor::system()`; `import_ndjson_as`
    /// (via `import_ndjson_with_actor` below) supplies the caller's own —
    /// there is exactly one construction site for the `Imported` row
    /// (`audit` a few lines down), so the two callers can never produce two
    /// rows for the one import between them.
    pub(crate) async fn import_as(
        &self,
        destination: &TenantId,
        stream: ImportStream,
        actor: &Actor,
    ) -> Result<ImportReport, EngineError> {
        let cfg = memorysafe_policy::BaselineConfig::default();
        let now = OffsetDateTime::now_utc();
        let max_protected_until = now + Duration::days(cfg.protection_window_days);

        let reassessed: ImportStream = stream
            .into_iter()
            .map(|record| match record {
                ExportRecord::Item { mut item, vector } => {
                    let candidate = memorysafe_core::Candidate {
                        body: item.body.clone(),
                        kind: item.kind.clone(),
                        tags: item.tags.clone(),
                        attrs: item.attrs.clone(),
                        sensitivity_hint: None,
                        embedding: None,
                        byte_size: item.byte_size(),
                    };
                    let detected = memorysafe_policy::sensitivity::assess(&candidate, &cfg).level;
                    item.sensitivity = item.sensitivity.max(detected);
                    item.protection = match item.protection {
                        Protection::Pinned => Protection::Normal,
                        Protection::Protected { until } if until > max_protected_until => {
                            Protection::Protected {
                                until: max_protected_until,
                            }
                        }
                        other => other,
                    };
                    ExportRecord::Item { item, vector }
                }
                other => other,
            })
            .collect();

        // Read off the reassessed stream, before it is moved into the backend
        // — and from the *reassessed* stream rather than the caller's, so the
        // digests in these refs are of what was actually stored, not of what
        // was offered. (`ItemRef::from_item` hashes body/kind/tags, none of
        // which the reassessment touches, so the two agree today; taking them
        // from the stored side means they still agree if that ever changes.)
        let refs: Vec<ItemRef> = reassessed
            .iter()
            .filter_map(|r| match r {
                ExportRecord::Item { item, .. } => Some(ItemRef::from_item(item)),
                _ => None,
            })
            .collect();
        // Every scope the import can have written to. `Backend::import`
        // preserves each item's own subject and namespace and only *checks*
        // its tenant against `destination`, so the scopes are the items' own.
        let mut written: Vec<Scope> = reassessed
            .iter()
            .filter_map(|r| match r {
                ExportRecord::Item { item, .. } => Some(item.scope.clone()),
                _ => None,
            })
            .collect();
        written.sort();
        written.dedup();

        let report = self.backend.import(destination, reassessed).await?;

        // `_admin`/`_admin`: an import is a tenant-level event and a subject
        // spans namespaces, which is what `ADMIN_COMPONENT` is reserved for.
        // The alternative — filing it under the first imported item's scope —
        // would make a tenant-wide event look like one subject's, and would
        // have nothing to file at all for a header-only stream.
        let audit_scope = Scope {
            tenant: destination.clone(),
            subject: SubjectId::new(ADMIN_COMPONENT)
                .expect("the reserved component is a valid subject"),
            namespace: Namespace::new(ADMIN_COMPONENT)
                .expect("the reserved component is a valid namespace"),
        };
        // `actor.clone()`: see this method's doc for who that is on each of
        // its two call paths.
        let audit = AuditRecord::new(
            audit_scope.clone(),
            AuditEvent::Imported,
            refs,
            actor.clone(),
            OffsetDateTime::now_utc(),
        );
        self.backend
            .apply(WriteTransaction::new(audit_scope, audit))
            .await?;

        // An import is a corpus change, so it invalidates like every other
        // one — see `EngineCache`'s doc, which used to claim every corpus
        // change this engine can make went through one of its enumerated
        // surfaces while this one went through none of them. Every namespace
        // the stream wrote into, individually, for the reason `purge_subject`
        // gives: `invalidate_scope` takes one `Scope` and an import spans as
        // many as the stream carries.
        for scope in &written {
            self.cache.invalidate_scope(scope).await;
        }

        Ok(report)
    }

    /// The round-trip format: one JSON object per line.
    pub async fn export_ndjson(&self, sel: &ScopeSelector) -> Result<String, EngineError> {
        let stream = self.export(sel).await?;
        let mut out = String::new();
        for record in &stream {
            let line = serde_json::to_string(record)
                .map_err(|e| EngineError::Validation(e.to_string()))?;
            out.push_str(&line);
            out.push('\n');
        }
        Ok(out)
    }

    /// `destination` is passed straight through to `Backend::import`. The
    /// engine does not derive it from the stream: an authorising caller — the
    /// HTTP route's API key, the CLI's `--scope` — already knows which tenant
    /// it is writing into, and deriving it here would put the payload back in
    /// charge of its own destination.
    pub async fn import_ndjson(
        &self,
        ndjson: &str,
        destination: &TenantId,
    ) -> Result<ImportReport, EngineError> {
        self.import_ndjson_with_actor(ndjson, destination, &Actor::system())
            .await
    }

    /// `import_ndjson`'s body, parameterised on the actor `import_as` should
    /// name. `Engine::import_ndjson_as` (`lib.rs`) is this method's other
    /// caller, supplying the caller-identified actor instead of
    /// `Actor::system()` — see `import_as`'s own doc for why routing through
    /// this shared parse-then-import path, rather than calling
    /// `import_ndjson` and separately auditing the result, is what keeps the
    /// `Imported` row to exactly one.
    pub(crate) async fn import_ndjson_with_actor(
        &self,
        ndjson: &str,
        destination: &TenantId,
        actor: &Actor,
    ) -> Result<ImportReport, EngineError> {
        let mut stream: ImportStream = Vec::new();
        for (i, line) in ndjson.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let record: ExportRecord = serde_json::from_str(line)
                .map_err(|e| EngineError::Validation(format!("line {}: {e}", i + 1)))?;
            stream.push(record);
        }
        self.import_as(destination, stream, actor).await
    }

    /// A human-readable rendering. This is what makes "your memory is yours"
    /// mean something a person can open, rather than a JSON blob.
    ///
    /// `body`, `kind` and each tag are attacker-controlled on the import path
    /// this same task opens (`MemoryItem`'s fields are public, and nothing
    /// about markdown export re-validates what an import already stored), so
    /// each is escaped through [`escape_markdown_structure`] before being
    /// interpolated. Without that, a body containing a newline followed by
    /// `## acme / victim / ns` would render as a genuine-looking scope
    /// section for a tenant this item was never in — misrepresenting
    /// provenance in the one artifact whose whole purpose is to be legible.
    pub async fn export_markdown(&self, sel: &ScopeSelector) -> Result<String, EngineError> {
        let stream = self.export(sel).await?;
        let mut out = String::from("# MemorySafe export\n\n");

        let mut current_scope: Option<String> = None;
        for record in &stream {
            let ExportRecord::Item { item, .. } = record else {
                continue;
            };
            // Scope components are validated at construction to a narrow
            // alphabet (lowercase ascii alphanumerics, `-`, `_`, `.`; see
            // `memorysafe_core::ids::validate_component`) that admits no `#`
            // and no newline, so this label needs no escaping — unlike
            // `kind`, `tags` and `body` below, which carry no such
            // constraint and are fully attacker-controlled on the import
            // path.
            let scope_label = format!(
                "{} / {} / {}",
                item.scope.tenant, item.scope.subject, item.scope.namespace
            );
            if current_scope.as_deref() != Some(scope_label.as_str()) {
                out.push_str(&format!("## {scope_label}\n\n"));
                current_scope = Some(scope_label);
            }

            out.push_str(&format!("### {}\n\n", item.id));
            out.push_str(&format!(
                "- **kind:** {}\n",
                escape_markdown_structure(&item.kind)
            ));
            out.push_str(&format!("- **created:** {}\n", item.created_at));
            out.push_str(&format!("- **sensitivity:** {:?}\n", item.sensitivity));
            out.push_str(&format!("- **protection:** {:?}\n", item.protection));
            if !item.tags.is_empty() {
                let tags: Vec<String> = item
                    .tags
                    .iter()
                    .map(|t| escape_markdown_structure(t))
                    .collect();
                out.push_str(&format!("- **tags:** {}\n", tags.join(", ")));
            }
            out.push_str(&format!("\n{}\n\n", escape_markdown_structure(&item.body)));
        }

        Ok(out)
    }
}

/// Neutralises the one provenance-forging shape identified in review: an
/// attacker-controlled body (or `kind`, or a tag) containing a line that
/// starts with `#` would otherwise render as a genuine ATX heading —
/// concretely, `## <tenant> / <subject> / <namespace>` impersonating this
/// exporter's own scope-section syntax. Escapes a leading `#` run (after
/// optional indentation) on each line by inserting a backslash before it,
/// which CommonMark parses as a literal `#` rather than heading syntax. This
/// is **not** a general markdown sanitiser — it defends against the specific
/// header-impersonation risk raised in review, not every way markdown syntax
/// could be misused in interpolated text.
fn escape_markdown_structure(text: &str) -> String {
    text.lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if let Some(stripped) = trimmed.strip_prefix('#') {
                let indent = &line[..line.len() - trimmed.len()];
                format!("{indent}\\#{stripped}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// `import` is a corpus change and joins the invalidation surfaces
// `EngineCache`'s doc comment enumerates — it was the one that did not, which
// is what made that doc's "every corpus change this engine can make goes
// through one of those surfaces" false rather than merely miscounted. A unit
// test rather than one in `tests/portability.rs` for the same reason
// `mutate.rs`'s and `reembed.rs`'s are: `Engine::cache` is `pub(crate)` and
// invalidation is not observable from outside the crate.
#[cfg(test)]
mod cache_invalidation_tests {
    use super::*;
    use crate::write::RememberRequest;
    use crate::{Engine, EngineConfig};
    use memorysafe_backend_sqlite::SqliteBackend;
    use memorysafe_core::ScopeStats;
    use memorysafe_embed::DeterministicEmbedder;
    use memorysafe_policy::BaselinePolicy;
    use std::sync::Arc;

    fn engine() -> Engine {
        let dir = tempfile::tempdir().expect("tempdir");
        Engine::new(EngineConfig::new(
            Arc::new(SqliteBackend::open(dir.keep())),
            Arc::new(DeterministicEmbedder::new(256)),
            Arc::new(BaselinePolicy::default()),
        ))
    }

    fn sentinel() -> ScopeStats {
        ScopeStats {
            item_count: 999_999,
            ..Default::default()
        }
    }

    /// An import spans as many scopes as its stream carries, so it must
    /// invalidate each of them and nothing else.
    ///
    /// **The negative control is the point.** Seeding only the scopes the
    /// import writes to would pass against an implementation that flushed the
    /// whole cache, which is a different (and worse) behaviour than the one
    /// claimed. `untouched` is a namespace under the same tenant that the
    /// stream never mentions.
    #[tokio::test]
    async fn import_invalidates_every_namespace_the_stream_wrote_into_and_nothing_else() {
        let tenant = TenantId::new("acme").unwrap();
        let ns_a = Scope::new("acme", "user-42", "aaa-namespace").unwrap();
        let ns_z = Scope::new("acme", "user-42", "zzz-namespace").unwrap();
        let untouched = Scope::new("acme", "user-42", "never-in-the-stream").unwrap();

        let source = engine();
        source
            .remember(RememberRequest::new(ns_a.clone(), "in namespace a"))
            .await
            .unwrap();
        source
            .remember(RememberRequest::new(ns_z.clone(), "in namespace z"))
            .await
            .unwrap();
        let ndjson = source
            .export_ndjson(&ScopeSelector {
                tenant: tenant.clone(),
                subject: None,
                namespace: None,
                include_audit: false,
            })
            .await
            .unwrap();

        let target = engine();
        for s in [&ns_a, &ns_z, &untouched] {
            target.cache.put_stats(s, sentinel()).await;
            assert!(
                target.cache.stats(s).await.is_some(),
                "premise: cache seeded for {s:?}"
            );
        }

        let report = target.import_ndjson(&ndjson, &tenant).await.unwrap();
        assert_eq!(report.items_imported, 2, "premise: both namespaces landed");

        assert!(
            target.cache.stats(&ns_a).await.is_none(),
            "import must invalidate every namespace it wrote into (a)"
        );
        assert!(
            target.cache.stats(&ns_z).await.is_none(),
            "import must invalidate every namespace it wrote into (z)"
        );
        assert!(
            target.cache.stats(&untouched).await.is_some(),
            "import must not invalidate a scope its stream never mentioned"
        );
    }
}

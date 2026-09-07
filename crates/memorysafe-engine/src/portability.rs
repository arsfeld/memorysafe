use crate::Engine;
use crate::error::EngineError;
use memorysafe_backend::{
    ExportRecord, ExportStream, ImportReport, ImportStream, ScopeSelector,
};
use memorysafe_core::{Protection, TenantId};

impl Engine {
    pub async fn export(&self, sel: &ScopeSelector) -> Result<ExportStream, EngineError> {
        Ok(self.backend.export(sel).await?)
    }

    /// Imported items are re-assessed rather than trusted. An import stream is
    /// caller-supplied JSON, and `MemoryItem`'s fields are public — so a stream
    /// can claim `sensitivity: "public"` for a body full of credentials and, if
    /// believed, that body would then satisfy a Public-clearance recall. And a
    /// stream can claim `protection: "pinned"` to forge an absolutely
    /// unevictable item that never went through `admit::decide` (which never
    /// produces `Pinned` on its own — see `memorysafe_policy::admit`; the
    /// only path to `Pinned` is an explicit `Engine::protect` call). `
    /// Backend::import` trusts neither field — its own doc is explicit that
    /// the backend layer stores `protection` and `sensitivity` exactly as the
    /// stream carries them, leaving the decision of whether a stream is
    /// trusted input to its caller. This is that caller: it recomputes the
    /// sensitivity the same way a write would and keeps whichever level is
    /// higher so an import can never lower a stored classification, and it
    /// downgrades a claimed `Protection::Pinned` to `Protection::Normal`.
    /// `Protection::Normal` and `Protection::Protected { until }` are left
    /// exactly as the stream carries them — both are time-bound or absent
    /// outcomes a genuine admission decision can already produce (and the
    /// legitimate re-import of one's own export, exercised by
    /// `ndjson_round_trips_through_a_fresh_engine`, must reproduce them
    /// exactly); only `Pinned`'s absolute, un-expiring guarantee is a claim an
    /// import must never be able to manufacture for itself.
    pub async fn import(
        &self,
        destination: &TenantId,
        stream: ImportStream,
    ) -> Result<ImportReport, EngineError> {
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
                    let detected = memorysafe_policy::sensitivity::assess(
                        &candidate,
                        &memorysafe_policy::BaselineConfig::default(),
                    )
                    .level;
                    item.sensitivity = item.sensitivity.max(detected);
                    if item.protection == Protection::Pinned {
                        item.protection = Protection::Normal;
                    }
                    ExportRecord::Item { item, vector }
                }
                other => other,
            })
            .collect();
        Ok(self.backend.import(destination, reassessed).await?)
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
        let mut stream: ImportStream = Vec::new();
        for (i, line) in ndjson.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let record: ExportRecord = serde_json::from_str(line).map_err(|e| {
                EngineError::Validation(format!("line {}: {e}", i + 1))
            })?;
            stream.push(record);
        }
        self.import(destination, stream).await
    }

    /// A human-readable rendering. This is what makes "your memory is yours"
    /// mean something a person can open, rather than a JSON blob.
    pub async fn export_markdown(&self, sel: &ScopeSelector) -> Result<String, EngineError> {
        let stream = self.export(sel).await?;
        let mut out = String::from("# MemorySafe export\n\n");

        let mut current_scope: Option<String> = None;
        for record in &stream {
            let ExportRecord::Item { item, .. } = record else {
                continue;
            };
            let scope_label = format!(
                "{} / {} / {}",
                item.scope.tenant, item.scope.subject, item.scope.namespace
            );
            if current_scope.as_deref() != Some(scope_label.as_str()) {
                out.push_str(&format!("## {scope_label}\n\n"));
                current_scope = Some(scope_label);
            }

            out.push_str(&format!("### {}\n\n", item.id));
            out.push_str(&format!("- **kind:** {}\n", item.kind));
            out.push_str(&format!("- **created:** {}\n", item.created_at));
            out.push_str(&format!("- **sensitivity:** {:?}\n", item.sensitivity));
            out.push_str(&format!("- **protection:** {:?}\n", item.protection));
            if !item.tags.is_empty() {
                out.push_str(&format!("- **tags:** {}\n", item.tags.join(", ")));
            }
            out.push_str(&format!("\n{}\n\n", item.body));
        }

        Ok(out)
    }
}

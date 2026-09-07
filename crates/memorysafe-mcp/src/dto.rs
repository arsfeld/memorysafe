//! Wire types. Flat, schema-bearing, and converted from the engine types by
//! total functions — a field added to `MemoryItem` that matters to a client is
//! a change here, not a silent omission.

use memorysafe_core::{
    Action, MemoryItem, OmittedItem, Protection, Reason, RecallMode, SelectedItem,
    SensitivityLevel, WorkingSet,
};
use memorysafe_engine::WriteOutcome;
use rmcp::ErrorData;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Round-trips through `serde` rather than a hand-written match, so the names
/// on the wire are exactly the names `memorysafe-core` serialises and cannot
/// drift from them.
pub fn parse_sensitivity(raw: &str) -> Result<SensitivityLevel, ErrorData> {
    serde_json::from_value(serde_json::Value::String(raw.to_owned())).map_err(|_| {
        ErrorData::invalid_params(
            format!("unknown sensitivity '{raw}'; expected one of public, internal, personal, sensitive, restricted"),
            None,
        )
    })
}

pub fn sensitivity_name(level: SensitivityLevel) -> String {
    match serde_json::to_value(level) {
        Ok(serde_json::Value::String(s)) => s,
        _ => unreachable!("SensitivityLevel serialises as a string"),
    }
}

pub fn parse_mode(raw: &str) -> Result<RecallMode, ErrorData> {
    serde_json::from_value(serde_json::Value::String(raw.to_owned())).map_err(|_| {
        ErrorData::invalid_params(
            format!("unknown mode '{raw}'; expected working_set or search"),
            None,
        )
    })
}

/// `Protection` serialises as a tagged object because `Protected` carries a
/// deadline. On the wire a client names the level and, for `protected`, the
/// deadline separately; the mapping is explicit and its names are asserted in
/// this module's tests.
pub fn protection_name(p: &Protection) -> &'static str {
    match p {
        Protection::Normal => "normal",
        Protection::Protected { .. } => "protected",
        Protection::Pinned => "pinned",
    }
}

/// `pinned` is absolute; `protected` is a window and must carry its deadline.
/// Defaulting the deadline would silently turn a time-boxed exemption into a
/// permanent one.
///
/// The five-branch decision table itself lives in `memorysafe_core::parse_protection`,
/// not here: it is pure logic about the `Protection` type, and the HTTP and
/// CLI adapters need the identical table. This function is the thin,
/// MCP-specific part — mapping `memorysafe_core::ProtectionParseError` into
/// `rmcp::ErrorData` — so a rename or a new branch in the shared table cannot
/// silently drift out of step with what this adapter reports.
pub fn parse_protection(level: &str, until: Option<i64>) -> Result<Protection, ErrorData> {
    memorysafe_core::parse_protection(level, until)
        .map_err(|e| ErrorData::invalid_params(e.to_string(), None))
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MemoryView {
    pub id: String,
    pub body: String,
    pub kind: String,
    pub tags: Vec<String>,
    pub sensitivity: String,
    pub protection: String,
    pub protected_until: Option<i64>,
    pub created_at: i64,
    pub occurred_at: Option<i64>,
    /// True while the item is invisible to vector search and reachable only by
    /// keyword or review. A client showing memories should say so.
    pub pending_embedding: bool,
}

impl From<&MemoryItem> for MemoryView {
    fn from(item: &MemoryItem) -> Self {
        // Destructured, not read by field access off `item` directly: a
        // struct LITERAL reading named fields off `&MemoryItem` compiles
        // fine and silently drops a field `MemoryItem` gains later, which is
        // exactly what this module's doc comment claims cannot happen. A
        // destructuring pattern with no `..` is exhaustive, so a new field on
        // `MemoryItem` fails this match to compile until it is named here —
        // even the fields this view does not carry (`scope`, `source`,
        // `attrs`, `ttl`) are bound, to `_`, to keep the pattern exhaustive.
        let MemoryItem {
            id,
            scope: _,
            body,
            kind,
            source: _,
            occurred_at,
            created_at,
            tags,
            attrs: _,
            sensitivity,
            ttl: _,
            protection,
            pending_embedding,
        } = item;
        Self {
            id: id.to_string(),
            body: body.clone(),
            kind: kind.clone(),
            tags: tags.clone(),
            sensitivity: sensitivity_name(*sensitivity),
            protection: protection_name(protection).to_owned(),
            protected_until: match protection {
                Protection::Protected { until } => Some(until.unix_timestamp()),
                _ => None,
            },
            created_at: created_at.unix_timestamp(),
            occurred_at: occurred_at.as_ref().map(|t| t.unix_timestamp()),
            pending_embedding: *pending_embedding,
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReasonView {
    pub code: String,
    pub detail: String,
}

impl From<&Reason> for ReasonView {
    fn from(r: &Reason) -> Self {
        Self {
            code: match serde_json::to_value(r.code) {
                Ok(serde_json::Value::String(s)) => s,
                _ => unreachable!("ReasonCode serialises as a string"),
            },
            detail: r.detail.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RecalledMemory {
    #[serde(flatten)]
    pub memory: MemoryView,
    pub relevance: f32,
    pub reason_code: String,
    pub reason_detail: String,
}

impl From<&SelectedItem> for RecalledMemory {
    fn from(s: &SelectedItem) -> Self {
        let reason = ReasonView::from(&s.reason);
        Self {
            memory: MemoryView::from(&s.item),
            relevance: s.relevance,
            reason_code: reason.code,
            reason_detail: reason.detail,
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OmittedView {
    pub id: String,
    pub reason_code: String,
    pub reason_detail: String,
}

impl From<&OmittedItem> for OmittedView {
    fn from(o: &OmittedItem) -> Self {
        let reason = ReasonView::from(&o.reason);
        Self {
            id: o.id.to_string(),
            reason_code: reason.code,
            reason_detail: reason.detail,
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RecallResult {
    pub items: Vec<RecalledMemory>,
    pub tokens_used: u32,
    /// A **sample** of what was considered and cut, with the reason, capped at
    /// `memorysafe_core::OMITTED_CAP`. Its length is not the number of
    /// omissions — read `omitted_total` for that.
    pub omitted: Vec<OmittedView>,
    /// How many items were omitted before the sample above was truncated.
    /// Carried through from `WorkingSet::omitted_total`, and carried
    /// deliberately: the caller this number exists for is the one tuning the
    /// recall budget, and dropping it at this boundary would leave an MCP
    /// client unable to tell fifty omissions from five thousand — which is the
    /// silent truncation the field was added to remove.
    pub omitted_total: usize,
    pub audit_id: String,
}

impl RecallResult {
    /// `audit_id` is `String`, not `Option<String>`: the engine guarantees a
    /// recall is audited before it returns, so a missing id here is a bug worth
    /// failing on rather than a shape a client has to handle.
    pub fn build(ws: &WorkingSet) -> Result<Self, ErrorData> {
        let audit_id = ws.audit_id.as_ref().ok_or_else(|| {
            ErrorData::internal_error("recall returned an unaudited working set", None)
        })?;
        Ok(Self {
            items: ws.items.iter().map(RecalledMemory::from).collect(),
            tokens_used: ws.tokens_used,
            omitted: ws.omitted.iter().map(OmittedView::from).collect(),
            omitted_total: ws.omitted_total,
            audit_id: audit_id.to_string(),
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RememberResult {
    pub item_id: Option<String>,
    /// `retain`, `merge`, or `reject`. All three are successful calls.
    pub action: String,
    pub protection: Option<String>,
    pub merged_into: Option<String>,
    pub reasons: Vec<ReasonView>,
    pub evicted: Vec<String>,
    pub audit_id: String,
}

impl From<&WriteOutcome> for RememberResult {
    fn from(out: &WriteOutcome) -> Self {
        let (action, protection) = match &out.action {
            Action::Retain { protection } => {
                ("retain", Some(protection_name(protection).to_owned()))
            }
            Action::Merge { .. } => ("merge", None),
            Action::Reject => ("reject", None),
        };
        Self {
            item_id: out.item_id.as_ref().map(|i| i.to_string()),
            action: action.to_owned(),
            protection,
            merged_into: out.merged_into.as_ref().map(|i| i.to_string()),
            reasons: out.reasons.iter().map(ReasonView::from).collect(),
            evicted: out.evicted.iter().map(|i| i.to_string()).collect(),
            audit_id: out.audit_id.to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RememberParams {
    /// The memory to store. One discrete fact, preference, event, procedure, or entity.
    pub body: String,
    /// Convention, not enforced: fact, preference, event, procedure, entity.
    pub kind: Option<String>,
    pub tags: Option<Vec<String>>,
    /// May only raise the level the detectors assign, never lower it.
    pub sensitivity_hint: Option<String>,
    /// When this memory happened, if different from when it was written.
    /// Unix seconds. Without it, this item's `occurred_at` is `None` on
    /// write, and `memory_recall`'s `occurred_after`/`occurred_before`
    /// bounds can never match it — an item with no `occurred_at` matches
    /// neither bound (`NULL >= x` is `NULL`, not true), not "matches
    /// everything."
    pub occurred_at: Option<i64>,
    pub ttl_seconds: Option<i64>,
    /// A retried write with the same key returns the original outcome.
    pub idempotency_key: Option<String>,
    /// Required over HTTP; over stdio it must match the configured subject.
    pub subject: Option<String>,
    /// Required over HTTP; over stdio it defaults to the server's namespace.
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RecallParams {
    pub query: Option<String>,
    /// `working_set` (default) composes under a budget; `search` returns a raw
    /// ranked list. Both are scope-filtered, sensitivity-capped, and audited.
    pub mode: Option<String>,
    pub max_tokens: Option<u32>,
    pub max_items: Option<usize>,
    pub tags_any: Option<Vec<String>>,
    pub kinds: Option<Vec<String>>,
    pub occurred_after: Option<i64>,
    pub occurred_before: Option<i64>,
    /// Items above this level are excluded in the backend query. Defaults to
    /// `internal`, matching `memorysafe_backend::query::HardFilters`'s own
    /// documented fail-closed default for this exact field — personal,
    /// sensitive, and restricted memories are excluded unless a caller names
    /// a wider ceiling explicitly.
    pub sensitivity_ceiling: Option<String>,
    pub subject: Option<String>,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ReviewParams {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub subject: Option<String>,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReviewedMemory {
    #[serde(flatten)]
    pub memory: MemoryView,
    /// The most recent recorded decision naming this item. `null` when the
    /// decision is older than the audit window this review looked at — the item
    /// is still governed, its reason is simply out of reach from here.
    pub reason_code: Option<String>,
    pub reason_detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReviewResult {
    pub items: Vec<ReviewedMemory>,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ForgetParams {
    pub ids: Option<Vec<String>>,
    pub tag: Option<String>,
    pub kind: Option<String>,
    pub subject: Option<String>,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ForgetResult {
    pub forgotten: Vec<String>,
    pub audit_id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProtectParams {
    pub id: String,
    /// `normal`, `protected`, or `pinned`. `protected` requires `until`.
    pub level: String,
    /// Unix seconds. Required for `protected`, rejected for the others.
    pub until: Option<i64>,
    pub subject: Option<String>,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ProtectResult {
    pub item_id: String,
    pub protection: String,
    pub protected_until: Option<i64>,
    pub audit_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_match_the_core_serde_names() {
        // All five levels, not a two-of-five spot check: the previous
        // version only pinned `Restricted` and `Personal`, so renaming
        // `Sensitive` (or `Public`, or `Internal`) would compile, run, and
        // pass every test in this module while quietly changing the wire
        // name a client sees. The length assertion closes the matching gap
        // in the other direction — a variant added to `SensitivityLevel`
        // without a corresponding row here.
        let expected = [
            (SensitivityLevel::Public, "public"),
            (SensitivityLevel::Internal, "internal"),
            (SensitivityLevel::Personal, "personal"),
            (SensitivityLevel::Sensitive, "sensitive"),
            (SensitivityLevel::Restricted, "restricted"),
        ];
        assert_eq!(
            expected.len(),
            SensitivityLevel::ALL.len(),
            "a SensitivityLevel variant was added or removed without updating this test"
        );
        for (level, name) in expected {
            assert_eq!(
                sensitivity_name(level),
                name,
                "{level:?} renders inconsistently"
            );
            assert_eq!(
                parse_sensitivity(name).unwrap(),
                level,
                "'{name}' did not parse back to {level:?}"
            );
        }
        assert!(
            parse_sensitivity("Personal").is_err(),
            "wire names are lowercase"
        );
        assert_eq!(parse_mode("search").unwrap(), RecallMode::Search);
        assert_eq!(parse_mode("working_set").unwrap(), RecallMode::WorkingSet);
    }

    #[test]
    fn remember_result_action_names_match_the_core_serde_names() {
        // `RememberResult::from`'s action string is a hand-written match, not
        // a derive — `ReasonCode`'s own doc warns these strings are a stored
        // wire format, and `Action` carries the same "renaming a variant is a
        // breaking change" property. Mirrors
        // `protection_names_survive_a_round_trip_through_core`: pin each
        // hand-written string against `Action`'s own serde tag, not just
        // against a hardcoded literal, so an `Action` rename is caught here
        // even if the hand-written match is edited to match it.
        let cases = [
            (
                Action::Retain {
                    protection: Protection::Normal,
                },
                "retain",
            ),
            (
                Action::Merge {
                    into: memorysafe_core::ItemId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap(),
                    strategy: memorysafe_core::MergeStrategy::AppendAndUnion,
                },
                "merge",
            ),
            (Action::Reject, "reject"),
        ];
        for (action, expected) in cases {
            let out = WriteOutcome {
                item_id: None,
                action: action.clone(),
                reasons: vec![],
                merged_into: None,
                evicted: vec![],
                audit_id: memorysafe_core::AuditId::new(),
            };
            let result = RememberResult::from(&out);
            assert_eq!(result.action, expected, "{action:?} renders inconsistently");

            let serialised = serde_json::to_value(&action).unwrap();
            assert_eq!(
                serialised["kind"], expected,
                "{action:?} disagrees with core's own serde tag"
            );
        }
    }

    #[test]
    fn protection_parsing_maps_the_core_error_into_error_data() {
        // The six-row decision table itself is tested exhaustively in
        // `memorysafe_core::item::tests` — this only pins that the MCP
        // wrapper actually delegates to it (rather than re-deriving the
        // table) and turns its structured error into `ErrorData::invalid_params`
        // with the same message text a caller would see from `memorysafe_core`
        // directly.
        assert_eq!(
            parse_protection("pinned", None).unwrap(),
            Protection::Pinned
        );
        assert_eq!(
            parse_protection("normal", None).unwrap(),
            Protection::Normal
        );

        let err = parse_protection("protected", None).unwrap_err();
        assert_eq!(
            err.message.as_ref(),
            "'protected' requires 'until'; use 'pinned' for permanent protection"
        );

        let err = parse_protection("pinned", Some(1)).unwrap_err();
        assert_eq!(
            err.message.as_ref(),
            "'until' is meaningless for level 'pinned'"
        );

        let err = parse_protection("locked", None).unwrap_err();
        assert_eq!(
            err.message.as_ref(),
            "unknown protection level 'locked'; expected normal, protected, or pinned"
        );

        let err = parse_protection("protected", Some(i64::MAX)).unwrap_err();
        assert_eq!(
            err.message.as_ref(),
            "'until' is not a valid Unix timestamp"
        );

        assert!(matches!(
            parse_protection("protected", Some(0)).unwrap(),
            Protection::Protected { until } if until == time::OffsetDateTime::UNIX_EPOCH
        ));
    }

    #[test]
    fn protection_names_survive_a_round_trip_through_core() {
        // If `Protection` gains a variant, this fails to compile rather than
        // silently rendering the new variant as something else.
        for p in [
            Protection::Normal,
            Protection::Pinned,
            Protection::Protected {
                until: time::OffsetDateTime::UNIX_EPOCH,
            },
        ] {
            let name = protection_name(&p);
            let serialised = serde_json::to_value(p).unwrap();
            assert_eq!(serialised["kind"], name, "{p:?} renders inconsistently");
        }
    }
}

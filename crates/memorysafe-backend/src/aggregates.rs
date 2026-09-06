//! Audit aggregates: counts, rates and score distributions grouped by policy
//! version, never identifying. The spec (§11, "Audit and retention") makes
//! them what keeps policy behaviour evaluable over time regardless of
//! retention profile, and `AuditRetention::aggregate` already defines a span
//! for them — but nothing produced or consumed one, and neither downstream
//! plan mentioned the concept.
//!
//! **What is in this module and what is not.** The contract: the key, the
//! histogram edges, and the read method on `Backend`. The storage table is
//! Task 19, the increments are Tasks 20 and 23, and retention enforcement is
//! Task 36. None of those belong here.
//!
//! # The key: tenant + policy version + event class + day bucket
//!
//! **No subject, and no namespace.** That is what makes an aggregate row
//! legitimately able to outlive `purge_subject`, and it is the single most
//! important sentence in this module.
//!
//! Write down why, because omitting it silently is not enough. A cascading
//! purge deletes items, vectors, audit rows, idempotency records **and**
//! capacity rows, all keyed by subject — so after it runs, nothing else in the
//! database retains the namespace. A namespace-keyed aggregate would therefore
//! be the *sole* surviving artifact naming it, and in a single-user deployment
//! a namespace is exactly the presence signal a purge exists to erase: "this
//! account had a `medical` namespace" survives the deletion of everything in
//! it. Both people who designed this reached for finer granularity out of
//! habit — it is what you write when you are thinking about querying the data
//! rather than about what survives a purge — so the next reader will reach for
//! it too, and with a good reason.
//!
//! **The same warning covers the day bucket.** Someone will want hourly
//! buckets for a latency investigation. Hourly is strictly worse on the
//! residual: a per-hour count over a single-subject tenant is a timeline of
//! when that person was active, which is a materially more identifying
//! artifact than a daily count of the same events. Neither the bucket nor the
//! key is a tuning knob.
//!
//! **The cost, stated so the trade is visible rather than discovered.** An
//! aggregate cannot answer "which namespace was misbehaving" — a real ops
//! question, and one that is answerable from detail rows for exactly as long
//! as they exist. Aggregates are for cross-version policy comparison, not for
//! incident triage; when triage needs namespace granularity it needs the
//! detail rows, and the retention profile is where that trade is configured.
//!
//! # Comparability over time is the entire purpose
//!
//! Two constraints follow from it, and both are about not silently changing
//! the meaning of a stored number.
//!
//! 1. **[`SCORE_HISTOGRAM_EDGES`] is fixed, named and versioned, and cannot
//!    change without a migration.** The argument against storing means applies
//!    one level up: if bucket edges change between releases, rows written
//!    before and after are silently incomparable and both sets look
//!    well-formed. There is no way to detect it from the data.
//! 2. **`ReasonCode` variant names become storage keys the moment the first
//!    aggregate row is written.** Before that a rename is a refactor; after it
//!    a rename orphans history. `memorysafe_core::ReasonCode` already
//!    documents that renaming an audit-stored variant is a data-compatibility
//!    break — this ties that break to *this commit*, not to a release
//!    boundary, because the first aggregate row can be written by any
//!    deployment running any build from here on.
//!
//! # The accepted residual
//!
//! A tenant that holds exactly one subject has an aggregate count that means
//! that subject's activity, and it survives their purge. This is stated
//! plainly rather than left to be rediscovered by whoever reads the schema
//! next.
//!
//! It is accepted because it is **out of scope for the operation**, not
//! because it is small. `purge_subject` erases a subject *within* a tenant;
//! what survives at tenant granularity is the account's own operational
//! history, which the account holder is not the data subject of in the
//! multi-subject case the tier is built for. Where tenant and subject collapse
//! into the same person, the erasure that deployment actually wants is tenant
//! deletion — and that is a different operation:
//!
//! - **Subject erasure is an API with a conformance test.** `purge_subject`,
//!   proven by `lifecycle::purge_subject_removes_everything_for_that_subject`.
//! - **Tenant erasure is an out-of-band operator action, and its mechanism is
//!   backend-specific.** On SQLite it is deleting the tenant's database file.
//!   On Postgres it is a schema drop, but only under the non-default
//!   `SchemaPerTenant` layout; under the default partitioned layout it is a
//!   partition-scoped delete executed under row-level security. No `Backend`
//!   method exposes it, deliberately.
//!
//! And the correlation between the residual and its remedy is **typical, not
//! guaranteed**: nothing prevents a single-tenant, single-subject Postgres
//! install: it is simply not the shape that tier is built for. Do not write
//! code that assumes a Postgres deployment has many subjects per tenant.

use memorysafe_core::{AuditEvent, PolicyId, Score, TenantId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// The version of [`SCORE_HISTOGRAM_EDGES`]. Stored on every aggregate row so
/// rows written under different edges are distinguishable rather than
/// silently comparable.
///
/// Bumping this is a migration, not an edit: every existing row keeps the
/// version it was written under, and a reader that mixes versions in one
/// series is reporting a number that means two different things.
pub const SCORE_HISTOGRAM_VERSION: u32 = 1;

/// Fixed bucket edges for every score distribution an aggregate stores.
///
/// Ten equal buckets over `Score`'s `[0, 1]` domain. The values are not the
/// interesting part — that they never change without a version bump is.
/// See the module doc: changing edges between releases makes rows before and
/// after silently incomparable, and both sets still look well-formed.
pub const SCORE_HISTOGRAM_EDGES: [f32; 11] =
    [0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0];

/// Number of buckets between the edges. Derived, never written out by hand.
pub const SCORE_HISTOGRAM_BUCKETS: usize = SCORE_HISTOGRAM_EDGES.len() - 1;

/// The bucket a score falls in: half-open `[edges[i], edges[i+1])`, except the
/// last bucket, which is closed so `Score::ONE` has somewhere to go.
///
/// The convention is part of the contract, not an implementation detail. Two
/// backends that disagreed about which bucket `0.1` belongs to would produce
/// histograms that differ by one count per boundary value while both looking
/// correct — the same class of silent incomparability the fixed edges exist to
/// prevent.
pub fn score_bucket(score: Score) -> usize {
    let v = score.get();
    let mut bucket = 0usize;
    while bucket + 1 < SCORE_HISTOGRAM_BUCKETS && v >= SCORE_HISTOGRAM_EDGES[bucket + 1] {
        bucket += 1;
    }
    bucket
}

/// The UTC day an event falls in, as whole days since the Unix epoch.
///
/// UTC, not local time: an aggregate keyed by a server's local day would move
/// rows between buckets when a deployment migrates region, and two backends
/// running in different zones would bucket the same event differently.
pub fn day_bucket(at: OffsetDateTime) -> i64 {
    at.unix_timestamp().div_euclid(86_400)
}

/// What an aggregate row is keyed by. There is no `subject` field and no
/// `namespace` field, and the module doc says at length why not — the absence
/// is the design, and a struct that cannot represent them is what keeps a
/// later "just for this one query" from being a one-line change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateKey {
    pub tenant: TenantId,
    /// The policy that produced the events being counted. Both name and
    /// version: "how did behaviour change when we shipped 1.4" is the question
    /// aggregates exist to answer.
    pub policy: PolicyId,
    /// The event class being counted, e.g. `Admitted` or `Rejected`. Its
    /// serialised snake_case name is the storage key.
    pub event: AuditEvent,
    /// Whole UTC days since the Unix epoch — see [`day_bucket`]. Not hours.
    pub day: i64,
}

/// One aggregate row: how many events of this class the policy produced on
/// this day, and how their scores were distributed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditAggregate {
    pub key: AggregateKey,
    /// Events matching the key. Rates are derived by the reader from counts
    /// across event classes, never stored — a stored rate is a stored mean,
    /// and means cannot be re-aggregated across days.
    pub count: u64,
    /// `Assessment::value` distribution, bucketed by [`score_bucket`].
    pub value_histogram: [u64; SCORE_HISTOGRAM_BUCKETS],
    /// `Assessment::fragility` distribution, bucketed the same way.
    pub fragility_histogram: [u64; SCORE_HISTOGRAM_BUCKETS],
    /// The [`SCORE_HISTOGRAM_VERSION`] in force when this row was written.
    /// Stored per row: a series that spans a bump must be splittable, and a
    /// reader that cannot see the boundary would silently splice two scales.
    pub histogram_version: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_core::Scope;

    #[test]
    fn histogram_edges_are_a_pinned_versioned_constant() {
        // A golden assertion, deliberately duplicating the constant. Bucket
        // edges cannot change without a migration (see the module doc), so a
        // change to them must not be possible as a silent edit — it has to
        // break a test whose message says why.
        assert_eq!(SCORE_HISTOGRAM_VERSION, 1);
        assert_eq!(
            SCORE_HISTOGRAM_EDGES,
            [0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0],
            "changing histogram edges makes rows written before and after \
             silently incomparable, and both sets still look well-formed. \
             This is a migration, not an edit: bump SCORE_HISTOGRAM_VERSION."
        );
        assert_eq!(SCORE_HISTOGRAM_BUCKETS, 10);

        // Edges must be strictly ascending and span exactly Score's domain,
        // or `score_bucket` is not a partition of `[0, 1]`.
        assert_eq!(SCORE_HISTOGRAM_EDGES[0], 0.0);
        assert_eq!(SCORE_HISTOGRAM_EDGES[SCORE_HISTOGRAM_BUCKETS], 1.0);
        for w in SCORE_HISTOGRAM_EDGES.windows(2) {
            assert!(w[1] > w[0], "edges must strictly ascend: {w:?}");
        }
    }

    #[test]
    fn score_buckets_are_half_open_with_a_closed_top() {
        // The boundary convention is the part two backends could disagree
        // about while both looking correct, so it is the part asserted.
        assert_eq!(score_bucket(Score::ZERO), 0);
        assert_eq!(score_bucket(Score::clamped(0.099)), 0);
        assert_eq!(
            score_bucket(Score::clamped(0.1)),
            1,
            "an edge value belongs to the bucket it opens, not the one it closes"
        );
        assert_eq!(score_bucket(Score::clamped(0.55)), 5);
        assert_eq!(score_bucket(Score::clamped(0.9)), 9);
        assert_eq!(
            score_bucket(Score::ONE),
            SCORE_HISTOGRAM_BUCKETS - 1,
            "the top bucket is closed so Score::ONE has somewhere to go"
        );
        // Every representable score lands in a real bucket.
        for i in 0..=1000 {
            let s = Score::clamped(i as f32 / 1000.0);
            assert!(score_bucket(s) < SCORE_HISTOGRAM_BUCKETS);
        }
    }

    #[test]
    fn day_buckets_are_utc_days_and_do_not_round_toward_zero() {
        assert_eq!(day_bucket(OffsetDateTime::UNIX_EPOCH), 0);
        assert_eq!(
            day_bucket(OffsetDateTime::from_unix_timestamp(86_399).unwrap()),
            0
        );
        assert_eq!(
            day_bucket(OffsetDateTime::from_unix_timestamp(86_400).unwrap()),
            1
        );
        // `div_euclid`, not `/`: truncating division would put every instant
        // in the last 24 hours before the epoch into day 0 alongside the day
        // after it, merging two days into one bucket.
        assert_eq!(
            day_bucket(OffsetDateTime::from_unix_timestamp(-1).unwrap()),
            -1,
            "pre-epoch instants must not collapse into day 0"
        );
    }

    #[test]
    fn an_aggregate_key_cannot_name_a_subject_or_a_namespace() {
        // The absence is the design (see the module doc), so it is asserted
        // rather than left to the struct definition. A field added later —
        // "just for this one query" — makes an aggregate row the sole
        // surviving artifact naming a purged subject's namespace, and this
        // test is where that gets caught.
        let scope = Scope::new("acme", "user-42", "medical").unwrap();
        let key = AggregateKey {
            tenant: scope.tenant.clone(),
            policy: PolicyId::new("baseline", "1.0.0"),
            event: AuditEvent::Admitted,
            day: day_bucket(OffsetDateTime::UNIX_EPOCH),
        };
        let json = serde_json::to_string(&key).unwrap();
        assert!(
            !json.contains("user-42"),
            "an aggregate key named a subject: {json}"
        );
        assert!(
            !json.contains("medical"),
            "an aggregate key named a namespace: {json}"
        );
        assert!(
            json.contains("acme"),
            "the tenant is part of the key: {json}"
        );
        assert!(
            json.contains("admitted"),
            "the event class's snake_case name is the storage key: {json}"
        );
    }

    #[test]
    fn an_aggregate_row_carries_the_histogram_version_it_was_written_under() {
        // Per row, not per database: a series spanning a bump has to be
        // splittable, and a reader that cannot see the boundary splices two
        // scales into one line on a chart.
        let row = AuditAggregate {
            key: AggregateKey {
                tenant: TenantId::new("acme").unwrap(),
                policy: PolicyId::new("baseline", "1.0.0"),
                event: AuditEvent::Rejected,
                day: 19_000,
            },
            count: 3,
            value_histogram: [0; SCORE_HISTOGRAM_BUCKETS],
            fragility_histogram: [0; SCORE_HISTOGRAM_BUCKETS],
            histogram_version: SCORE_HISTOGRAM_VERSION,
        };
        let back: AuditAggregate =
            serde_json::from_str(&serde_json::to_string(&row).unwrap()).unwrap();
        assert_eq!(back, row, "an aggregate row did not survive a round trip");
        assert_eq!(back.histogram_version, SCORE_HISTOGRAM_VERSION);
        assert_eq!(back.value_histogram.len(), SCORE_HISTOGRAM_BUCKETS);
    }
}

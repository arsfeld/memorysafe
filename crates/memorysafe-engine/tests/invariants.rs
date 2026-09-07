use memorysafe_backend::ScopeSelector;
use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{
    AuditFilter, Budget, Protection, RecallBudget, RecallMode, RecallRequest, Scope,
    SensitivityLevel, TenantId,
};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{Engine, EngineConfig, RememberRequest};
use memorysafe_policy::BaselinePolicy;
use proptest::prelude::*;
use std::sync::Arc;

fn engine() -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    Engine::new(EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    ))
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Bodies are distinct enough that the redundancy check does not collapse them
/// all into merges, which would make the capacity properties vacuous.
fn bodies() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(0u32..10_000, 1..40).prop_map(|ns| {
        ns.into_iter()
            .enumerate()
            .map(|(i, n)| format!("memory {i} concerning subject {n} and topic {n}"))
            .collect()
    })
}

// Every property below carries an inline premise assertion, in the shape the
// sixth invariant's two tests use ("the premise: ..."). They are guards, not
// restatements of the invariant: each one fails if the case reached its
// assertions without exercising the thing the invariant is about.
//
// The reason they are here rather than in a measurement someone once took:
// every one of the five has a live vacuity path. If all writes fail,
// Invariant 1's `stored.len() <= max` and `used_items == 0` both hold. If no
// eviction pressure occurs, Invariant 2's `any(|i| i.id == pinned)` holds
// trivially. If the corpus is empty, Invariant 5 compares two empty vectors
// and Invariant 3's loop runs zero times. `bodies()`' own doc comment names
// the concrete rot path: change the strategy, or `duplicate_threshold`, so
// generated bodies collapse into merges, and three of the five go quietly
// vacuous with nothing to report it.
//
// **A premise assertion must hold for every input the strategy can draw, not
// merely for typical ones** — a flaky invariant suite is worse than a vacuous
// one. Two of the five are therefore conditional, and each says on what:
// `bodies()` draws `1..40`, so a single-body draw creates no eviction
// pressure against Invariant 2's budget of 2, and Invariant 3's `Public`
// ceiling admits nothing by construction (see that test's own doc). The
// conditions are written as implications so the guard still fails when the
// condition holds and the premise does not.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Invariant 1: capacity is never exceeded, whatever the write sequence.
    #[test]
    fn capacity_is_never_exceeded(bodies in bodies(), max in 1usize..12) {
        runtime().block_on(async {
            let e = engine();
            e.set_budget(&scope(), Budget { max_items: Some(max as u64), max_bytes: None })
                .await
                .unwrap();

            for b in &bodies {
                let _ = e.remember(RememberRequest::new(scope(), b)).await;
            }

            let stored = e.review(&scope(), &memorysafe_backend::Page { offset: 0, limit: 1000 })
                .await
                .unwrap();
            // Unconditional: `bodies()` draws at least one body, and the
            // first write into an empty scope cannot fail — the corpus has
            // no neighbour to be redundant against, and `max >= 1` leaves
            // room for it. If nothing is stored, every assertion below holds
            // for the wrong reason.
            prop_assert!(
                !stored.is_empty(),
                "the premise: no write survived, so `len() <= {max}` and \
                 `used_items == 0` would both hold vacuously"
            );
            prop_assert!(
                stored.len() <= max,
                "budget {max} exceeded: {} items stored",
                stored.len()
            );

            let state = e.capacity_state(&scope()).await.unwrap();
            prop_assert_eq!(state.used_items, stored.len() as u64, "accounting drifted");
            Ok(())
        })?;
    }

    /// Invariant 2: a pinned item survives any amount of pressure.
    #[test]
    fn pinned_items_are_never_evicted(bodies in bodies()) {
        runtime().block_on(async {
            let e = engine();
            let pinned = e
                .remember(RememberRequest::new(scope(), "the pinned memory that must survive"))
                .await
                .unwrap()
                .item_id
                .unwrap();
            e.protect(&scope(), &pinned, Protection::Pinned).await.unwrap();

            e.set_budget(&scope(), Budget { max_items: Some(2), max_bytes: None })
                .await
                .unwrap();

            for b in &bodies {
                let _ = e.remember(RememberRequest::new(scope(), b)).await;
            }
            let _ = e.maintain(&scope(), None).await;

            let stored = e.review(&scope(), &memorysafe_backend::Page { offset: 0, limit: 1000 })
                .await
                .unwrap();

            // The premise, and the sharp case of the whole exercise: this
            // invariant is trivially true if nothing ever pressured the pin.
            //
            // Read back from the audit trail rather than from the write
            // loop's outcomes, so the mandated loop above stays exactly as
            // the plan wrote it. `Decision` is stored on the audit record, so
            // `retained` counts every admission (the pinned item's own
            // included — `protect` files a record with no `decision` and is
            // correctly not counted) and `planned_evictions` counts every
            // eviction a decision called for.
            //
            // **Conditional, and this is the trap it avoids.** `bodies()`
            // draws `1..40`. A one-body draw puts the pin plus one item
            // against a budget of 2, which fits, so no eviction is required
            // and a bare `planned_evictions > 0` would fail legitimately —
            // a flaky suite, which is worse than a vacuous one. Written as an
            // implication instead: eviction is required exactly when the
            // admissions exceed the budget, and the guard fires when that
            // holds and no eviction happened. I chose the implication over
            // constraining `bodies()` to `2..40` because the strategy is
            // plan-mandated and shared by four other properties, and because
            // the small draws are worth keeping — they are the ones that
            // exercise admission below the budget.
            let trail = e
                .audit(&scope(), &AuditFilter { limit: 100_000, ..Default::default() })
                .await
                .unwrap();
            let decisions: Vec<_> = trail.iter().filter_map(|r| r.decision.as_ref()).collect();
            let retained = decisions
                .iter()
                .filter(|d| matches!(d.action, memorysafe_core::Action::Retain { .. }))
                .count();
            let planned_evictions: usize =
                decisions.iter().map(|d| d.evictions.len()).sum();
            prop_assert!(
                retained <= 2 || planned_evictions > 0,
                "the premise: {retained} admissions against a budget of 2 must have \
                 forced an eviction, but the trail records none — this case cannot \
                 show a pin surviving pressure"
            );

            prop_assert!(
                stored.iter().any(|i| i.id == pinned),
                "a pinned item was evicted"
            );
            Ok(())
        })?;
    }

    /// Invariant 3: nothing above the caller's ceiling is ever returned.
    ///
    /// **The `Public` arm is one-sided, and the `is_empty` assertion below is
    /// a tripwire on that, not a leak detector.** `BaselinePolicy` cannot
    /// classify anything `Public` — for *any* input, not just the bodies
    /// generated here. `sensitivity::assess` opens with `let mut detected =
    /// SensitivityLevel::Internal` and every subsequent write to it is a
    /// `.max(..)`, so the value only ever rises; the closing
    /// `detected.raised_by(cand.sensitivity_hint)` is itself a `max`, and
    /// `write.rs` applies `raised_by` a second time. Nothing in that chain
    /// can lower a level, so the floor is `Internal` by construction.
    /// `sensitivity::tests::ordinary_text_is_internal` pins the floor itself.
    ///
    /// So when `ceiling` is drawn as `0`, a correct recall returns nothing and
    /// the per-item loop below executes zero times. That arm therefore
    /// produces no *positive* evidence — it never shows an item being
    /// returned and correctly admitted — and no assertion can give it any,
    /// because an item at or below a `Public` ceiling cannot exist to be
    /// returned. That is a true statement about the domain, not a gap to
    /// close.
    ///
    /// What it is **not** is unfalsifiable. A leak at ceiling `0` already
    /// fails through the loop: any item that comes back is at least
    /// `Internal`, and `Internal <= Public` is false. Verified rather than
    /// assumed — an isolating mutation that widens the ceiling *only* when
    /// the request asks for `Public`
    /// (`req.sensitivity_ceiling.max(SensitivityLevel::Internal)` in
    /// `read.rs`) is killed by this property as it stands, three runs out of
    /// three, reporting `"Internal leaked past a Public ceiling in
    /// WorkingSet mode"`.
    ///
    /// The `is_empty` assertion earns its place for a different reason: it
    /// pins the structural claim above. **Its unique coverage is narrower
    /// than "any change that makes `BaselinePolicy` emit `Public`", and the
    /// narrower statement is the true one.** A change that lowers the floor
    /// wholesale — `let mut detected = SensitivityLevel::Public` — is caught
    /// first by `memorysafe_policy::sensitivity::tests::ordinary_text_is_internal`
    /// and by `import_uses_the_baseline_floor_not_a_deployments_tightened_config`,
    /// measured: workspace-wide with `--all-features`, that mutation fails
    /// three tests, of which this is only one.
    ///
    /// What nothing else catches is a **new detector arm that emits `Public`
    /// for some inputs while leaving ordinary text at `Internal`** — the
    /// realistic shape of the change, since a deliberate "this content is
    /// public" classifier would be conditional, not a lowered floor. Measured
    /// with exactly that mutation (`Public` when the body contains a token
    /// the generated corpus carries and `ordinary_text_is_internal`'s fixture
    /// does not): workspace-wide with `--all-features`, **1 failed, 546
    /// passed**, and the one failure is this property.
    ///
    /// **That failure is the signal to replace this special case with the
    /// general assertion**, not to delete it: once `Public` items can exist,
    /// the arm should assert what every other ceiling asserts, and the loop
    /// below already says it.
    #[test]
    fn the_sensitivity_ceiling_is_never_violated(bodies in bodies(), ceiling in 0i64..5) {
        runtime().block_on(async {
            let e = engine();
            let level = SensitivityLevel::from_ordinal(ceiling).unwrap();

            for (i, b) in bodies.iter().enumerate() {
                let mut r = RememberRequest::new(scope(), b);
                // Force a spread of sensitivity levels via caller hints.
                r.sensitivity_hint = SensitivityLevel::from_ordinal((i % 5) as i64);
                let _ = e.remember(r).await;
            }

            for mode in [RecallMode::WorkingSet, RecallMode::Search] {
                let ws = e.recall(RecallRequest {
                    scope: scope(),
                    query: Some("memory concerning subject".into()),
                    tags_any: vec![],
                    kinds: vec![],
                    occurred_after: None,
                    occurred_before: None,
                    mode,
                    budget: RecallBudget { max_tokens: Some(8000), max_items: Some(50) },
                    sensitivity_ceiling: level,
                })
                .await
                .unwrap();

                // The premise. Conditional on the ceiling, because the
                // `Public` arm returns nothing by construction — see this
                // test's doc comment. At every other ceiling the loop below
                // must actually iterate: `bodies()` draws at least one body,
                // its hint is `i % 5 == 0` so its resolved level is the
                // `Internal` floor, and an `Internal` item satisfies any
                // ceiling from `Internal` up. A zero-iteration loop at those
                // ceilings means retrieval returned nothing and the
                // assertion checked nothing.
                prop_assert!(
                    level == SensitivityLevel::Public || !ws.items.is_empty(),
                    "the premise: a {:?} ceiling in {:?} mode returned nothing, so the \
                     leak check below never ran",
                    level, mode
                );

                for s in &ws.items {
                    prop_assert!(
                        s.item.sensitivity <= level,
                        "{:?} leaked past a {:?} ceiling in {:?} mode",
                        s.item.sensitivity, level, mode
                    );
                }

                // The tripwire on the `Public` arm's premise; see this test's
                // doc comment for why it is not a second leak check. A
                // failure here means `BaselinePolicy` has started emitting
                // `Public`, and the fix is to delete this branch so the arm
                // asserts what the loop above already asserts for every
                // other ceiling.
                if level == SensitivityLevel::Public {
                    prop_assert!(
                        ws.items.is_empty(),
                        "a Public ceiling returned {} items in {:?} mode; \
                         BaselinePolicy's detected level is floored at Internal \
                         and only ever raised, so nothing should satisfy it",
                        ws.items.len(), mode
                    );
                }
            }
            Ok(())
        })?;
    }

    /// Invariant 4: one audit record per mutation, never more, never fewer.
    #[test]
    fn every_mutation_has_exactly_one_audit_record(bodies in bodies()) {
        runtime().block_on(async {
            let e = engine();
            let mut mutations = 0usize;

            for b in &bodies {
                if e.remember(RememberRequest::new(scope(), b)).await.is_ok() {
                    mutations += 1;
                }
            }

            let audit = e.audit(&scope(), &AuditFilter { limit: 100_000, ..Default::default() })
                .await
                .unwrap();
            // Unconditional, for the same reason as Invariant 1: at least one
            // body is drawn and the first write into an empty scope cannot
            // fail. `0 == 0` would satisfy the equality below.
            prop_assert!(
                mutations > 0,
                "the premise: no write succeeded, so the count below compares 0 with 0"
            );
            prop_assert_eq!(
                audit.len(), mutations,
                "{} mutations produced {} audit records",
                mutations, audit.len()
            );

            // And no body ever reached the trail. Its own premise: the trail
            // must actually carry item data, or "no body reached it" is true
            // because nothing reached it. An admitted write files an
            // `ItemRef`, and the first write is always an admission.
            prop_assert!(
                audit.iter().any(|r| !r.items.is_empty()),
                "the premise: no audit record references an item, so the leak check \
                 below has nothing to find a body in"
            );
            let json = serde_json::to_string(&audit).unwrap();
            prop_assert!(!json.contains("concerning subject"), "audit leaked a body");
            Ok(())
        })?;
    }

    /// Invariant 5: export then import reproduces the corpus exactly.
    #[test]
    fn export_import_round_trips_exactly(bodies in bodies()) {
        runtime().block_on(async {
            let source = engine();
            for b in &bodies {
                let _ = source.remember(RememberRequest::new(scope(), b)).await;
            }

            let selector = ScopeSelector {
                tenant: TenantId::new("acme").unwrap(),
                subject: None,
                namespace: None,
                include_audit: false,
            };
            let ndjson = source.export_ndjson(&selector).await.unwrap();

            let target = engine();
            target.import_ndjson(&ndjson, &selector.tenant).await.unwrap();

            let page = memorysafe_backend::Page { offset: 0, limit: 1000 };
            let mut before = source.review(&scope(), &page).await.unwrap();
            let mut after = target.review(&scope(), &page).await.unwrap();
            before.sort_by(|a, b| a.id.cmp(&b.id));
            after.sort_by(|a, b| a.id.cmp(&b.id));

            // Unconditional: at least one body is drawn and the first write
            // into an empty scope cannot fail. Without this, an export that
            // emitted nothing and an import that stored nothing compare equal
            // — two empty vectors — and the round trip "succeeds" having
            // moved no data at all.
            prop_assert!(
                !before.is_empty(),
                "the premise: the source corpus is empty, so the comparison below \
                 is between two empty vectors"
            );
            prop_assert_eq!(before, after, "the round trip lost or altered items");
            Ok(())
        })?;
    }
}

/// Invariant 4's missing half, pinned deterministically rather than left to
/// the generator.
///
/// `every_mutation_has_exactly_one_audit_record` counts every `Ok` write as
/// one mutation and demands exactly that many audit rows. Its whole weight
/// therefore rests on a *rejected* write still filing exactly one record —
/// a rejection is a successful governance outcome, not an error, so it
/// returns `Ok` and increments the counter while storing no item. And that
/// case never happens under `bodies()`: every generated body carries its own
/// index, so no two are near-duplicates, and the property was measured
/// producing 528 retains, 0 merges and 0 rejects across 24 cases. The
/// counter's load-bearing case was untested by the property that depends on
/// it.
///
/// Writing one body twice reaches it: `DeterministicEmbedder` embeds
/// identical text identically, cosine similarity is 1.0, and
/// `BaselineConfig::classify` reads anything at or above
/// `duplicate_threshold` (0.98) as `Verdict::NearDuplicate` and rejects. The
/// assertions below pin all three facts the property needs and cannot see:
/// the second write really is rejected, it stores nothing, and it still
/// leaves exactly one audit row behind.
#[tokio::test]
async fn a_rejected_write_still_files_exactly_one_audit_record() {
    let e = engine();
    let body = "exactly the same body written twice over";

    let first = e
        .remember(RememberRequest::new(scope(), body))
        .await
        .unwrap();
    let second = e
        .remember(RememberRequest::new(scope(), body))
        .await
        .unwrap();

    assert!(
        matches!(first.action, memorysafe_core::Action::Retain { .. }),
        "the premise: the first write must be admitted, got {:?}",
        first.action
    );
    assert_eq!(
        second.action,
        memorysafe_core::Action::Reject,
        "the premise: a re-written identical body must be rejected as a \
         near-duplicate, or this test exercises the same path the property does"
    );
    assert!(second.item_id.is_none(), "a rejected write stored an item");

    let page = memorysafe_backend::Page {
        offset: 0,
        limit: 1000,
    };
    assert_eq!(
        e.review(&scope(), &page).await.unwrap().len(),
        1,
        "a rejected write stored a second item"
    );

    let audit = e
        .audit(
            &scope(),
            &AuditFilter {
                limit: 100_000,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        audit.len(),
        2,
        "two writes, one admitted and one rejected, must leave exactly two \
         audit rows; got {audit:#?}"
    );
    assert_eq!(
        audit
            .iter()
            .filter(|r| r.event == memorysafe_core::AuditEvent::Rejected)
            .count(),
        1,
        "the rejected write filed no audit record of its own"
    );
}

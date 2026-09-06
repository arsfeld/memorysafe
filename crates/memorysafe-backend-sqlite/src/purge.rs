use crate::tenant::SqlResultExt;
use memorysafe_backend::{BackendError, PurgeReport};
use memorysafe_core::{AuditRecord, PurgeCascade, SubjectId};
use rusqlite::{Connection, params};

/// Right-to-delete for one subject. A first-class operation rather than a
/// scan-and-delete loop: everything for the subject goes in one transaction
/// across every namespace it owns.
///
/// `cascade` decides the audit detail and nothing else; `audit` — the
/// caller's `SubjectPurged` record — is inserted either way, after the
/// deletes and inside this same transaction. See `Backend::purge_subject` for
/// why the order and the single transaction are both load-bearing. Note the
/// `audit_aggregates` table — which the schema task now actually creates — is
/// absent from every statement below, deliberately:
/// `lifecycle::audit_aggregates_survive_a_cascading_purge` fails the moment it
/// joins the sweep.
pub fn subject(
    conn: &mut Connection,
    subject: &SubjectId,
    cascade: PurgeCascade,
    audit: &AuditRecord,
) -> Result<PurgeReport, BackendError> {
    let tx = conn
        .transaction()
        .map_err(|e| crate::tenant::storage_error(e, false))?;
    let s = subject.as_str();

    // Counted before anything is written. `audit_rows_removed +
    // audit_rows_preserved` must equal the rows the subject held immediately
    // before this call, and the record inserted at the end belongs to neither
    // term — counting after the insert would put it in `preserved` and break
    // the equation `PurgeReport` states.
    let existing_audit = tx
        .query_row(
            "SELECT count(*) FROM audit WHERE subject = ?1",
            params![s],
            |r| r.get::<_, i64>(0),
        )
        .sql()? as u64;

    let vectors_removed = tx
        .execute("DELETE FROM vectors WHERE subject = ?1", params![s])
        .sql()? as u64;
    let items_removed = tx
        .execute("DELETE FROM items WHERE subject = ?1", params![s])
        .sql()? as u64;
    let (audit_rows_removed, audit_rows_preserved) = match cascade {
        PurgeCascade::Cascade => (
            tx.execute("DELETE FROM audit WHERE subject = ?1", params![s])
                .sql()? as u64,
            0,
        ),
        PurgeCascade::Preserve => (0, existing_audit),
    };
    tx.execute("DELETE FROM idempotency WHERE subject = ?1", params![s])
        .sql()?;
    tx.execute("DELETE FROM capacity WHERE subject = ?1", params![s])
        .sql()?;

    // Delete first, insert second — under `Cascade` the sweep above would
    // otherwise delete this very row, and the purge would eat its own record.
    // `audit::insert` writes `audit.id` verbatim: the echo rule on `Backend`.
    crate::audit::insert(&tx, audit)?;
    // And it increments, like every audit row — see the write rule in the
    // items-and-audit task. After a cascading purge this row's aggregate is
    // the only remaining evidence, at tenant granularity, that an erasure
    // happened that day; the detail row it summarises may itself be swept by
    // a later retention pass.
    crate::aggregates::increment(&tx, audit)?;

    tx.commit()
        .map_err(|e| crate::tenant::storage_error(e, false))?;

    Ok(PurgeReport {
        items_removed,
        vectors_removed,
        audit_rows_removed,
        audit_rows_preserved,
    })
}

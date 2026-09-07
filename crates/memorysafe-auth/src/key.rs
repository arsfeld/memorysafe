use crate::AuthError;
use base64::Engine as _;
use memorysafe_core::TenantId;
use serde::{Deserialize, Serialize};

/// Presented form: `msk_<26-char ULID id>_<base64url secret>`.
pub const KEY_PREFIX: &str = "msk";
const SECRET_BYTES: usize = 32;

/// What is written to configuration. Holds a hash, never a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    pub id: String,
    pub tenant: TenantId,
    /// BLAKE3 of the whole presented key, hex encoded.
    pub hash: String,
    pub label: String,
    #[serde(default)]
    pub disabled: bool,
}

/// The only moment the secret exists. Returned once, then unrecoverable.
#[derive(Debug, Clone)]
pub struct GeneratedKey {
    pub secret: String,
    pub record: ApiKeyRecord,
}

pub fn generate(tenant: TenantId, label: &str) -> Result<GeneratedKey, AuthError> {
    let id = ulid::Ulid::generate().to_string();
    let mut bytes = [0u8; SECRET_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| AuthError::Rng)?;
    let secret_part = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let secret = format!("{KEY_PREFIX}_{id}_{secret_part}");

    Ok(GeneratedKey {
        record: ApiKeyRecord {
            id,
            tenant,
            hash: hash_presented(&secret),
            label: label.to_owned(),
            disabled: false,
        },
        secret,
    })
}

pub(crate) fn hash_presented(presented: &str) -> String {
    blake3::hash(presented.as_bytes()).to_hex().to_string()
}

/// Splits a presented key into its id, without validating the secret. The id is
/// public by construction — it is how the store finds one record instead of
/// hashing against all of them.
pub(crate) fn parse_presented(presented: &str) -> Result<&str, AuthError> {
    let rest = presented
        .strip_prefix(KEY_PREFIX)
        .ok_or(AuthError::Malformed)?;
    let rest = rest.strip_prefix('_').ok_or(AuthError::Malformed)?;
    let (id, secret) = rest.split_once('_').ok_or(AuthError::Malformed)?;
    if id.len() != 26 || secret.len() < 40 {
        return Err(AuthError::Malformed);
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_core::TenantId;

    #[test]
    fn a_generated_key_carries_its_id_in_the_clear_and_its_secret_only_once() {
        let tenant = TenantId::new("acme").unwrap();
        let g = generate(tenant.clone(), "ci runner").expect("generate");

        let (prefix, rest) = g.secret.split_once('_').expect("prefixed");
        assert_eq!(prefix, KEY_PREFIX);
        let (id, secret) = rest.split_once('_').expect("id then secret");
        assert_eq!(
            id, g.record.id,
            "the id must be readable without the secret"
        );
        assert_eq!(id.len(), 26, "a ULID id");
        assert!(secret.len() >= 40, "at least 240 bits of base64url");

        assert_eq!(g.record.tenant, tenant);
        assert_eq!(g.record.label, "ci runner");
        assert!(!g.record.disabled);
    }

    #[test]
    fn the_record_never_contains_the_secret() {
        // The record is what gets written to msafe.toml. If the secret survives
        // serialization, every operator's config file is a credential store in
        // plaintext.
        //
        // This used to take its needle from `g.secret.rsplit('_').next()`. The
        // base64url alphabet (URL_SAFE_NO_PAD) contains `_`, so that needle was
        // whatever followed the *last* underscore anywhere in the 43-char
        // body -- always a SUFFIX of the body, hence a suffix of the whole
        // secret. That made the old check *more* sensitive than the one
        // below, not less: any JSON containing the whole secret necessarily
        // contains that suffix too, so the old assertion fired in a strict
        // superset of the cases this one does. (Correction of `66c7b41`
        // [formerly `ec22ff0`; this branch's author identity was rewritten
        // after that commit landed, giving every commit on it a new SHA --
        // see the history-rewrite note at the end of this branch's commit
        // log], which called this replacement "strictly stronger" --
        // backwards; see that commit's follow-up for the measurement.) What
        // was wrong with the old needle was never its sensitivity -- it was that the
        // needle's length and position were unspecifiable in advance. On the
        // days the body's last `_` fell near the end, the needle was one or
        // two characters, too short to mean anything, and matched the
        // *record* by coincidence: a false failure, not a caught leak, and
        // indistinguishable from one by reading the test's own name. The
        // defect was specifiability and false positives, not weak detection.
        //
        // The assertions below are deterministic and well-defined -- not
        // "stronger" than the old needle in the case where nothing leaks --
        // and the sliding-window assertion at the end recovers partial-leak
        // sensitivity on purpose, rather than by accident of where `_` lands.
        let g = generate(TenantId::new("acme").unwrap(), "ci").unwrap();
        let json = serde_json::to_string(&g.record).unwrap();
        assert!(
            !json.contains(&g.secret),
            "the record serialised the secret"
        );

        // A real leak might carry only the random body, not the whole
        // "msk_<id>_<body>" string. Split off the two known prefix segments by
        // position (`splitn`, keep the third field) rather than by searching
        // for a delimiter that also occurs inside the body -- that search is
        // exactly the bug above. This gives an independent needle that can
        // neither flake nor pass hollowly.
        let body = g
            .secret
            .splitn(3, '_')
            .nth(2)
            .expect("secret is prefix_id_body");
        assert!(
            !json.contains(body),
            "the record serialised the secret's random body"
        );

        // Neither assertion above catches a leak of a *fragment* of the body:
        // `&body[..20]` would pass both, exactly the gap the brief named --
        // "a leak of the secret's prefix or middle would not be caught at
        // all" -- and that gap survived unfixed until this commit. Slide a
        // fixed-size window across the body and require that no window
        // appears in the record. The window size is fixed, not derived from
        // where any delimiter falls, so this is deterministic; and it is
        // genuinely more sensitive than both assertions above, recovering on
        // purpose the kind of partial-leak detection the old `rsplit` needle
        // only ever had by accident.
        const WINDOW: usize = 16;
        for start in 0..=body.len().saturating_sub(WINDOW) {
            let fragment = &body[start..start + WINDOW];
            assert!(
                !json.contains(fragment),
                "the record serialised a fragment of the secret's random body: {fragment:?}"
            );
        }
    }

    #[test]
    fn two_generated_keys_never_collide() {
        let a = generate(TenantId::new("acme").unwrap(), "a").unwrap();
        let b = generate(TenantId::new("acme").unwrap(), "b").unwrap();
        assert_ne!(a.record.id, b.record.id);
        assert_ne!(a.record.hash, b.record.hash);
        assert_ne!(a.secret, b.secret);
    }

    #[test]
    fn a_malformed_presented_key_is_rejected_before_any_lookup() {
        for bad in [
            "",
            "nope",
            "msk_short",
            "xxx_01ARZ3NDEKTSV4RRFFQ69G5FAV_abc",
            "msk__abc",
        ] {
            assert!(
                matches!(parse_presented(bad), Err(AuthError::Malformed)),
                "{bad} was accepted"
            );
        }
    }
}

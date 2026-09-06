use crate::config::BaselineConfig;
use memorysafe_core::{
    Candidate, Score, SensitivityAssessment, SensitivityCategory, SensitivityLevel,
};

/// Pattern and lexicon detectors for the baseline policy. Crude and
/// English-only — that is stated plainly here rather than hidden, and it is
/// one of the places the proprietary scorer earns its price. Lexicons are
/// lowercase substrings matched against the lowercased body.
const CREDENTIAL_MARKERS: &[&str] = &[
    "password:",
    "passwd:",
    "api key",
    "api_key",
    "apikey",
    "secret_access_key",
    "secret key",
    "private key",
    "-----begin",
    "bearer ",
    "authorization:",
];
const HEALTH_MARKERS: &[&str] = &[
    "patient",
    "diagnos",
    "prescri",
    "symptom",
    "medication",
    "mg daily",
    "blood pressure",
    "hypertension",
    "diabetes",
    "oncolog",
    "psychiatr",
    "therapy session",
];
const FINANCIAL_MARKERS: &[&str] = &[
    "account number",
    "routing number",
    "iban",
    "credit card",
    "salary",
    "net worth",
];
const LEGAL_MARKERS: &[&str] = &[
    "attorney-client",
    "privileged and confidential",
    "settlement agreement",
    "under seal",
];

/// A long unbroken run of key-ish characters with mixed case or digits — the
/// shape of a bearer token or API key even when it carries none of the
/// `CREDENTIAL_MARKERS` label text. `min_len` is
/// `cfg.credential_token_min_len`.
fn looks_like_secret_token(text: &str, min_len: usize) -> bool {
    text.split_whitespace().any(|t| {
        let core = t.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        core.len() >= min_len
            && core
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && core.chars().any(|c| c.is_ascii_digit())
            && core.chars().any(|c| c.is_ascii_alphabetic())
    })
}

fn looks_like_email(text: &str) -> bool {
    text.split_whitespace().any(|t| match t.find('@') {
        Some(i) => i > 0 && t[i + 1..].contains('.') && !t.ends_with('.'),
        None => false,
    })
}

/// `min_digits` is `cfg.phone_digit_min_count`.
fn looks_like_phone(text: &str, min_digits: usize) -> bool {
    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.len() >= min_digits && text.chars().any(|c| matches!(c, '-' | '(' | ')' | '+'))
}

/// Detects sensitive content in a write candidate's body via pattern and
/// lexicon matching, then folds in the caller's `sensitivity_hint`.
///
/// The hint may only RAISE the resolved level, never lower it — enforced by
/// `SensitivityLevel::raised_by`, not by any check here. A caller cannot use a
/// low hint to downgrade content this function actually detected as sensitive.
pub fn assess(cand: &Candidate, cfg: &BaselineConfig) -> SensitivityAssessment {
    let lower = cand.body.to_lowercase();
    let mut categories = Vec::new();
    let mut detected = SensitivityLevel::Internal;
    let mut confidence = cfg.sensitivity_baseline_confidence;

    let has = |markers: &[&str]| markers.iter().any(|m| lower.contains(m));

    if has(CREDENTIAL_MARKERS) || looks_like_secret_token(&cand.body, cfg.credential_token_min_len)
    {
        categories.push(SensitivityCategory::Credential);
        detected = detected.max(SensitivityLevel::Restricted);
        confidence = cfg.sensitivity_credential_confidence;
    }
    if has(HEALTH_MARKERS) {
        categories.push(SensitivityCategory::Health);
        detected = detected.max(SensitivityLevel::Sensitive);
        confidence = confidence.max(cfg.sensitivity_category_confidence);
    }
    if has(FINANCIAL_MARKERS) {
        categories.push(SensitivityCategory::Financial);
        detected = detected.max(SensitivityLevel::Sensitive);
        confidence = confidence.max(cfg.sensitivity_category_confidence);
    }
    if has(LEGAL_MARKERS) {
        categories.push(SensitivityCategory::Legal);
        detected = detected.max(SensitivityLevel::Sensitive);
        confidence = confidence.max(cfg.sensitivity_category_confidence);
    }
    if looks_like_email(&cand.body) || looks_like_phone(&cand.body, cfg.phone_digit_min_count) {
        categories.push(SensitivityCategory::Pii);
        detected = detected.max(SensitivityLevel::Personal);
        confidence = confidence.max(cfg.sensitivity_pii_confidence);
    }

    SensitivityAssessment {
        // The hint may only raise the level, never lower it.
        level: detected.raised_by(cand.sensitivity_hint),
        categories,
        confidence: Score::clamped(confidence),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::candidate_from;
    use memorysafe_core::{SensitivityCategory, SensitivityLevel};

    fn cfg() -> BaselineConfig {
        BaselineConfig::default()
    }

    #[test]
    fn ordinary_text_is_internal() {
        let a = assess(
            &candidate_from("the deployment finished at noon", None),
            &cfg(),
        );
        assert_eq!(a.level, SensitivityLevel::Internal);
        assert!(a.categories.is_empty());
    }

    #[test]
    fn credentials_are_restricted() {
        for text in [
            "the api key is sk-abc123def456ghi789jkl012",
            "password: hunter2correcthorse",
            "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMIK7MDENGbPxRfiCYEXAMPLEKEY",
        ] {
            let a = assess(&candidate_from(text, None), &cfg());
            assert_eq!(
                a.level,
                SensitivityLevel::Restricted,
                "missed credential in {text:?}"
            );
            assert!(a.categories.contains(&SensitivityCategory::Credential));
        }
    }

    #[test]
    fn health_language_is_sensitive() {
        let a = assess(
            &candidate_from("patient was diagnosed with hypertension", None),
            &cfg(),
        );
        assert!(a.level >= SensitivityLevel::Sensitive);
        assert!(a.categories.contains(&SensitivityCategory::Health));
    }

    #[test]
    fn contact_details_are_personal() {
        let a = assess(
            &candidate_from("reach them at someone@example.com", None),
            &cfg(),
        );
        assert!(a.level >= SensitivityLevel::Personal);
        assert!(a.categories.contains(&SensitivityCategory::Pii));
    }

    // `looks_like_secret_token` exists precisely so a key or bearer token is
    // caught even when it carries none of `CREDENTIAL_MARKERS`' label text —
    // every case above is decided by the lexicon before this heuristic is
    // ever reached, so it needs its own coverage.

    #[test]
    fn a_bare_token_with_no_marker_words_is_still_flagged_as_a_credential() {
        let a = assess(
            &candidate_from(
                "for reference the value is sk_ab-CD1234567890xyz today",
                None,
            ),
            &cfg(),
        );
        assert_eq!(a.level, SensitivityLevel::Restricted);
        assert!(a.categories.contains(&SensitivityCategory::Credential));
    }

    #[test]
    fn a_token_shape_needs_at_least_twenty_characters() {
        // One character short of the documented floor must not trip the
        // heuristic; at the floor, it must.
        let short = assess(
            &candidate_from("token A1b2C3d4E5f6G7h8I9j here", None),
            &cfg(),
        );
        assert!(!short.categories.contains(&SensitivityCategory::Credential));

        let at_floor = assess(
            &candidate_from("token A1b2C3d4E5f6G7h8I9j0 here", None),
            &cfg(),
        );
        assert!(
            at_floor
                .categories
                .contains(&SensitivityCategory::Credential)
        );
    }

    #[test]
    fn an_ordinary_long_word_is_not_mistaken_for_a_secret() {
        // Long and alphabetic is just English; nothing digit-shaped about it.
        let a = assess(
            &candidate_from("the aforementionedcircumstances were unusual", None),
            &cfg(),
        );
        assert!(!a.categories.contains(&SensitivityCategory::Credential));
    }

    #[test]
    fn punctuation_inside_a_long_run_rules_out_the_secret_shape() {
        // A '!' is not alphanumeric, '-', or '_' — the run does not have the
        // shape of a key even though it is long and mixes letters and digits.
        let a = assess(
            &candidate_from("an exclamation!1234567890abcdef mark was unusual", None),
            &cfg(),
        );
        assert!(!a.categories.contains(&SensitivityCategory::Credential));
    }

    // `looks_like_phone` is the other PII detector; nothing above exercises
    // it, since the PII test above is decided by the email detector.

    #[test]
    fn a_phone_number_is_personal() {
        let a = assess(
            &candidate_from("call them back at (555) 123-4567", None),
            &cfg(),
        );
        assert!(a.level >= SensitivityLevel::Personal);
        assert!(a.categories.contains(&SensitivityCategory::Pii));
    }

    #[test]
    fn ten_digits_alone_without_phone_punctuation_is_not_a_phone_number() {
        // A bare long digit run (an id, a zip+4, a price in cents) is common
        // and must not read as a phone number on digit count alone.
        let a = assess(
            &candidate_from("reference id 5551234567 was created", None),
            &cfg(),
        );
        assert!(!a.categories.contains(&SensitivityCategory::Pii));
    }

    #[test]
    fn phone_punctuation_alone_without_ten_digits_is_not_a_phone_number() {
        let a = assess(&candidate_from("call (555) 123 now", None), &cfg());
        assert!(!a.categories.contains(&SensitivityCategory::Pii));
    }

    #[test]
    fn nine_digits_falls_just_short_of_the_phone_floor() {
        let a = assess(&candidate_from("dial (555) 123-456 now", None), &cfg());
        assert!(!a.categories.contains(&SensitivityCategory::Pii));
    }

    // Email boundary conditions: none of the tests above distinguish a real
    // '@'-address from things that merely contain '@' or '.'.

    #[test]
    fn an_at_sign_at_the_start_of_a_token_is_not_an_email() {
        let a = assess(
            &candidate_from("ping @example.com for details", None),
            &cfg(),
        );
        assert!(!a.categories.contains(&SensitivityCategory::Pii));
    }

    #[test]
    fn a_domain_with_no_dot_is_not_an_email() {
        let a = assess(
            &candidate_from("reach them at someone@examplecom", None),
            &cfg(),
        );
        assert!(!a.categories.contains(&SensitivityCategory::Pii));
    }

    #[test]
    fn a_sentence_final_period_does_not_manufacture_an_email() {
        let a = assess(
            &candidate_from("write to someone@example.com.", None),
            &cfg(),
        );
        assert!(!a.categories.contains(&SensitivityCategory::Pii));
    }

    #[test]
    fn a_dot_immediately_before_the_at_sign_does_not_count_as_the_domains_dot() {
        // The domain slice must start AFTER the '@', not before it: a dot
        // sitting right in front of the '@' belongs to the local part, not
        // the domain, and must not be read as the domain's own TLD dot.
        let a = assess(
            &candidate_from("write to contact.@localhost now", None),
            &cfg(),
        );
        assert!(!a.categories.contains(&SensitivityCategory::Pii));
    }

    #[test]
    fn a_hint_raises_but_never_lowers() {
        let raised = assess(
            &candidate_from("innocuous note", Some(SensitivityLevel::Restricted)),
            &cfg(),
        );
        assert_eq!(raised.level, SensitivityLevel::Restricted);

        let not_lowered = assess(
            &candidate_from(
                "the api key is sk-abc123def456ghi789jkl012",
                Some(SensitivityLevel::Public),
            ),
            &cfg(),
        );
        assert_eq!(
            not_lowered.level,
            SensitivityLevel::Restricted,
            "a caller hint must never lower a detected level"
        );
    }

    // F3: every threshold promoted into `BaselineConfig` gets a test proving
    // it is actually READ from config, at its boundary.

    #[test]
    fn credential_token_floor_is_configurable() {
        // 21 characters trips the default floor (20) but must stop tripping
        // a raised, configured floor.
        let token_text = "for reference the value is A1b2C3d4E5f6G7h8I9j0X today";
        assert!(
            assess(&candidate_from(token_text, None), &cfg())
                .categories
                .contains(&SensitivityCategory::Credential)
        );
        let raised_cfg = BaselineConfig {
            credential_token_min_len: 30,
            ..BaselineConfig::default()
        };
        assert!(
            !assess(&candidate_from(token_text, None), &raised_cfg)
                .categories
                .contains(&SensitivityCategory::Credential)
        );
    }

    #[test]
    fn phone_digit_floor_is_configurable() {
        // 10 digits trips the default floor but must stop tripping a raised,
        // configured floor.
        let text = "call them back at (555) 123-4567";
        assert!(
            assess(&candidate_from(text, None), &cfg())
                .categories
                .contains(&SensitivityCategory::Pii)
        );
        let raised_cfg = BaselineConfig {
            phone_digit_min_count: 11,
            ..BaselineConfig::default()
        };
        assert!(
            !assess(&candidate_from(text, None), &raised_cfg)
                .categories
                .contains(&SensitivityCategory::Pii)
        );
    }

    #[test]
    fn sensitivity_confidences_are_configurable() {
        let custom = BaselineConfig {
            sensitivity_baseline_confidence: 0.11,
            sensitivity_credential_confidence: 0.91,
            sensitivity_category_confidence: 0.71,
            sensitivity_pii_confidence: 0.61,
            ..BaselineConfig::default()
        };

        let baseline = assess(
            &candidate_from("the deployment finished at noon", None),
            &custom,
        );
        assert_eq!(baseline.confidence.get(), 0.11);

        let credential = assess(
            &candidate_from("the api key is sk-abc123def456ghi789jkl012", None),
            &custom,
        );
        assert_eq!(credential.confidence.get(), 0.91);

        let health = assess(
            &candidate_from("patient was diagnosed with hypertension", None),
            &custom,
        );
        assert_eq!(health.confidence.get(), 0.71);

        let pii = assess(
            &candidate_from("reach them at someone@example.com", None),
            &custom,
        );
        assert_eq!(pii.confidence.get(), 0.61);
    }
}

//! Ed25519 signatures for rule sets.
//!
//! The payload is signed byte for byte as it is stored in the file, so no
//! canonicalisation step can be abused to smuggle in different content.

use crate::rules::RuleSet;
use base64::prelude::{Engine, BASE64_STANDARD};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use ghostify_core::{Error, Result};
use serde::{Deserialize, Serialize};

/// A public key Ghostify trusts for rule sets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedKey {
    pub key_id: String,
    /// Base64 of the 32 byte Ed25519 public key.
    pub public_key: String,
}

/// File format of a distributed rule set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRuleSet {
    pub key_id: String,
    /// Base64 of the 64 byte signature over `payload`.
    pub signature: String,
    /// The rule set as JSON text.
    pub payload: String,
}

/// Signs a rule set. Used by the release tooling; the secret key never ships
/// with the application.
pub fn sign(ruleset: &RuleSet, key_id: &str, secret: &SigningKey) -> Result<SignedRuleSet> {
    ruleset.validate()?;
    let payload = serde_json::to_string(ruleset)?;
    let signature = secret.sign(payload.as_bytes());
    Ok(SignedRuleSet {
        key_id: key_id.to_owned(),
        signature: BASE64_STANDARD.encode(signature.to_bytes()),
        payload,
    })
}

/// Encodes the public half of a key for [`TrustedKey::public_key`].
pub fn encode_public_key(secret: &SigningKey) -> String {
    BASE64_STANDARD.encode(secret.verifying_key().to_bytes())
}

/// Verifies signature and structure and returns the rule set.
///
/// `min_sequence` is the sequence of the rule set applied last; anything older
/// is refused so that a captured old (but validly signed) file cannot be
/// replayed.
pub fn verify(signed: &SignedRuleSet, keys: &[TrustedKey], min_sequence: u64) -> Result<RuleSet> {
    let key = keys
        .iter()
        .find(|key| key.key_id == signed.key_id)
        .ok_or_else(|| {
            Error::PermissionDenied(format!(
                "rule set signed by untrusted key `{}`",
                signed.key_id
            ))
        })?;

    let key_bytes: [u8; 32] = BASE64_STANDARD
        .decode(&key.public_key)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| Error::Invalid(format!("trusted key `{}` is malformed", key.key_id)))?;
    let verifying = VerifyingKey::from_bytes(&key_bytes)
        .map_err(|_| Error::Invalid(format!("trusted key `{}` is malformed", key.key_id)))?;

    let signature_bytes = BASE64_STANDARD
        .decode(&signed.signature)
        .map_err(|_| Error::Invalid("signature is not valid base64".into()))?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| Error::Invalid("signature has the wrong length".into()))?;

    verifying
        .verify_strict(signed.payload.as_bytes(), &signature)
        .map_err(|_| Error::PermissionDenied("rule set signature does not match".into()))?;

    let ruleset: RuleSet = serde_json::from_str(&signed.payload)?;
    ruleset.validate()?;
    if ruleset.sequence < min_sequence {
        return Err(Error::PermissionDenied(format!(
            "rule set sequence {} is older than the last applied sequence {min_sequence}",
            ruleset.sequence
        )));
    }
    Ok(ruleset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::builtin;

    fn secret(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn trusted(seed: u8, id: &str) -> Vec<TrustedKey> {
        vec![TrustedKey {
            key_id: id.into(),
            public_key: encode_public_key(&secret(seed)),
        }]
    }

    fn ruleset(sequence: u64) -> RuleSet {
        let mut set = builtin();
        set.version = format!("test-{sequence}");
        set.sequence = sequence;
        set
    }

    #[test]
    fn signed_rule_set_verifies() {
        let signed = sign(&ruleset(5), "release", &secret(1)).unwrap();
        let loaded = verify(&signed, &trusted(1, "release"), 5).unwrap();
        assert_eq!(loaded, ruleset(5));
    }

    #[test]
    fn untrusted_or_wrong_keys_are_refused() {
        let signed = sign(&ruleset(1), "release", &secret(1)).unwrap();
        let err = verify(&signed, &trusted(1, "other"), 0).unwrap_err();
        assert_eq!(err.code(), "permission_denied");
        let err = verify(&signed, &trusted(2, "release"), 0).unwrap_err();
        assert_eq!(err.code(), "permission_denied");
    }

    #[test]
    fn tampering_with_the_payload_is_detected() {
        let mut signed = sign(&ruleset(1), "release", &secret(1)).unwrap();
        signed.payload = signed
            .payload
            .replace("builtin-2", "evil-1")
            .replace("test-1", "evil-1");
        let err = verify(&signed, &trusted(1, "release"), 0).unwrap_err();
        assert_eq!(err.code(), "permission_denied");
    }

    #[test]
    fn replaying_an_older_rule_set_is_refused() {
        let signed = sign(&ruleset(3), "release", &secret(1)).unwrap();
        let keys = trusted(1, "release");
        assert!(verify(&signed, &keys, 3).is_ok());
        let err = verify(&signed, &keys, 4).unwrap_err();
        assert_eq!(err.code(), "permission_denied");
    }

    #[test]
    fn malformed_inputs_are_rejected_not_panicked_on() {
        let keys = trusted(1, "release");
        let good = sign(&ruleset(1), "release", &secret(1)).unwrap();

        let mut bad = good.clone();
        bad.signature = "!!!not base64!!!".into();
        assert_eq!(verify(&bad, &keys, 0).unwrap_err().code(), "invalid_input");

        let mut bad = good.clone();
        bad.signature = BASE64_STANDARD.encode([0u8; 10]);
        assert_eq!(verify(&bad, &keys, 0).unwrap_err().code(), "invalid_input");

        let bad_keys = vec![TrustedKey {
            key_id: "release".into(),
            public_key: "AAAA".into(),
        }];
        assert_eq!(
            verify(&good, &bad_keys, 0).unwrap_err().code(),
            "invalid_input"
        );
    }

    #[test]
    fn signing_refuses_invalid_rule_sets() {
        let mut set = ruleset(1);
        set.rules.clear();
        assert!(sign(&set, "release", &secret(1)).is_err());
    }

    #[test]
    fn validly_signed_but_structurally_invalid_payload_is_rejected() {
        let mut set = ruleset(1);
        set.rules[0].file = "../escape".into();
        let payload = serde_json::to_string(&set).unwrap();
        let key = secret(1);
        let signed = SignedRuleSet {
            key_id: "release".into(),
            signature: BASE64_STANDARD.encode(key.sign(payload.as_bytes()).to_bytes()),
            payload,
        };
        assert_eq!(
            verify(&signed, &trusted(1, "release"), 0)
                .unwrap_err()
                .code(),
            "invalid_input"
        );
    }
}

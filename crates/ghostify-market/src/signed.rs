//! Ed25519 signature over the exact bytes of the index file.

use crate::index::Index;
use base64::prelude::{Engine, BASE64_STANDARD};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use ghostify_core::{Error, Result};
use serde::{Deserialize, Serialize};

pub const MAX_INDEX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedKey {
    pub key_id: String,
    /// Base64 of the 32 byte public key.
    pub public_key: String,
}

/// The file served as `index.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedIndex {
    pub key_id: String,
    /// Base64 of the 64 byte signature over `payload`.
    pub signature: String,
    /// The [`Index`] as JSON text, signed byte for byte.
    pub payload: String,
}

pub fn generate_key() -> Result<SigningKey> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed)
        .map_err(|e| Error::Internal(format!("no secure random source: {e}")))?;
    Ok(SigningKey::from_bytes(&seed))
}

pub fn encode_public_key(key: &SigningKey) -> String {
    BASE64_STANDARD.encode(key.verifying_key().to_bytes())
}

pub fn encode_secret_key(key: &SigningKey) -> String {
    BASE64_STANDARD.encode(key.to_bytes())
}

pub fn decode_secret_key(text: &str) -> Result<SigningKey> {
    let bytes: [u8; 32] = BASE64_STANDARD
        .decode(text.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| Error::Invalid("the secret key must be 32 bytes of base64".into()))?;
    Ok(SigningKey::from_bytes(&bytes))
}

/// Signs an index. Used by the registry's release workflow, never by the app.
pub fn sign(index: &Index, key_id: &str, key: &SigningKey) -> Result<SignedIndex> {
    index.validate()?;
    let payload = serde_json::to_string(index)?;
    Ok(SignedIndex {
        key_id: key_id.to_owned(),
        signature: BASE64_STANDARD.encode(key.sign(payload.as_bytes()).to_bytes()),
        payload,
    })
}

/// Checks the signature, parses and validates the index, and refuses indexes
/// older than `min_sequence` (replay of an old but validly signed file).
pub fn verify(signed: &SignedIndex, keys: &[TrustedKey], min_sequence: u64) -> Result<Index> {
    let key = keys
        .iter()
        .find(|k| k.key_id == signed.key_id)
        .ok_or_else(|| {
            Error::PermissionDenied(format!(
                "the index is signed by an untrusted key `{}`",
                signed.key_id
            ))
        })?;
    let key_bytes: [u8; 32] = BASE64_STANDARD
        .decode(&key.public_key)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| Error::Invalid(format!("trusted key `{}` is malformed", key.key_id)))?;
    let verifying = VerifyingKey::from_bytes(&key_bytes)
        .map_err(|_| Error::Invalid(format!("trusted key `{}` is malformed", key.key_id)))?;
    let signature = BASE64_STANDARD
        .decode(&signed.signature)
        .ok()
        .and_then(|b| Signature::from_slice(&b).ok())
        .ok_or_else(|| Error::Invalid("the index signature is malformed".into()))?;
    verifying
        .verify(signed.payload.as_bytes(), &signature)
        .map_err(|_| Error::PermissionDenied("the index signature is invalid".into()))?;

    let index: Index = serde_json::from_str(&signed.payload)
        .map_err(|e| Error::Invalid(format!("the index is malformed: {e}")))?;
    index.validate()?;
    if index.sequence < min_sequence {
        return Err(Error::PermissionDenied(format!(
            "the index is older (sequence {}) than the one already applied ({min_sequence})",
            index.sequence
        )));
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::tests::sample_index;

    fn keys(key: &SigningKey) -> Vec<TrustedKey> {
        vec![TrustedKey {
            key_id: "main".into(),
            public_key: encode_public_key(key),
        }]
    }

    #[test]
    fn sign_and_verify_round_trip() {
        let key = generate_key().unwrap();
        let signed = sign(&sample_index(5), "main", &key).unwrap();
        let index = verify(&signed, &keys(&key), 0).unwrap();
        assert_eq!(index.sequence, 5);
        // The file survives serialisation as JSON.
        let text = serde_json::to_string(&signed).unwrap();
        let again: SignedIndex = serde_json::from_str(&text).unwrap();
        assert_eq!(verify(&again, &keys(&key), 5).unwrap().listings.len(), 2);
    }

    #[test]
    fn tampering_is_detected() {
        let key = generate_key().unwrap();
        let mut signed = sign(&sample_index(1), "main", &key).unwrap();
        signed.payload = signed.payload.replace("\"stars\":10", "\"stars\":99");
        assert_eq!(
            verify(&signed, &keys(&key), 0).unwrap_err().code(),
            "permission_denied"
        );

        let mut other = sign(&sample_index(1), "main", &key).unwrap();
        other.payload.push(' ');
        assert!(verify(&other, &keys(&key), 0).is_err());
    }

    #[test]
    fn untrusted_or_wrong_keys_are_refused() {
        let (a, b) = (generate_key().unwrap(), generate_key().unwrap());
        let signed = sign(&sample_index(1), "main", &a).unwrap();
        assert!(
            verify(&signed, &keys(&b), 0).is_err(),
            "right id, wrong key"
        );
        let mut renamed = signed.clone();
        renamed.key_id = "other".into();
        assert_eq!(
            verify(&renamed, &keys(&a), 0).unwrap_err().code(),
            "permission_denied"
        );
        assert!(verify(&signed, &[], 0).is_err());
    }

    #[test]
    fn old_indexes_are_refused_but_the_same_sequence_is_fine() {
        let key = generate_key().unwrap();
        let signed = sign(&sample_index(7), "main", &key).unwrap();
        assert!(verify(&signed, &keys(&key), 8).is_err());
        assert!(verify(&signed, &keys(&key), 7).is_ok());
    }

    #[test]
    fn malformed_inputs_are_errors_not_panics() {
        let key = generate_key().unwrap();
        let good = sign(&sample_index(1), "main", &key).unwrap();
        for signature in ["", "!!!", "AAAA"] {
            let bad = SignedIndex {
                signature: signature.into(),
                ..good.clone()
            };
            assert!(verify(&bad, &keys(&key), 0).is_err(), "{signature:?}");
        }
        let broken_key = vec![TrustedKey {
            key_id: "main".into(),
            public_key: "AAAA".into(),
        }];
        assert_eq!(
            verify(&good, &broken_key, 0).unwrap_err().code(),
            "invalid_input"
        );
        // A correctly signed payload that is not an index.
        let payload = "{\"nope\":1}".to_owned();
        let signed = SignedIndex {
            key_id: "main".into(),
            signature: BASE64_STANDARD.encode(key.sign(payload.as_bytes()).to_bytes()),
            payload,
        };
        assert!(verify(&signed, &keys(&key), 0).is_err());
    }

    #[test]
    fn secret_key_encoding_round_trips() {
        let key = generate_key().unwrap();
        let again = decode_secret_key(&encode_secret_key(&key)).unwrap();
        assert_eq!(encode_public_key(&again), encode_public_key(&key));
        assert!(decode_secret_key("short").is_err());
    }
}

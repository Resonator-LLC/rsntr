//! The JSON Web Tokens the relay signs: ES256 for APNs provider tokens,
//! RS256 for the OAuth assertion of an FCM service account. Signed with
//! ring; never logged.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{
    ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, RSA_PKCS1_SHA256, RsaKeyPair,
};
use serde_json::Value;

/// A key that cannot be used.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct KeyError(pub String);

/// The DER bytes of the first PEM block of `text` (any label).
pub fn pem_to_der(text: &str) -> Result<Vec<u8>, KeyError> {
    let mut inside = false;
    let mut body = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("-----BEGIN ") {
            inside = true;
        } else if line.starts_with("-----END ") {
            break;
        } else if inside {
            body.push_str(line);
        }
    }
    if body.is_empty() {
        return Err(KeyError("no PEM block".to_owned()));
    }
    STANDARD
        .decode(body)
        .map_err(|_| KeyError("bad base64 in the PEM block".to_owned()))
}

/// An ES256 key from a PKCS#8 PEM (an APNs `.p8`).
pub fn es256_key(pem: &str) -> Result<EcdsaKeyPair, KeyError> {
    let der = pem_to_der(pem)?;
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &der, &SystemRandom::new())
        .map_err(|error| KeyError(format!("not a P-256 PKCS#8 key: {error}")))
}

/// An RS256 key from a PKCS#8 PEM (a service account's `private_key`).
pub fn rs256_key(pem: &str) -> Result<RsaKeyPair, KeyError> {
    let der = pem_to_der(pem)?;
    RsaKeyPair::from_pkcs8(&der)
        .map_err(|error| KeyError(format!("not an RSA PKCS#8 key: {error}")))
}

fn signing_input(header: &Value, claims: &Value) -> String {
    format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}

/// `header.claims.signature`, ES256.
pub fn sign_es256(key: &EcdsaKeyPair, header: &Value, claims: &Value) -> Result<String, KeyError> {
    let input = signing_input(header, claims);
    let signature = key
        .sign(&SystemRandom::new(), input.as_bytes())
        .map_err(|_| KeyError("ES256 signing failed".to_owned()))?;
    Ok(format!(
        "{input}.{}",
        URL_SAFE_NO_PAD.encode(signature.as_ref())
    ))
}

/// `header.claims.signature`, RS256.
pub fn sign_rs256(key: &RsaKeyPair, header: &Value, claims: &Value) -> Result<String, KeyError> {
    let input = signing_input(header, claims);
    let mut signature = vec![0u8; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        input.as_bytes(),
        &mut signature,
    )
    .map_err(|_| KeyError("RS256 signing failed".to_owned()))?;
    Ok(format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature)))
}

/// The header and claims of a JWT, unchecked (tests and fakes).
pub fn decode_unverified(jwt: &str) -> Option<(Value, Value)> {
    let mut parts = jwt.split('.');
    let header = URL_SAFE_NO_PAD.decode(parts.next()?).ok()?;
    let claims = URL_SAFE_NO_PAD.decode(parts.next()?).ok()?;
    Some((
        serde_json::from_slice(&header).ok()?,
        serde_json::from_slice(&claims).ok()?,
    ))
}

/// Unix seconds now.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{ECDSA_P256_SHA256_FIXED, KeyPair, UnparsedPublicKey};
    use serde_json::json;

    pub(crate) fn p8() -> String {
        let doc =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
                .unwrap();
        format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            STANDARD.encode(doc.as_ref())
        )
    }

    #[test]
    fn es256_tokens_verify() {
        let key = es256_key(&p8()).unwrap();
        let jwt = sign_es256(
            &key,
            &json!({"alg": "ES256", "kid": "K"}),
            &json!({"iss": "T"}),
        )
        .unwrap();
        let (header, claims) = decode_unverified(&jwt).unwrap();
        assert_eq!(header["kid"], "K");
        assert_eq!(claims["iss"], "T");
        let (input, signature) = jwt.rsplit_once('.').unwrap();
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, key.public_key().as_ref())
            .verify(
                input.as_bytes(),
                &URL_SAFE_NO_PAD.decode(signature).unwrap(),
            )
            .unwrap();
        assert!(es256_key("nothing").is_err());
        assert!(es256_key("-----BEGIN X-----\nAAAA\n-----END X-----").is_err());
    }
}

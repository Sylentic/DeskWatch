//! Signing in as a GitHub App instead of as a person.
//!
//! The App's private key (the `.pem` file GitHub hands out once) signs a short
//! JSON Web Token (RS256) that names the App. GitHub swaps that for an
//! installation token that lasts an hour and carries only the rights the
//! organisation gave the App. The exchange itself is in `api.rs`; this file
//! only reads the key and signs the token.

use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};

use crate::source::Secret;

/// A GitHub App installed on an organisation or account.
pub struct GithubApp {
    app_id: String,
    pub(super) installation_id: String,
    key: RsaKeyPair,
}

impl GithubApp {
    /// `private_key` is the PEM text: `BEGIN RSA PRIVATE KEY` (what GitHub
    /// gives out) or `BEGIN PRIVATE KEY`. A key that cannot be read fails here,
    /// not at the first poll.
    pub fn new(app_id: &str, installation_id: &str, private_key: &Secret) -> Result<Self> {
        Ok(Self {
            app_id: app_id.trim().to_string(),
            installation_id: installation_id.trim().to_string(),
            key: parse_key(private_key.expose())?,
        })
    }

    /// The signed token that proves we are the App. GitHub accepts at most ten
    /// minutes; it is backdated a minute for clocks that run a little behind.
    pub fn jwt(&self, now_unix: u64) -> Result<String> {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let claims = serde_json::json!({
            "iat": now_unix.saturating_sub(60),
            "exp": now_unix + 9 * 60,
            "iss": self.app_id,
        });
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signing_input = format!("{header}.{payload}");
        let mut signature = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signing_input.as_bytes(),
                &mut signature,
            )
            .map_err(|_| anyhow::anyhow!("cannot sign with the private key"))?;
        Ok(format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature)
        ))
    }
}

/// Read an RSA key from PEM text. The text is never part of an error message.
fn parse_key(pem: &str) -> Result<RsaKeyPair> {
    let mut label = None;
    let mut body = String::new();
    for line in pem.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("-----BEGIN ") {
            label = rest.strip_suffix("-----");
        } else if !line.starts_with("-----") {
            body.push_str(line);
        }
    }
    let label = label.context("the private key is not PEM text (no BEGIN line)")?;
    let der = STANDARD
        .decode(body.as_bytes())
        .context("the private key is not valid PEM text")?;
    let key = match label {
        "RSA PRIVATE KEY" => RsaKeyPair::from_der(&der),
        "PRIVATE KEY" => RsaKeyPair::from_pkcs8(&der),
        other => anyhow::bail!("the private key is a {other:?}, not an RSA private key"),
    };
    key.map_err(|_| anyhow::anyhow!("the private key is not a usable RSA key (2048 bits or more)"))
}

#[cfg(test)]
pub(crate) mod tests {
    use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};

    use super::*;

    /// Throwaway keys made for these tests, kept without their PEM lines.
    pub(crate) const PKCS1: &str = include_str!("testdata/app_key_pkcs1.b64");
    const PKCS8: &str = include_str!("testdata/app_key_pkcs8.b64");
    pub(crate) const PUBLIC: &str = include_str!("testdata/app_key_public.b64");

    pub(crate) fn pem(label: &str, body: &str) -> Secret {
        let lines: Vec<&str> = body
            .as_bytes()
            .chunks(64)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        Secret::new(format!(
            "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
            lines.join("\n")
        ))
    }

    #[test]
    fn signs_a_token_github_can_check() {
        let app = GithubApp::new(" 12345 ", "678", &pem("RSA PRIVATE KEY", PKCS1)).unwrap();
        let jwt = app.jwt(1_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "12345");
        assert_eq!(claims["iat"], 999_940);
        assert_eq!(claims["exp"], 1_000_540);
        let public = STANDARD.decode(PUBLIC).unwrap();
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, public)
            .verify(
                format!("{}.{}", parts[0], parts[1]).as_bytes(),
                &URL_SAFE_NO_PAD.decode(parts[2]).unwrap(),
            )
            .expect("the signature checks out against the public key");
    }

    #[test]
    fn reads_both_pem_forms() {
        assert!(GithubApp::new("1", "2", &pem("PRIVATE KEY", PKCS8)).is_ok());
        assert!(GithubApp::new("1", "2", &pem("RSA PRIVATE KEY", PKCS1)).is_ok());
    }

    #[test]
    fn a_bad_key_is_refused_without_echoing_it() {
        for bad in [
            "not a key at all",
            "-----BEGIN RSA PRIVATE KEY-----\n!!!\n-----END RSA PRIVATE KEY-----",
            "-----BEGIN RSA PRIVATE KEY-----\nQUJD\n-----END RSA PRIVATE KEY-----",
            "-----BEGIN EC PRIVATE KEY-----\nQUJD\n-----END EC PRIVATE KEY-----",
        ] {
            let err = GithubApp::new("1", "2", &Secret::new(bad)).err().unwrap();
            let text = format!("{err:#}");
            assert!(!text.contains("QUJD") && !text.contains("!!!"), "{text}");
        }
    }
}

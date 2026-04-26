// HTTPS client for `PUT /v1/devices/link`. Shares the Xous trust store
// through rustls::ClientConfig built by tls::Tls::new().client_config().
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::panic)]

use crate::manager::account_attrs::AccountAttributes;
use crate::manager::prekeys::{KyberPreKeyJson, Prekeys, SignedPreKeyJson};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use tls::Tls;
use url::Url;

#[derive(Serialize)]
pub struct SignedPreKeyEntity {
    #[serde(rename = "keyId")]
    pub key_id: u32,
    #[serde(rename = "publicKey")]
    pub public_key: String,
    pub signature: String,
}

impl From<SignedPreKeyJson> for SignedPreKeyEntity {
    fn from(k: SignedPreKeyJson) -> Self {
        Self {
            key_id: k.key_id,
            public_key: k.public_key_b64url,
            signature: k.signature_b64url,
        }
    }
}

#[derive(Serialize)]
pub struct KyberPreKeyEntity {
    #[serde(rename = "keyId")]
    pub key_id: u32,
    #[serde(rename = "publicKey")]
    pub public_key: String,
    pub signature: String,
}

impl From<KyberPreKeyJson> for KyberPreKeyEntity {
    fn from(k: KyberPreKeyJson) -> Self {
        Self {
            key_id: k.key_id,
            public_key: k.public_key_b64url,
            signature: k.signature_b64url,
        }
    }
}

#[derive(Serialize)]
pub struct LinkDeviceRequestBody {
    #[serde(rename = "verificationCode")]
    pub verification_code: String,
    #[serde(rename = "accountAttributes")]
    pub account_attributes: AccountAttributes,
    #[serde(rename = "aciSignedPreKey")]
    pub aci_signed_pre_key: SignedPreKeyEntity,
    #[serde(rename = "pniSignedPreKey")]
    pub pni_signed_pre_key: SignedPreKeyEntity,
    #[serde(rename = "aciPqLastResortPreKey")]
    pub aci_pq_last_resort_pre_key: KyberPreKeyEntity,
    #[serde(rename = "pniPqLastResortPreKey")]
    pub pni_pq_last_resort_pre_key: KyberPreKeyEntity,
    #[serde(rename = "gcmToken", skip_serializing_if = "Option::is_none")]
    pub gcm_token: Option<()>,
}

impl LinkDeviceRequestBody {
    pub fn from_parts(
        verification_code: String,
        account_attributes: AccountAttributes,
        prekeys: &Prekeys,
    ) -> Self {
        use crate::manager::prekeys::{KyberPreKeyJson, SignedPreKeyJson};
        fn spk_to_entity(k: &SignedPreKeyJson) -> SignedPreKeyEntity {
            SignedPreKeyEntity {
                key_id: k.key_id,
                public_key: k.public_key_b64url.clone(),
                signature: k.signature_b64url.clone(),
            }
        }
        fn kpk_to_entity(k: &KyberPreKeyJson) -> KyberPreKeyEntity {
            KyberPreKeyEntity {
                key_id: k.key_id,
                public_key: k.public_key_b64url.clone(),
                signature: k.signature_b64url.clone(),
            }
        }
        Self {
            verification_code,
            account_attributes,
            aci_signed_pre_key: spk_to_entity(&prekeys.aci_signed),
            pni_signed_pre_key: spk_to_entity(&prekeys.pni_signed),
            aci_pq_last_resort_pre_key: kpk_to_entity(&prekeys.aci_kyber_last_resort),
            pni_pq_last_resort_pre_key: kpk_to_entity(&prekeys.pni_kyber_last_resort),
            gcm_token: None,
        }
    }
}

fn de_u32_from_str_or_num<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    use serde::de::{self, Visitor};
    struct U32OrStr;
    impl<'de> Visitor<'de> for U32OrStr {
        type Value = u32;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("u32 or decimal string")
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<u32, E> {
            use std::convert::TryFrom as _;
            u32::try_from(v).map_err(de::Error::custom)
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<u32, E> {
            v.parse().map_err(de::Error::custom)
        }
    }
    d.deserialize_any(U32OrStr)
}

#[derive(Deserialize, Debug)]
pub struct LinkDeviceResponse {
    pub uuid: String,
    pub pni: String,
    #[serde(rename = "deviceId", deserialize_with = "de_u32_from_str_or_num")]
    pub device_id: u32,
}

/// PUT {base_url}/v1/devices/link with Basic auth.
///
/// First attempt uses `<e164>.-1` as the Basic-auth identifier (Signal's
/// sentinel for a device that has not yet been assigned a deviceId). On
/// 403, the call retries once with the suffix stripped — a one-shot
/// diagnostic hedge for open Spec Question #1. Any other status, or a
/// second 403, propagates.
pub fn put_devices_link(
    base_url: &Url,
    phone_number: &str,
    password: &str,
    body: &LinkDeviceRequestBody,
) -> Result<LinkDeviceResponse, Error> {
    let mut link_url = base_url.clone();
    link_url.set_path("/v1/devices/link");
    let link_url_str = link_url.to_string();

    let client_config = Arc::new(Tls::new().client_config());
    let agent = ureq::AgentBuilder::new()
        .tls_config(client_config)
        .build();

    let identifier = format!("{}.-1", phone_number);
    let auth_value = basic_auth_header(&identifier, password);
    log::info!(
        "PUT {link_url_str} with Basic auth for identifier={identifier} (password redacted)"
    );

    match try_put(&agent, &link_url_str, &auth_value, body) {
        Ok(r) => Ok(r),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            let identifier_no_suffix = phone_number.to_string();
            let auth2 = basic_auth_header(&identifier_no_suffix, password);
            log::warn!(
                "first PUT returned 403; retrying once without .-1 suffix, identifier={identifier_no_suffix}"
            );
            try_put(&agent, &link_url_str, &auth2, body)
        }
        Err(e) => Err(e),
    }
}

fn basic_auth_header(identifier: &str, password: &str) -> String {
    let raw = format!("{}:{}", identifier, password);
    format!("Basic {}", STANDARD.encode(raw.as_bytes()))
}

/// Replace the JSON value of `verificationCode` with a short redaction so the
/// single-use code does not leak into logs. Used only in the outgoing-body
/// diagnostic log.
fn redact_verification_code(json: &str) -> String {
    let key = "\"verificationCode\":\"";
    if let Some(start) = json.find(key) {
        let vstart = start + key.len();
        if let Some(rel_end) = json[vstart..].find('"') {
            let mut out = String::with_capacity(json.len());
            out.push_str(&json[..vstart]);
            out.push_str("<redacted>");
            out.push_str(&json[vstart + rel_end..]);
            return out;
        }
    }
    json.to_string()
}

fn try_put(
    agent: &ureq::Agent,
    url: &str,
    auth_value: &str,
    body: &LinkDeviceRequestBody,
) -> Result<LinkDeviceResponse, Error> {
    let json = serde_json::to_string(body).map_err(|e| {
        log::error!("JSON body serialize failed: {e}");
        Error::new(ErrorKind::Other, "failed to serialize link body")
    })?;
    // The Xous log-server truncates huge log lines; dump the redacted body
    // to a file on hosted so 422 post-mortems can see the full payload.
    let redacted = redact_verification_code(&json);
    #[cfg(feature = "hosted")]
    {
        use std::io::Write as _;
        let path = "/tmp/sigchat-link-body.json";
        match std::fs::File::create(path) {
            Ok(mut f) => match f.write_all(redacted.as_bytes()) {
                Ok(()) => log::info!("link request body dumped to {path} (len={})", json.len()),
                Err(e) => log::warn!("body file write failed: {e}"),
            },
            Err(e) => log::warn!("body file create failed: {e}"),
        }
    }
    #[cfg(not(feature = "hosted"))]
    {
        let _ = &redacted; // file dump is hosted-only for now
        log::info!("link request body len={}", json.len());
    }

    let resp = agent
        .put(url)
        .set("Authorization", auth_value)
        .set("Content-Type", "application/json")
        .send_bytes(json.as_bytes());

    match resp {
        Ok(r) => {
            let status = r.status();
            log::info!("PUT {url} -> {status}");
            let resp_str = r.into_string().map_err(|e| {
                log::error!("read response body: {e}");
                Error::new(ErrorKind::InvalidData, "failed to read response body")
            })?;
            serde_json::from_str::<LinkDeviceResponse>(&resp_str).map_err(|e| {
                let preview: String = resp_str.chars().take(200).collect();
                log::error!("parse LinkDeviceResponse: {e}; body[0..200]={preview}");
                Error::new(ErrorKind::InvalidData, "failed to parse link response")
            })
        }
        Err(ureq::Error::Status(code, r)) => {
            let body_text = r.into_string().unwrap_or_default();
            let preview: String = body_text.chars().take(200).collect();
            #[cfg(feature = "hosted")]
            {
                use std::io::Write as _;
                let err_path = "/tmp/sigchat-link-error-body.txt";
                if let Ok(mut f) = std::fs::File::create(err_path) {
                    let _ = f.write_all(body_text.as_bytes());
                    log::info!("server {code} error body dumped to {err_path} (len={})", body_text.len());
                }
            }
            let kind = match code {
                403 => {
                    log::error!(
                        "403: auth wrong (verificationCode or Authorization header). body[0..200]={preview}"
                    );
                    ErrorKind::PermissionDenied
                }
                409 => {
                    log::error!(
                        "409: missing capability — check spqr=true in request. body[0..200]={preview}"
                    );
                    ErrorKind::Other
                }
                411 => {
                    log::error!(
                        "411: account has max linked devices. body[0..200]={preview}"
                    );
                    ErrorKind::Other
                }
                422 => {
                    log::error!(
                        "422: malformed body — check field names vs spec §4. body[0..200]={preview}"
                    );
                    ErrorKind::InvalidData
                }
                429 => {
                    log::error!(
                        "429: rate limited — wait before retrying. body[0..200]={preview}"
                    );
                    ErrorKind::Other
                }
                n => {
                    log::error!("unexpected status {n}. body[0..200]={preview}");
                    ErrorKind::Other
                }
            };
            Err(Error::new(kind, format!("HTTP {code}")))
        }
        Err(ureq::Error::Transport(e)) => {
            log::error!("transport error during PUT: {e}");
            Err(Error::new(ErrorKind::Other, "transport error"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_auth_matches_reference_encoding() {
        // Sanity for the Basic-auth construction; values from a manual
        // base64 encode of "+14155552671.-1:hunter2hunter2hunter2hh" (24-char
        // pw like generate_link_password would produce).
        let header = basic_auth_header("+14155552671.-1", "hunter2hunter2hunter2hh");
        assert!(header.starts_with("Basic "));
        let decoded = STANDARD
            .decode(&header["Basic ".len()..])
            .expect("valid base64");
        let decoded_str = std::str::from_utf8(&decoded).expect("utf-8");
        assert_eq!(
            decoded_str,
            "+14155552671.-1:hunter2hunter2hunter2hh"
        );
    }

    #[test]
    fn device_id_string_parses_to_u32() {
        let raw = r#"{"uuid":"u","pni":"p","deviceId":"2"}"#;
        let r: LinkDeviceResponse = serde_json::from_str(raw).expect("parse");
        assert_eq!(r.device_id, 2);
        assert_eq!(r.uuid, "u");
        assert_eq!(r.pni, "p");
    }

    #[test]
    fn body_serializes_with_expected_keys() {
        use crate::manager::account_attrs::build_account_attributes;
        use crate::manager::prekeys;
        use libsignal_protocol::IdentityKeyPair;
        use rand::TryRngCore as _;

        let attrs = build_account_attributes(
            "name".to_string(),
            &[0u8; 32],
            1,
            2,
        )
        .expect("attrs");
        let mut rng = rand::rngs::OsRng.unwrap_err();
        let aci_pair = IdentityKeyPair::generate(&mut rng);
        let pni_pair = IdentityKeyPair::generate(&mut rng);
        let prekeys = prekeys::generate_prekeys(aci_pair.private_key(), pni_pair.private_key())
            .expect("generate_prekeys");
        let body = LinkDeviceRequestBody::from_parts("VC".into(), attrs, &prekeys);
        let json = serde_json::to_value(&body).expect("serialize");
        for key in [
            "verificationCode",
            "accountAttributes",
            "aciSignedPreKey",
            "pniSignedPreKey",
            "aciPqLastResortPreKey",
            "pniPqLastResortPreKey",
        ] {
            assert!(json.get(key).is_some(), "missing {} in body", key);
        }
        // gcmToken is None and must be omitted (not null) per spec §4.
        assert!(json.get("gcmToken").is_none());
        assert_eq!(json["verificationCode"], serde_json::json!("VC"));
        // keyId is a random u32 — just check it's present and in valid range.
        let key_id = json["aciSignedPreKey"]["keyId"].as_u64().expect("keyId u64");
        assert!(key_id >= 1 && key_id <= 0x00FF_FFFF, "keyId out of Medium range");
        // publicKey is standard-no-pad base64 of a 33-byte EC key = 44 chars.
        let pk = json["aciSignedPreKey"]["publicKey"].as_str().expect("publicKey");
        assert_eq!(pk.len(), 44, "publicKey wrong length");
    }
}

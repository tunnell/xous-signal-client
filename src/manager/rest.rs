// HTTPS client for the small set of Signal REST endpoints this client
// invokes:
// - `PUT /v1/devices/link` — initial secondary-device link.
// - `PUT /v1/accounts/attributes` — post-link account-attributes refresh
//   (issue #16). The link body already carries an `accountAttributes`
//   sub-object; this separate call updates the canonical account record
//   so the server's per-device and per-account views agree.
//
// All endpoints share the Xous trust store through rustls::ClientConfig
// built by tls::Tls::new().client_config().
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

#[derive(Serialize, Debug)]
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

#[derive(Serialize, Debug)]
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

/// PUT {base_url}/v1/accounts/attributes with Basic auth, sending the
/// canonical AccountAttributes JSON body (issue #16).
///
/// Called after `put_devices_link` succeeds. The link body carries an
/// `accountAttributes` sub-object that updates the device record, but
/// modern Signal-Server treats the per-account record as a separate
/// store; reference clients (signal-cli, libsignal-service-rs,
/// Signal-Android) issue this PUT in addition to refresh the canonical
/// account-level fields.
///
/// Failure is treated by the caller as non-fatal: the link itself
/// succeeded, the message-receive path works, and the attributes can
/// be retried on a future startup.
///
/// Identifier format: `<aci>.<deviceId>` (the new device's auth credentials).
/// Returns Ok(()) on any 2xx response (Signal-Server returns 204 No Content
/// on success).
pub fn put_accounts_attributes(
    base_url: &Url,
    identifier: &str,
    password: &str,
    attrs: &AccountAttributes,
) -> Result<(), Error> {
    let mut url = base_url.clone();
    url.set_path("/v1/accounts/attributes");
    let url_str = url.to_string();

    let client_config = Arc::new(Tls::new().client_config());
    let agent = ureq::AgentBuilder::new()
        .tls_config(client_config)
        .build();

    let auth_value = basic_auth_header(identifier, password);
    log::info!(
        "PUT {url_str} with Basic auth for identifier={identifier} (password redacted)"
    );

    let json = serde_json::to_string(attrs).map_err(|e| {
        log::error!("AccountAttributes serialize failed: {e}");
        Error::new(ErrorKind::Other, "failed to serialize attrs body")
    })?;
    log::info!("PUT /v1/accounts/attributes body len={}", json.len());

    let resp = agent
        .put(&url_str)
        .set("Authorization", &auth_value)
        .set("Content-Type", "application/json")
        .send_bytes(json.as_bytes());

    match resp {
        Ok(r) => {
            let status = r.status();
            log::info!("PUT {url_str} -> {status}");
            Ok(())
        }
        Err(ureq::Error::Status(code, r)) => {
            let body_text = r.into_string().unwrap_or_default();
            let preview: String = body_text.chars().take(200).collect();
            log::error!("PUT {url_str} -> {code}: {preview}");
            Err(Error::new(
                ErrorKind::Other,
                format!("PUT /v1/accounts/attributes returned {code}"),
            ))
        }
        Err(e) => {
            log::error!("PUT {url_str} request failed: {e}");
            Err(Error::new(
                ErrorKind::ConnectionAborted,
                format!("PUT /v1/accounts/attributes request failed: {e}"),
            ))
        }
    }
}

fn basic_auth_header(identifier: &str, password: &str) -> String {
    let raw = format!("{}:{}", identifier, password);
    format!("Basic {}", STANDARD.encode(raw.as_bytes()))
}

// ---- /v2/keys (issue #15: prekey replenishment) ----------------------------

/// Server-reported prekey stock for a single identity (ACI or PNI).
/// Mirrors `org.whispersystems.textsecuregcm.entities.PreKeyCount`.
#[derive(Deserialize, Debug, PartialEq, Eq)]
pub(crate) struct PreKeyCount {
    pub count: u32,
    #[serde(rename = "pqCount")]
    pub pq_count: u32,
}

/// Single one-time EC prekey, as serialized into the `preKeys` array
/// of `PUT /v2/keys`. Matches `ECPreKey` in Signal-Server. One-time
/// EC prekeys are unsigned — the signature lives on `signedPreKey`
/// which we don't rotate here.
#[derive(Serialize, Debug)]
pub(crate) struct OneTimePreKeyEntity {
    #[serde(rename = "keyId")]
    pub key_id: u32,
    #[serde(rename = "publicKey")]
    pub public_key: String,
}

/// Body of `PUT /v2/keys?identity=<aci|pni>`. The server merges into
/// the existing per-identity stock — fields that are `None` / empty
/// are ignored, NOT cleared. This is why we send only `pre_keys` for
/// one-time EC replenishment and leave the signed / pq fields empty.
#[derive(Serialize, Debug, Default)]
pub(crate) struct SetKeysRequest {
    #[serde(rename = "preKeys", skip_serializing_if = "Vec::is_empty")]
    pub pre_keys: Vec<OneTimePreKeyEntity>,
    #[serde(rename = "signedPreKey", skip_serializing_if = "Option::is_none")]
    pub signed_pre_key: Option<SignedPreKeyEntity>,
    #[serde(rename = "pqPreKeys", skip_serializing_if = "Vec::is_empty")]
    pub pq_pre_keys: Vec<KyberPreKeyEntity>,
    #[serde(rename = "pqLastResortPreKey", skip_serializing_if = "Option::is_none")]
    pub pq_last_resort_pre_key: Option<KyberPreKeyEntity>,
}

/// `GET {base_url}/v2/keys?identity=aci` with Basic auth. Returns the
/// server's view of how many one-time prekeys remain in stock for this
/// identity. Used by [`prekey_replenish`] to decide whether to upload.
pub(crate) fn get_keys_status(
    base_url: &Url,
    identifier: &str,
    password: &str,
) -> Result<PreKeyCount, Error> {
    let mut url = base_url.clone();
    url.set_path("/v2/keys");
    url.set_query(Some("identity=aci"));
    let url_str = url.to_string();

    let client_config = Arc::new(Tls::new().client_config());
    let agent = ureq::AgentBuilder::new().tls_config(client_config).build();
    let auth_value = basic_auth_header(identifier, password);

    log::info!("GET {url_str} with Basic auth for identifier={identifier}");
    let resp = agent
        .get(&url_str)
        .set("Authorization", &auth_value)
        .set("Accept", "application/json")
        .call();
    match resp {
        Ok(r) => {
            let status = r.status();
            let body = r
                .into_string()
                .map_err(|e| Error::new(ErrorKind::Other, format!("read body: {e}")))?;
            log::info!("GET {url_str} -> {status}, body len={}", body.len());
            parse_prekey_count(&body)
        }
        Err(ureq::Error::Status(code, r)) => {
            let body_text = r.into_string().unwrap_or_default();
            let preview: String = body_text.chars().take(200).collect();
            log::error!("GET {url_str} -> {code}: {preview}");
            Err(Error::new(
                ErrorKind::Other,
                format!("GET /v2/keys returned {code}"),
            ))
        }
        Err(e) => {
            log::error!("GET {url_str} request failed: {e}");
            Err(Error::new(
                ErrorKind::ConnectionAborted,
                format!("GET /v2/keys request failed: {e}"),
            ))
        }
    }
}

/// `PUT {base_url}/v2/keys?identity=aci` with Basic auth. Body is the
/// `SetKeysRequest` JSON above. Server returns 200 on success.
pub(crate) fn put_keys(
    base_url: &Url,
    identifier: &str,
    password: &str,
    body: &SetKeysRequest,
) -> Result<(), Error> {
    let mut url = base_url.clone();
    url.set_path("/v2/keys");
    url.set_query(Some("identity=aci"));
    let url_str = url.to_string();

    let client_config = Arc::new(Tls::new().client_config());
    let agent = ureq::AgentBuilder::new().tls_config(client_config).build();
    let auth_value = basic_auth_header(identifier, password);

    let json = serde_json::to_string(body).map_err(|e| {
        log::error!("SetKeysRequest serialize failed: {e}");
        Error::new(ErrorKind::Other, "failed to serialize SetKeysRequest")
    })?;
    log::info!(
        "PUT {url_str} body len={} (preKeys={}, pqPreKeys={})",
        json.len(),
        body.pre_keys.len(),
        body.pq_pre_keys.len(),
    );

    let resp = agent
        .put(&url_str)
        .set("Authorization", &auth_value)
        .set("Content-Type", "application/json")
        .send_bytes(json.as_bytes());

    match resp {
        Ok(r) => {
            log::info!("PUT {url_str} -> {}", r.status());
            Ok(())
        }
        Err(ureq::Error::Status(code, r)) => {
            let body_text = r.into_string().unwrap_or_default();
            let preview: String = body_text.chars().take(200).collect();
            log::error!("PUT {url_str} -> {code}: {preview}");
            Err(Error::new(
                ErrorKind::Other,
                format!("PUT /v2/keys returned {code}"),
            ))
        }
        Err(e) => {
            log::error!("PUT {url_str} request failed: {e}");
            Err(Error::new(
                ErrorKind::ConnectionAborted,
                format!("PUT /v2/keys request failed: {e}"),
            ))
        }
    }
}

fn parse_prekey_count(body: &str) -> Result<PreKeyCount, Error> {
    serde_json::from_str::<PreKeyCount>(body)
        .map_err(|e| Error::new(ErrorKind::InvalidData, format!("PreKeyCount parse: {e}")))
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
    fn basic_auth_supports_aci_dot_device_id_format() {
        // The post-link `PUT /v1/accounts/attributes` (issue #16) uses the
        // `<aci>.<deviceId>` identifier (the device's authoritative auth
        // credentials after link returns). Sanity: the same Basic-auth
        // constructor handles this format with no special-casing.
        let header = basic_auth_header(
            "12345678-1234-1234-1234-123456789abc.42",
            "hunter2hunter2hunter2hh",
        );
        let decoded = STANDARD
            .decode(&header["Basic ".len()..])
            .expect("valid base64");
        let decoded_str = std::str::from_utf8(&decoded).expect("utf-8");
        assert_eq!(
            decoded_str,
            "12345678-1234-1234-1234-123456789abc.42:hunter2hunter2hunter2hh"
        );
    }

    #[test]
    fn account_attributes_clone_preserves_field_set() {
        // AccountAttributes derives Clone (issue #16) so the link flow can
        // pass one copy to the link body and keep another for the post-link
        // PUT /v1/accounts/attributes call. Verify Clone is structurally
        // sound: serializing both copies produces byte-identical JSON.
        use crate::manager::account_attrs::build_account_attributes;
        let attrs = build_account_attributes(
            "name".to_string(),
            &[0u8; 32],
            42,
            43,
        )
        .expect("attrs");
        let json_a = serde_json::to_string(&attrs).expect("serialize a");
        let json_b = serde_json::to_string(&attrs.clone()).expect("serialize b");
        assert_eq!(json_a, json_b);
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

    // ---- /v2/keys (issue #15) -------------------------------------------

    #[test]
    fn prekey_count_parses_camel_case() {
        let body = r#"{"count":42,"pqCount":7}"#;
        let parsed = parse_prekey_count(body).expect("parse");
        assert_eq!(parsed, PreKeyCount { count: 42, pq_count: 7 });
    }

    #[test]
    fn prekey_count_rejects_bad_shape() {
        assert!(parse_prekey_count("not json").is_err());
        // Missing fields should fail — both are required.
        assert!(parse_prekey_count("{\"count\":1}").is_err());
    }

    #[test]
    fn set_keys_request_with_only_pre_keys_omits_other_fields() {
        let body = SetKeysRequest {
            pre_keys: vec![
                OneTimePreKeyEntity {
                    key_id: 100,
                    public_key: "AAA".to_string(),
                },
                OneTimePreKeyEntity {
                    key_id: 101,
                    public_key: "BBB".to_string(),
                },
            ],
            ..SetKeysRequest::default()
        };
        let json = serde_json::to_value(&body).expect("ser");
        // preKeys present, others absent (skip_serializing_if = empty/none).
        assert!(json.get("preKeys").is_some());
        assert_eq!(json["preKeys"].as_array().unwrap().len(), 2);
        assert_eq!(json["preKeys"][0]["keyId"], 100);
        assert_eq!(json["preKeys"][0]["publicKey"], "AAA");
        // signedPreKey, pqPreKeys, pqLastResortPreKey must be absent (the
        // server does merge-not-replace, so omitting preserves existing
        // signed/Kyber stock; sending null would be different semantics).
        assert!(json.get("signedPreKey").is_none(), "signedPreKey leaked");
        assert!(json.get("pqPreKeys").is_none(), "pqPreKeys leaked");
        assert!(json.get("pqLastResortPreKey").is_none(), "pqLastResortPreKey leaked");
    }

    #[test]
    fn set_keys_request_empty_serializes_to_object() {
        // Edge case — an empty body should still serialize, with all fields
        // skipped. The server would no-op on a fully-empty PUT, but we
        // guard the call site against this so it's not a wire concern.
        let body = SetKeysRequest::default();
        let json = serde_json::to_string(&body).expect("ser");
        assert_eq!(json, "{}");
    }
}

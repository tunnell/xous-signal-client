//! Renode emulation testing scaffold.
//!
//! Compiled only when the `renode-test` feature is enabled.
//!
//! Two pieces of infrastructure live here:
//!
//! 1. `RENODE_TEST_HOST` — IP address of the TAP-host gateway
//!    where the stand-in WebSocket server listens. Sigchat's
//!    `signal_config()` reads this when the feature is on.
//! 2. `unverified_tls_stream()` — opens a TLS connection without
//!    server cert validation. Used by `signal_ws::connect()` to
//!    accept the self-signed cert that the stand-in serves.
//!
//! Both are EXPLICITLY UNSAFE for production. The feature flag is
//! `renode-test`; do not enable in default builds.
//!
//! Companion stand-in:
//! `xous-signal-client-notes/renode-test/mock/signal_mock.py`.
//! Companion harness writeup:
//! `xous-signal-client-notes/_open-followups/2026-05-02-renode-signal-test-setup.md`.

use std::convert::TryFrom;
use std::io::{Error, ErrorKind};
use std::net::TcpStream;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};

/// The TAP-host gateway IP the renode-test harness binds the stand-in
/// server to. Matches the default `--host` in `signal_mock.py`.
pub const RENODE_TEST_HOST: &str = "192.168.100.1";

/// Port the stand-in server listens on. Matches `signal_mock.py`'s
/// default (`--port 8443`). Production sigchat connects on 443 (the
/// hard-coded port in `signal_ws::connect`); renode-test redirects
/// to the unprivileged 8443 because the stand-in runs as a normal
/// user and can't bind to the well-known port without CAP_NET_BIND_SERVICE.
pub const RENODE_TEST_PORT: u16 = 8443;

/// NoOp TLS server-cert verifier — accepts ANY presented certificate,
/// any name, any signature. Trivially insecure. Compiled into the
/// binary only when the `renode-test` feature is enabled, which is
/// gated to non-production builds.
#[derive(Debug)]
struct NoVerification;

impl ServerCertVerifier for NoVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        log::warn!(
            "renode-test: skipping TLS server cert verification (NoVerification verifier active). \
             This is INSECURE and only valid for emulation testing against signal_mock.py."
        );
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
        ]
    }
}

/// Open a TLS stream to `host` over `sock`, skipping cert verification.
/// Mirrors `tls::Tls::stream_owned()` but without the WebPki/CA chain
/// validation that the production path enforces.
pub fn unverified_tls_stream(
    host: &str,
    sock: TcpStream,
) -> Result<StreamOwned<ClientConnection, TcpStream>, Error> {
    let server_name: ServerName<'static> = ServerName::try_from(host.to_owned())
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("invalid server name {host}: {e}")))?;
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerification))
        .with_no_client_auth();
    let conn = ClientConnection::new(Arc::new(config), server_name).map_err(|e| {
        Error::new(ErrorKind::Other, format!("rustls ClientConnection::new failed: {e}"))
    })?;
    Ok(StreamOwned::new(conn, sock))
}

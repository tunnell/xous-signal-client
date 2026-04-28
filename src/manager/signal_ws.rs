use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use rustls::{ClientConnection, StreamOwned};
use std::io::{self, Error, ErrorKind};
use std::net::TcpStream;
use std::time::Duration;
use tls::Tls;
use tungstenite::{Message, WebSocket};
use url::Url;

const PROVISIONING_PATH: [&str; 4] = ["v1", "websocket", "provisioning", ""];
const REGISTRATION_PATH: [&str; 3] = ["v1", "registration", ""];

#[allow(dead_code)]
pub struct SignalWS {
    ws: WebSocket<StreamOwned<ClientConnection, TcpStream>>,
}

impl SignalWS {
    pub fn new_message(
        host: &str,
        aci_service_id: &str,
        device_id: u32,
        password: &str,
    ) -> Result<Self, Error> {
        let url = Url::parse(&format!("wss://{}/v1/websocket/", host))
            .map_err(|_| Error::new(ErrorKind::InvalidInput, "invalid host for ws url"))?;
        // Signal-Server's WebSocketAccountAuthenticator reads only the
        // Authorization header; query-param credentials are silently ignored,
        // leaving the connection unauthenticated and the message-delivery
        // subscription never created. Reference: WebSocketAccountAuthenticator.java:33-46.
        let login = format!("{}.{}", aci_service_id, device_id);
        let auth = BASE64.encode(format!("{}:{}", login, password));
        Self::new_with_auth(&url, &auth)
    }

    fn new_with_auth(url: &Url, auth: &str) -> Result<Self, Error> {
        let ws = SignalWS::connect(url, Some(auth))?;
        Ok(Self { ws })
    }

    fn new(url: &Url) -> Result<Self, Error> {
        let ws = SignalWS::connect(url, None)?;
        Ok(Self { ws })
    }

    pub fn new_provision(url: &mut Url) -> Result<Self, Error> {
        url.set_scheme("wss").expect("failed to set scheme");
        url.path_segments_mut().expect("failed to add path").extend(&PROVISIONING_PATH);
        Self::new(url)
    }

    #[allow(dead_code)]
    pub fn new_register(url: &mut Url) -> Result<Self, Error> {
        url.set_scheme("wss").expect("failed to set scheme");
        url.path_segments_mut().expect("failed to add path").extend(&REGISTRATION_PATH);
        Self::new(url)
    }

    /// Set (or clear) a read timeout on the underlying TCP stream. tungstenite
    /// reads flow through the rustls ClientConnection down to this socket, so a
    /// timeout set here propagates as `io::ErrorKind::WouldBlock` or
    /// `io::ErrorKind::TimedOut` from `ws.read()`. If the stack does not
    /// support it (e.g. an unusual socket type on a given target), the
    /// underlying `TcpStream` returns an `io::Error` which is surfaced
    /// verbatim — the caller decides whether to treat that as fatal.
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.ws.get_ref().sock.set_read_timeout(timeout)
    }

    /// Convenience method: install a one-shot read timeout, perform a single
    /// read, restore blocking mode. Intended for the main thread's initial
    /// UUID read where the worker has not spawned yet. tungstenite errors
    /// are mapped to `io::Error` (`TimedOut` when the timeout fires).
    pub fn read_once(&mut self, timeout: Duration) -> io::Result<Message> {
        self.set_read_timeout(Some(timeout))?;
        let result = match self.ws.read() {
            Ok(msg) => Ok(msg),
            Err(tungstenite::Error::Io(io_err))
                if matches!(io_err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                Err(Error::new(ErrorKind::TimedOut, "ws read_once timeout"))
            }
            Err(e) => {
                log::warn!("ws read_once: {e}");
                Err(Error::other("ws read error"))
            }
        };
        let _ = self.set_read_timeout(None);
        result
    }

    /// Raw `tungstenite::WebSocket::read` passthrough. Intended for the
    /// worker thread's interleaved loop, which needs to distinguish
    /// `Error::Io(WouldBlock|TimedOut)` (normal cycle) from real errors.
    // tungstenite::Error is large (200+ bytes) but we need the raw type so
    // the worker can pattern-match on it; boxing would just shift the cost.
    #[allow(clippy::result_large_err)]
    pub fn read(&mut self) -> Result<Message, tungstenite::Error> {
        self.ws.read()
    }

    /// Raw `tungstenite::WebSocket::send` passthrough. Used by the worker's
    /// keepalive Pings.
    #[allow(clippy::result_large_err)]
    pub fn send(&mut self, msg: Message) -> Result<(), tungstenite::Error> {
        self.ws.send(msg)
    }

    /// Best-effort close handshake. Consumes `self` so the WebSocket and its
    /// TLS state are dropped when this returns — matching the shutdown
    /// semantics in `ws_server.rs` (worker exits its loop then closes).
    pub fn close(mut self) {
        log::info!("attempting to close websocket connection");
        let _ = self.ws.close(None);
        loop {
            match self.ws.flush() {
                Ok(()) => (),
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    log::info!("websocket connection closed");
                    break;
                }
                Err(e) => {
                    log::warn!("ws close flush: {e}");
                    break;
                }
            }
        }
    }

    fn connect(
        url: &Url,
        auth: Option<&str>,
    ) -> Result<WebSocket<StreamOwned<ClientConnection, TcpStream>>, Error> {
        log::info!("attempting websocket connection to {}", url.as_str());
        let host = url.host_str().expect("failed to extract host from url");
        let sock = TcpStream::connect((host, 443))?;
        log::info!("tcp connected to {host}");
        let xtls = Tls::new();
        let tls_stream = xtls.stream_owned(host, sock)?;
        log::info!("tls configured");
        let request = build_ws_upgrade_request(url, auth)?;
        match tungstenite::client(request, tls_stream) {
            Ok((socket, response)) => {
                log::info!("Websocket connected to: {}", url.as_str());
                log::info!("Response HTTP code: {}", response.status());
                Ok(socket)
            }
            Err(e) => {
                log::info!("failed to connect websocket: {}", e);
                Err(Error::from(ErrorKind::ConnectionRefused))
            }
        }
    }
}

/// Build the HTTP/1.1 upgrade request used to open a WebSocket to
/// Signal-Server. Pure function (no I/O), extracted from `connect` so the
/// header set can be unit-tested.
///
/// `X-Signal-Receive-Stories` is set to `"false"` because xous-signal-client
/// has no Story-rendering UI; asking the server for Story envelopes wastes
/// bandwidth and decryption cycles on envelopes we'd silently drop.
/// (Issue #18.)
fn build_ws_upgrade_request(
    url: &Url,
    auth: Option<&str>,
) -> Result<tungstenite::http::Request<()>, Error> {
    let host = url
        .host_str()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "url missing host"))?;
    let mut builder = tungstenite::http::Request::builder()
        .method("GET")
        .uri(url.as_str())
        .header("Host", host)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tungstenite::handshake::client::generate_key(),
        )
        .header("X-Signal-Receive-Stories", "false");
    if let Some(credentials) = auth {
        builder = builder.header("Authorization", format!("Basic {}", credentials));
    }
    builder
        .body(())
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("build ws upgrade req: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_test_url() -> Url {
        Url::parse("wss://chat.signal.org/v1/websocket/").unwrap()
    }

    #[test]
    fn upgrade_request_does_not_request_story_envelopes() {
        // Signal-Server uses X-Signal-Receive-Stories on the WS handshake to
        // decide whether to deliver Story envelopes. We set it to "false"
        // because we have no Story UI; setting it to "true" causes the server
        // to deliver envelopes we silently drop. Issue #18.
        let req = build_ws_upgrade_request(&parse_test_url(), None)
            .expect("request build should succeed");
        let header = req
            .headers()
            .get("X-Signal-Receive-Stories")
            .expect("X-Signal-Receive-Stories header should be present");
        assert_eq!(header.to_str().unwrap(), "false");
    }

    #[test]
    fn upgrade_request_sets_websocket_upgrade_headers() {
        // Sanity-check the bundle of WS-upgrade headers tungstenite needs.
        // Catches accidental drops of one of these in future refactors.
        let req = build_ws_upgrade_request(&parse_test_url(), None)
            .expect("request build should succeed");
        let headers = req.headers();
        assert_eq!(headers.get("Connection").unwrap(), "Upgrade");
        assert_eq!(headers.get("Upgrade").unwrap(), "websocket");
        assert_eq!(headers.get("Sec-WebSocket-Version").unwrap(), "13");
        assert!(headers.get("Sec-WebSocket-Key").is_some());
        assert_eq!(headers.get("Host").unwrap(), "chat.signal.org");
    }

    #[test]
    fn upgrade_request_omits_authorization_when_unauthenticated() {
        // Provisioning websockets connect without an Authorization header;
        // only authenticated chat websockets have one.
        let req = build_ws_upgrade_request(&parse_test_url(), None)
            .expect("request build should succeed");
        assert!(req.headers().get("Authorization").is_none());
    }

    #[test]
    fn upgrade_request_includes_basic_auth_when_credentials_provided() {
        let creds = "user:pass-base64-already";
        let req = build_ws_upgrade_request(&parse_test_url(), Some(creds))
            .expect("request build should succeed");
        let auth = req
            .headers()
            .get("Authorization")
            .expect("Authorization header should be present when creds supplied")
            .to_str()
            .unwrap();
        assert_eq!(auth, format!("Basic {}", creds));
    }
}

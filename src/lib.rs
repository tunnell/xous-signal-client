mod account;
pub mod api;
pub mod manager;
#[cfg(target_os = "xous")]
mod getrandom_xous;

use crate::account::{Account, ServiceEnvironment};
use crate::manager::{Config, Manager, TrustMode};
pub use api::*;
use chat::Chat;
use locales::t;
use modals::Modals;
use std::io::{Error, ErrorKind};
use tls::Tls;
use url::{Host, Url};

/// PDDB Dict for sigchat keys
const SIGCHAT_ACCOUNT: &str = "sigchat.account";
const SIGCHAT_DIALOGUE: &str = "sigchat.dialogue";

const WIFI_TIMEOUT: u32 = 10; // seconds

#[cfg(not(target_os = "xous"))]
pub const HOSTED_MODE: bool = true;
#[cfg(target_os = "xous")]
pub const HOSTED_MODE: bool = false;

fn signal_config() -> Config {
    Config::new(
        Host::parse("signal.org").expect("hardcoded host is valid"),
        ServiceEnvironment::Live,
    )
}

//#[derive(Debug)]
pub struct SigChat<'a> {
    chat: &'a Chat,
    manager: Option<Manager>,
    netmgr: net::NetManager,
    modals: Modals,
}
impl<'a> SigChat<'a> {
    pub fn new(chat: &Chat) -> SigChat<'_> {
        let xns = xous_names::XousNames::new().unwrap();
        let modals = Modals::new(&xns).expect("can't connect to Modals server");
        let pddb = pddb::Pddb::new();
        pddb.try_mount();
        SigChat {
            chat: chat,
            manager: match Account::read(SIGCHAT_ACCOUNT) {
                Ok(account) if account.is_registered() => Some(Manager::new(account, TrustMode::OnFirstUse)),
                _ => None,
            },
            netmgr: net::NetManager::new(),
            modals: modals,
        }
    }

    /// Returns true if a registered account is already loaded and ready to connect.
    pub fn is_ready(&self) -> bool {
        self.manager.is_some()
    }

    /// Connect to the Signal servers
    ///
    /// The process first waits for an active WiFi connection, and then
    /// initiates a Signal Account Manager with the Signal Account struct stored
    /// in the pddb (or kicks off the Account setup process otherwise). The
    /// Account Manager orchestrates the interaction with the Signal host server.
    ///
    /// # Returns
    /// true on a successful connection to a Signal Account/Server
    ///
    pub fn connect(&mut self) -> Result<bool, Error> {
        log::info!("Attempting connect to Signal server");
        if self.wifi() {
            if self.manager.is_none() {
                log::info!("Setting up Signal Account Manager");
                let account = match Account::read(SIGCHAT_ACCOUNT) {
                    Ok(account) if account.is_registered() => account,
                    _ => match self.account_setup() {
                        Ok(a) => a,
                        Err(e) => {
                            self.chat.set_status_text(t!("sigchat.status.offline", locales::LANG));
                            return Err(e);
                        }
                    },
                };
                self.chat
                    .set_status_text(t!("sigchat.status.connecting", locales::LANG));
                self.chat.set_busy_state(true);
                self.manager = Some(Manager::new(account, TrustMode::OnFirstUse));
                self.chat.set_busy_state(false);
            }
            if self.manager.is_some() {
                log::info!("Signal Account Manager OK");
                self.chat
                    .set_status_text(t!("sigchat.status.online", locales::LANG));
                Ok(true)
            } else {
                log::info!("failed to setup Signal Account Manager");
                self.chat
                    .set_status_text(t!("sigchat.status.offline", locales::LANG));
                Ok(false)
            }
        } else {
            self.modals
                .show_notification(t!("sigchat.wifi.warning", locales::LANG), None)
                .expect("notification failed");
            Ok(false)
        }
    }

    /// Setup a Signal Account via Registration or Linking,
    /// or abort setup and read existing chat Dialogues in pddb
    ///
    /// The user can choose to:
    /// 1. Link to an existing Signal Account
    /// 2. Register a new Signal Account
    /// 3. Abort account setup to read existing Signal Dialogues stored in the pddb
    /// The online options involve nominating the Signal host server,
    /// and probing the host for trusted tlls Certificate Authorities.
    ///
    /// # Returns
    /// Account struct stored in pddb
    ///
    fn account_setup(&mut self) -> Result<Account, Error> {
        log::info!("Attempting to setup a Signal Account");
        self.modals
            .add_list_item(t!("sigchat.account.link", locales::LANG))
            .expect("failed add list item");
        self.modals
            .add_list_item(t!("sigchat.account.register", locales::LANG))
            .expect("failed add list item");
        self.modals
            .add_list_item(t!("sigchat.account.offline", locales::LANG))
            .expect("failed add list item");
        self.modals
            .get_radiobutton(t!("sigchat.account.title", locales::LANG))
            .expect("failed radiobutton modal");
        match self.modals.get_radio_index() {
            Ok(index) => match index {
                0 => {
                    let config = signal_config();
                    match self.probe_host(config.url()) {
                        true => Ok(self.account_link(&config)?),
                        false => Err(Error::new(
                            ErrorKind::Other,
                            "failed to trust host certificate",
                        )),
                    }
                }
                1 => {
                    let config = signal_config();
                    match self.probe_host(config.url()) {
                        true => Ok(self.account_register(&config)?),
                        false => Err(Error::new(
                            ErrorKind::Other,
                            "failed to trust host certificate",
                        )),
                    }
                }
                2 => {
                    log::info!("account setup aborted");
                    Err(Error::new(ErrorKind::Other, "account setup aborted"))
                }
                _ => {
                    log::warn!("invalid index");
                    Err(Error::new(ErrorKind::Other, "invalid radio index"))
                }
            },
            Err(e) => {
                log::warn!("failed to present account setup radio buttons: {:?}", e);
                Err(Error::new(
                    ErrorKind::Other,
                    "failed to present account setup radio buttons",
                ))
            }
        }
    }

    /// Probe host for tls Certificate Authority chain of trust
    ///
    /// # Arguments
    /// * `host` - the dns name or ip address of a Signal server
    ///
    /// # Returns
    /// true if at least 1 Certificate Authority is trusted
    ///
    fn probe_host(&self, url: &Url) -> bool {
        self.chat
            .set_status_text(t!("sigchat.status.probing", locales::LANG));
        self.chat.set_busy_state(true);
        let tls = Tls::new();
        if tls.accessible(url.host_str().unwrap(), true) {
            self.chat.set_busy_state(false);
            true
        } else {
            self.modals
                .show_notification(t!("sigchat.account.abort", locales::LANG), None)
                .expect("abort failed");
            self.chat.set_busy_state(false);
            false
        }
    }

    /// Link this device to an existing Signal Account
    ///
    /// Signal allows to link additional devices to your primary device (e.g. Signal-Android).
    /// Note that currently Signal allows up to three linked devices per primary.
    ///
    /// The user must provide a name for the current device before the Link process
    /// commences - culminating by presenting a qr-code to be scanned by the primary device.
    ///
    /// # Arguments
    /// * `config` - Signal configuration - host server and Live/Staging environment
    ///
    /// # Returns
    /// Account struct stored in pddb
    ///
    pub fn account_link(&mut self, config: &Config) -> Result<Account, Error> {
        log::info!("Attempting to Link to an existing Signal Account");
        self.chat
            .set_status_text(t!("sigchat.status.initializing", locales::LANG));
        self.chat.set_busy_state(true);
        match Account::new(SIGCHAT_ACCOUNT, config.host(), config.service_environment()) {
            Ok(account) => {
                let mut manager = Manager::new(account, TrustMode::OnFirstUse);
                let name = self.name_modal(
                    "xous",
                    t!("sigchat.account.link.name", locales::LANG),
                );
                self.chat
                    .set_status_text(t!("sigchat.status.connecting", locales::LANG));
                self.chat.set_busy_state(true);
                match manager.link(&name) {
                    Ok(true) => {
                        log::info!("Linked Signal Account");
                        self.chat.set_busy_state(false);
                        Ok(Account::read(SIGCHAT_ACCOUNT)?)
                    }
                    Ok(false) => {
                        log::info!("failed to link Signal Account");
                        self.chat.set_busy_state(false);
                        Err(Error::new(
                            ErrorKind::Other,
                            "failed to link Signal Account",
                        ))
                    }
                    Err(e) => {
                        log::warn!("error while linking Signal Account: {e}");
                        Account::delete(SIGCHAT_ACCOUNT).unwrap_or_else(|e| {
                            log::warn!("failed to delete unregistered account from pddb: {e}")
                        });
                        self.chat.set_busy_state(false);
                        self.modals
                            .show_notification(&format!("{}", e), None)
                            .expect("notification failed");
                        Err(Error::new(
                            ErrorKind::Other,
                            "error while linking Signal Account",
                        ))
                    }
                }
            }
            Err(e) => {
                log::warn!(
                    "failed to create new Account in pddb:{} : {e}",
                    SIGCHAT_ACCOUNT
                );
                self.modals
                    .show_notification(t!("sigchat.account.failed", locales::LANG), None)
                    .expect("notification failed");
                Err(Error::new(
                    ErrorKind::Other,
                    "failed to create new Account in pddb",
                ))
            }
        }
    }

    /// Prompt a name from the user
    ///
    /// # Arguments
    /// * `default_name` - A name suggested to the user
    /// * `prompt` - A prompt to explain what is being requested
    ///
    /// # Returns
    /// the name provided by the user
    ///
    fn name_modal(&self, default_name: &str, prompt: &str) -> String {
        match self
            .modals
            .alert_builder(prompt)
            .field(Some(default_name.to_string()), None)
            .build()
        {
            Ok(text) => text.content()[0].content.to_string(),
            _ => default_name.to_string(),
        }
    }

    /// Register a new Signal Account with this as the primary device.
    ///
    /// A Signal Account requires a phone number to receive SMS or incoming calls for registration & validation.
    /// The phone number must include the country calling code, i.e. the number must start with a "+" sign.
    /// Warning: this will disable the authentication of your phone as a primary device.
    ///
    /// # Arguments
    ///
    /// * `config` - Signal configuration - host server and Live/Staging environment
    ///
    pub fn account_register(&mut self, config: &Config) -> Result<Account, Error> {
        log::info!("Attempting to Register a new Signal Account");
        self.modals
            .show_notification(&"sorry - registration is not implemented yet", None)
            .expect("notification failed");
        match self.number_modal() {
            Ok(number) => {
                log::info!("registration phone number = {:?}", number);
                match Account::new(SIGCHAT_ACCOUNT, config.host(), config.service_environment()) {
                    Ok(mut account) => match account.set_number(&number.to_string()) {
                        Ok(_number) => {
                            self.manager = Some(Manager::new(account, TrustMode::OnFirstUse));
                        }
                        Err(e) => log::warn!("failed to set Account number: {e}"),
                    },
                    Err(e) => log::warn!("failed to create new Account: {e}"),
                }
            }
            Err(e) => log::warn!("failed to get phone number: {e}"),
        }
        Err(Error::new(ErrorKind::Other, "not implmented"))
    }

    /// Attempts to obtain a phone number from the user.
    ///
    /// A Signal Account requires a phone number to receive SMS or incoming calls for registration & validation.
    /// The phone number must include the country calling code, i.e. the number must start with a "+" sign.
    ///
    #[allow(dead_code)]
    fn number_modal(&mut self) -> Result<String, Error> {
        let mut builder = self
            .modals
            .alert_builder(t!("sigchat.number.title", locales::LANG));
        let builder = builder.field(Some(t!("sigchat.number", locales::LANG).to_string()), None);
        match builder.build() {
            Ok(payloads) => {
                let number = payloads.content()[0].content.to_string();
                log::info!("registration phone number = {:?}", number);
                Ok(number)
            }
            Err(_) => Err(Error::from(ErrorKind::ConnectionRefused)),
        }
    }

    /// Spawn the authenticated receive worker and wire it to the Chat UI.
    ///
    /// Must be called after a successful `connect()`. `chat_cid` is obtained
    /// from `chat.cid()` in the main binary; it is `Copy` (u32 alias) so it
    /// can be passed freely between threads.
    pub fn start_receive(&self, chat_cid: xous::CID) -> Result<bool, Error> {
        match &self.manager {
            Some(mgr) => mgr
                .start_receive(chat_cid)
                .map(|_| true)
                .map_err(|e| Error::new(ErrorKind::Other, format!("start_receive: {e}"))),
            None => {
                log::warn!("start_receive called before connect — no manager");
                Ok(false)
            }
        }
    }

    pub fn redraw(&self) {
        self.chat.redraw();
    }

    pub fn dialogue_set(&self, room_alias: Option<&str>) {
        self.chat
            .dialogue_set(SIGCHAT_DIALOGUE, room_alias)
            .expect("failed to set dialogue");
    }

    pub fn post(&self, text: &str) {
        self.chat.set_busy_state(true);
        self.chat.set_status_text("sending...");
        log::info!("post: '{}'", text);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // Local echo first — Signal's WS does not push back the sender's own
        // messages, so the local store is the only place "me" appears.
        chat::cf_post_add(self.chat.cid(), "me", ts, text);
        chat::cf_redraw(self.chat.cid());

        // Phase 2b: encrypt + submit to /v1/messages/{uuid} with bounded
        // retry on 409 (mismatched devices) and 410 (stale devices). Local
        // echo above stays in the UI regardless of send outcome — there is
        // no "failed to send" UI marker yet.
        match crate::manager::outgoing::current_recipient() {
            Ok(recipient) => {
                let mut http = crate::manager::send::UreqHttpClient::new();
                match crate::manager::send::submit_with_retry(text, ts, &recipient, &mut http) {
                    Ok(()) => {
                        log::info!("post: sent to {}", recipient.name());
                    }
                    Err(e) => {
                        log::warn!("post: send failed for {}: {e}", recipient.name());
                    }
                }
            }
            Err(e) => {
                log::info!("post: no outgoing recipient ({e}); local-echo only");
            }
        }

        self.chat.set_busy_state(false);
    }

    pub fn help(&self) {
        self.chat.help();
    }

    /// Returns true if wifi is connected
    ///
    /// If wifi is not connected then a modal offers to "Connect to wifi?"
    /// and tries for 10 seconds before representing.
    ///
    /// # Returns
    /// true when wifi is connected
    ///
    pub fn wifi(&self) -> bool {
        if HOSTED_MODE {
            return true;
        }

        if let Some(conf) = self.netmgr.get_ipv4_config() {
            if conf.dhcp == com_rs::DhcpState::Bound {
                return true;
            }
        }

        while self.wifi_try_modal() {
            self.netmgr.connection_manager_wifi_on_and_run().unwrap();
            self.modals
                .start_progress("Connecting ...", 0, 10, 0)
                .expect("no progress bar");
            let tt = ticktimer_server::Ticktimer::new().unwrap();
            for wait in 0..WIFI_TIMEOUT {
                tt.sleep_ms(1000).unwrap();
                self.modals
                    .update_progress(wait)
                    .expect("no progress update");
                if let Some(conf) = self.netmgr.get_ipv4_config() {
                    if conf.dhcp == com_rs::DhcpState::Bound {
                        self.modals
                            .finish_progress()
                            .expect("failed progress finish");
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Returns true if "Connect to WiFi?" yes option is chosen
    ///
    fn wifi_try_modal(&self) -> bool {
        self.modals.add_list_item("yes").expect("failed radio yes");
        self.modals.add_list_item("no").expect("failed radio no");
        self.modals
            .get_radiobutton("Connect to WiFi?")
            .expect("failed radiobutton modal");
        match self.modals.get_radio_index() {
            Ok(button) => button == 0,
            _ => false,
        }
    }
}

//! The UwUMail Gateway in the portal: how the tunnel is doing, pairing with a code and forgetting
//! the gateway. The server runs the tunnel and plugs itself in with [`crate::Web::set_gateway`].

use std::future::Future;
use std::pin::Pin;

use serde::Serialize;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GatewayState {
    /// No gateway: mail leaves from this server.
    #[default]
    None,
    Connecting,
    Connected,
    /// The gateway does not accept this server.
    Refused,
}

/// What the portal shows about the gateway.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayView {
    pub state: GatewayState,
    /// Where the tunnel goes.
    pub tunnel: Vec<String>,
    /// SHA-256 of the gateway's certificate.
    pub fingerprint: Option<String>,
    /// The gateway's public addresses: where the host name has to point.
    pub addresses: Vec<String>,
    pub services: Vec<String>,
    pub outbound_ports: Vec<u16>,
    pub software: Option<String>,
    /// Unix time the tunnel came up, while it is up.
    pub connected_since: Option<i64>,
    /// Unix time the tunnel went down (or pairing started), while it is down.
    pub down_since: Option<i64>,
    /// Why the last attempt failed.
    pub error: Option<String>,
    /// Why the gateway refused: `notPaired`, `wrongToken`, `otherServer` or `version`.
    pub refusal: Option<String>,
    /// The pairing comes from `gateway.code` in the configuration.
    pub from_config: bool,
    /// What the gateway says about the machine it runs on: its updates, its firewall, its bans.
    /// `None` while the tunnel is down, and for gateways from before they told us.
    pub machine: Option<GatewayMachine>,
    /// Whether the portal can ask this gateway to update or restart its machine. False for a
    /// gateway without a helper beside it, which is also the one that shows the commands instead.
    pub can_install: bool,
    /// The newest gateway there is to install, when the server knows of one.
    pub software_version: Option<String>,
}

/// The gateway's machine, as the portal shows it. The server fills this in from what comes through
/// the tunnel; the portal never learns that a tunnel is what carries it.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayMachine {
    pub system: Option<GatewaySystem>,
    pub protection: Option<GatewayProtection>,
    /// The addresses the gateway keeps out of every ban list, this server's among them.
    pub trusted: Vec<String>,
    /// Unix time the gateway last looked at its machine.
    pub checked_at: i64,
    /// The job the portal asked for last, while there is one.
    pub job: Option<GatewayJob>,
}

/// A job the gateway's machine is carrying out, or has.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayJob {
    pub id: String,
    /// `running`, `done`, `failed` or `refused`.
    pub state: String,
    pub error: String,
    pub at: i64,
    /// What it has printed so far, capped.
    pub log: String,
}

/// The operating system on the gateway's machine and what it waits for.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySystem {
    /// As the machine calls itself, for example `Ubuntu 26.04.1 LTS`.
    pub name: String,
    pub updates: u32,
    pub security_updates: u32,
    pub reboot_required: bool,
    /// Whether security updates install themselves.
    pub automatic_security: bool,
    /// A newer release of the operating system, when one waits.
    pub new_release: Option<String>,
    /// What to run on the gateway to install the updates.
    pub command: String,
}

/// What keeps the gateway's machine itself safe.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayProtection {
    /// The firewall in use, for example `ufw`; empty when none was found.
    pub firewall: String,
    pub firewall_active: bool,
    pub fail2ban: bool,
    /// Addresses fail2ban keeps out right now, over all jails.
    pub banned: u32,
    pub jails: Vec<String>,
    /// Of those, the ones this server asked for.
    pub from_server: u32,
}

pub type GatewayFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// The server's tunnel, as the portal may use it.
pub trait GatewayBackend: Send + Sync + 'static {
    fn view(&self) -> GatewayView;
    /// Pairs with the gateway of `code` instead of any earlier one. Returns once the pairing is
    /// saved; [`GatewayBackend::view`] then follows the connection.
    fn pair<'a>(&'a self, code: &'a str) -> GatewayFuture<'a>;
    /// Disconnects and forgets the gateway; mail leaves from this server again.
    fn forget(&self) -> GatewayFuture<'_>;
    /// Asks the gateway's machine for `os-update`, `reboot` or `gateway-update`; the version is
    /// only for the last one. Returns the id of the job, to follow it with [`GatewayBackend::view`].
    ///
    /// Fails when the tunnel is down or the gateway is too old to listen. It has to: an ask that
    /// went nowhere would otherwise leave the portal waiting for an answer nobody is writing.
    fn ask<'a>(
        &'a self,
        verb: &'a str,
        version: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;
}

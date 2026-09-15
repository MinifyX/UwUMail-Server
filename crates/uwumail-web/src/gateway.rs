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
}

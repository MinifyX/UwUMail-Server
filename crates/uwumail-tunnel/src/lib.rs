//! The tunnel between a UwUMail server and its UwUMail Gateway.
//!
//! The server at home dials out to the gateway, a small machine with a fixed public address,
//! over QUIC. Both sides present a self-signed certificate and pin the other one's fingerprint,
//! learned once through a pairing code. Over that one connection:
//!
//! - the gateway opens a stream for every connection that arrives on its public ports and starts
//!   it with an [`Open`] header that names the service and the real client address;
//! - the server opens a stream with a [`Connect`] request for every connection it makes to other
//!   mail servers, so they see the gateway's address and never the one at home;
//! - the first stream the server opens is the control stream with [`Hello`] and the gateway's
//!   answer.
//!
//! The gateway only passes bytes along: TLS of mail apps, browsers and other mail servers ends
//! at home, so the gateway never sees passwords or the content of encrypted connections.

mod client;
pub mod code;
mod identity;
pub mod net;
pub mod proto;
mod quic;
mod stream;
mod verify;

pub use client::{ClientSettings, Inbound, Status, TunnelClient};
pub use code::{CodeError, PairingCode, Token};
pub use identity::{Fingerprint, Identity};
pub use proto::{Connect, ConnectFailure, ConnectReply, Hello, HelloReply, Open, Refusal, Service, Welcome};
pub use quic::{client_config, peer_fingerprint, server_endpoint};
pub use stream::TunnelStream;

#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("TLS: {0}")]
    Tls(#[from] rustls::Error),
    #[error("certificate: {0}")]
    Certificate(#[from] rcgen::Error),
    #[error("QUIC: {0}")]
    Quic(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

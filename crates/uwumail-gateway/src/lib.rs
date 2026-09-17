//! UwUMail Gateway: a small machine with a fixed public address in front of a UwUMail server at
//! home. It takes mail, mail apps and browsers on its public ports and carries every connection
//! through the tunnel to the server, and it makes the server's connections to other mail servers,
//! so the internet only ever sees the gateway's address. It stores no mail: while the server is
//! away, it asks senders to try again later.

pub mod config;
mod gateway;
mod limits;
pub mod machine;
mod outbound;
mod pipe;
mod public;
pub mod state;

pub use config::GatewayConfig;
pub use gateway::{Running, pairing_code, start};

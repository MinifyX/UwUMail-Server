//! Connections the server asks the gateway to make: mail to other servers, sent from the
//! gateway's address.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::timeout;
use uwumail_tunnel::TunnelStream;
use uwumail_tunnel::proto::{self, Connect, ConnectFailure, ConnectReply};

use crate::gateway::Shared;

const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONNECT_SECS: u32 = 120;

pub(crate) async fn handle(shared: Arc<Shared>, mut stream: TunnelStream) {
    let request: Connect = match timeout(HEADER_TIMEOUT, proto::read_message(&mut stream)).await {
        Ok(Ok(request)) => request,
        _ => {
            stream.abort();
            return;
        }
    };
    let address = SocketAddr::new(request.address.ip().to_canonical(), request.address.port());
    if let Err(message) = shared.config.outbound.allows(address) {
        tracing::warn!(%address, %message, "refused an outgoing connection for the server");
        refuse(&mut stream, ConnectFailure::Policy, message).await;
        return;
    }

    let limit = Duration::from_secs(u64::from(request.timeout_secs.clamp(1, MAX_CONNECT_SECS)));
    let tcp = match timeout(limit, TcpStream::connect(address)).await {
        Ok(Ok(tcp)) => tcp,
        Ok(Err(err)) => {
            tracing::debug!(%address, %err, "outgoing connection failed");
            refuse(&mut stream, ConnectFailure::from_io(&err), err.to_string()).await;
            return;
        }
        Err(_) => {
            tracing::debug!(%address, "outgoing connection timed out");
            refuse(&mut stream, ConnectFailure::Timeout, format!("{address} did not answer")).await;
            return;
        }
    };
    let Ok(local) = tcp.local_addr() else {
        stream.abort();
        return;
    };
    if proto::write_message(&mut stream, &ConnectReply::Connected { local }).await.is_err() {
        return;
    }
    tracing::debug!(%address, %local, "outgoing connection for the server");
    let _ = tcp.set_nodelay(true);
    crate::pipe::pipe(tcp, stream).await;
}

async fn refuse(stream: &mut TunnelStream, reason: ConnectFailure, message: String) {
    let _ = proto::write_message(stream, &ConnectReply::Failed { reason, message }).await;
    let _ = stream.shutdown().await;
}

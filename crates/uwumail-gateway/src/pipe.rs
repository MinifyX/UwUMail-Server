//! Copies bytes between a TCP connection and a tunnel stream, in both directions.

use std::time::Duration;

use quinn::VarInt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::timeout;
use uwumail_tunnel::TunnelStream;

const ABORTED: VarInt = VarInt::from_u32(1);
/// After the server side ended, how long the client may still send before the gateway hangs up.
const AFTER_SERVER_ENDED: Duration = Duration::from_secs(30);
/// After the client stopped sending, how long an answer may still take (large downloads).
const AFTER_CLIENT_ENDED: Duration = Duration::from_secs(600);

/// Runs until both directions ended. A clean end on one side ends that direction cleanly on the
/// other; a broken one is passed on as a reset.
pub(crate) async fn pipe(tcp: TcpStream, stream: TunnelStream) {
    let (mut tcp_read, mut tcp_write) = tcp.into_split();
    let (mut send, mut recv) = stream.into_parts();

    let upstream = async {
        match tokio::io::copy(&mut tcp_read, &mut send).await {
            Ok(_) => {
                let _ = send.finish();
            }
            Err(_) => {
                let _ = send.reset(ABORTED);
            }
        }
    };
    let downstream = async {
        match tokio::io::copy(&mut recv, &mut tcp_write).await {
            Ok(_) => {
                let _ = tcp_write.shutdown().await;
            }
            Err(_) => {
                let _ = recv.stop(ABORTED);
            }
        }
    };
    tokio::pin!(upstream, downstream);
    tokio::select! {
        _ = &mut upstream => {
            let _ = timeout(AFTER_CLIENT_ENDED, &mut downstream).await;
        }
        _ = &mut downstream => {
            let _ = timeout(AFTER_SERVER_ENDED, &mut upstream).await;
        }
    }
}

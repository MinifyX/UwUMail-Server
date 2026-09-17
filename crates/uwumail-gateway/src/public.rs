//! The public ports: every connection goes through the tunnel to the server, with the client's
//! real address in front. While the server is away, mail senders hear "try again later".

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::time::timeout;
use uwumail_tunnel::TunnelStream;
use uwumail_tunnel::proto::{self, Open, Service};

use crate::gateway::Shared;

/// Opening a stream waits while the server carries as many connections as it allows.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) async fn serve(
    shared: Arc<Shared>,
    listener: TcpListener,
    service: Service,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((tcp, client)) => {
                    tokio::spawn(handle(shared.clone(), tcp, client, service));
                }
                Err(err) => {
                    tracing::warn!(%err, service = service.as_str(), "accepting a connection failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            _ = shutdown.changed() => break,
        }
    }
}

async fn handle(shared: Arc<Shared>, mut tcp: TcpStream, client: SocketAddr, service: Service) {
    // Dual-stack listeners report IPv4 clients as ::ffff:a.b.c.d; the server needs the plain address.
    let client = SocketAddr::new(client.ip().to_canonical(), client.port());
    let Some(_admission) = shared.limits.admit(client.ip()) else {
        shared.note_turned_away(client.ip(), service);
        say_goodbye(&mut tcp, service, Goodbye::Busy, &shared.hostname()).await;
        return;
    };
    let Some(active) = shared.active.borrow().clone().filter(|active| active.services.contains(&service)) else {
        say_goodbye(&mut tcp, service, Goodbye::Away, &shared.hostname()).await;
        return;
    };
    let local = match tcp.local_addr() {
        Ok(local) => SocketAddr::new(local.ip().to_canonical(), local.port()),
        Err(_) => return,
    };
    let opened = timeout(OPEN_TIMEOUT, async {
        let (send, recv) = active.connection.open_bi().await.map_err(io::Error::other)?;
        let mut stream = TunnelStream::new(send, recv);
        proto::write_message(&mut stream, &Open { service, client, local }).await?;
        Ok::<_, io::Error>(stream)
    })
    .await;
    let stream = match opened {
        Ok(Ok(stream)) => stream,
        Ok(Err(_)) => {
            say_goodbye(&mut tcp, service, Goodbye::Away, &active.hostname).await;
            return;
        }
        Err(_) => {
            say_goodbye(&mut tcp, service, Goodbye::Busy, &active.hostname).await;
            return;
        }
    };
    tracing::debug!(%client, service = service.as_str(), "carrying a connection to the server");
    let _ = tcp.set_nodelay(true);
    crate::pipe::pipe(tcp, stream).await;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Goodbye {
    /// The server is not connected.
    Away,
    /// Too many connections.
    Busy,
}

/// What a client hears when its connection cannot reach the server. Mail servers retry after a
/// 421; TLS ports are simply closed, since only the server holds the certificate.
fn goodbye(service: Service, reason: Goodbye, hostname: &str) -> Option<String> {
    match (service, reason) {
        (Service::Smtp | Service::Submission, Goodbye::Away) => {
            Some(format!("421 4.3.2 {hostname} Service not available right now, please try again later\r\n"))
        }
        (Service::Smtp | Service::Submission, Goodbye::Busy) => {
            Some(format!("421 4.7.0 {hostname} Too many connections, please try again later\r\n"))
        }
        (Service::Http, _) => Some(
            "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 120\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into(),
        ),
        (Service::Submissions | Service::Https | Service::Imaps, _) => None,
    }
}

async fn say_goodbye(tcp: &mut TcpStream, service: Service, reason: Goodbye, hostname: &str) {
    if let Some(text) = goodbye(service, reason, hostname) {
        let _ = timeout(Duration::from_secs(5), tcp.write_all(text.as_bytes())).await;
    }
    let _ = tcp.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn senders_are_asked_to_come_back() {
        let away = goodbye(Service::Smtp, Goodbye::Away, "mail.example.com").unwrap();
        assert!(away.starts_with("421 4.3.2 mail.example.com "));
        assert!(away.ends_with("\r\n"));
        assert!(goodbye(Service::Http, Goodbye::Busy, "mail.example.com").unwrap().starts_with("HTTP/1.1 503"));
        assert!(goodbye(Service::Https, Goodbye::Away, "mail.example.com").is_none());
    }
}

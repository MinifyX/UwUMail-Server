//! IMAP4rev1 with the extensions mail apps expect (IDLE, UIDPLUS, MOVE, SPECIAL-USE, CONDSTORE,
//! QRESYNC, ESEARCH, QUOTA, UTF8=ACCEPT and more), on top of the store's mailboxes and messages.
//! Only implicit TLS (port 993) is offered.

pub mod command;
mod mailboxes;
mod mime;
pub mod mutf7;
pub mod parser;
mod response;
mod search;
mod session;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tokio_rustls::TlsAcceptor;
use uwumail_smtp::{AuthLimiter, BoxIo};
use uwumail_store::Store;

const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CONNECTIONS: usize = 2000;

/// Everything IMAP connections share. Cheap to clone.
#[derive(Clone)]
pub struct Imap {
    store: Store,
    limiter: Arc<AuthLimiter>,
    /// The biggest message APPEND takes, like the biggest message SMTP takes.
    max_append: usize,
    connections: Arc<Semaphore>,
}

impl Imap {
    pub fn new(store: Store, max_append: usize) -> Imap {
        Imap {
            store,
            limiter: Arc::new(AuthLimiter::default()),
            max_append,
            connections: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        }
    }

    /// Accepts connections on port 993 until `shutdown` changes.
    pub async fn serve(
        self,
        listener: TcpListener,
        tls: Arc<rustls::ServerConfig>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        loop {
            tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok((socket, peer)) => {
                        let _ = socket.set_nodelay(true);
                        self.serve_stream(Box::new(socket), peer, tls.clone());
                    }
                    Err(err) => {
                        tracing::warn!(%err, "accepting an imap connection failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                },
                _ = shutdown.changed() => break,
            }
        }
    }

    /// Runs one encrypted session on a connection from `peer`, which arrived on a listener or
    /// through the UwUMail Gateway.
    pub fn serve_stream(&self, stream: BoxIo, peer: SocketAddr, tls: Arc<rustls::ServerConfig>) {
        let imap = self.clone();
        tokio::spawn(async move {
            let Ok(_permit) = imap.connections.clone().try_acquire_owned() else {
                return;
            };
            let acceptor = TlsAcceptor::from(tls);
            let stream = match tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                Ok(Ok(stream)) => stream,
                Ok(Err(err)) => {
                    tracing::debug!(%peer, %err, "imap tls handshake failed");
                    return;
                }
                Err(_) => return,
            };
            imap.serve_connection(stream, peer).await;
        });
    }

    /// Runs a session on a connection that is already encrypted (or a test stream).
    pub async fn serve_connection<S>(&self, stream: S, peer: SocketAddr)
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (reader, writer) = tokio::io::split(stream);
        match session::Session::new(self.clone(), peer, reader, writer).run().await {
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::UnexpectedEof
                ) => {}
            Err(err) => tracing::debug!(%peer, %err, "imap session ended with an error"),
            Ok(()) => {}
        }
    }
}

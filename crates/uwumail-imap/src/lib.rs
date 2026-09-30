//! IMAP4rev1 and IMAP4rev2 with the extensions mail apps expect (IDLE, UIDPLUS, MOVE, SPECIAL-USE, CONDSTORE,
//! QRESYNC, ESEARCH, QUOTA, UTF8=ACCEPT, ACL and more), on top of the store's mailboxes and messages,
//! folders others share with the account among them (docs/sharing.md).
//! Only implicit TLS (port 993) is offered. ManageSieve (RFC 5804, port 4190) lives here too: it
//! shares the logins and the lockouts.

pub mod command;
mod mailboxes;
mod managesieve;
pub mod mime;
pub mod mutf7;
pub mod parser;
mod response;
mod search;
mod session;

pub use managesieve::ManageSieve;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tokio_rustls::TlsAcceptor;
use uwumail_smtp::{AuthLimiter, BoxIo, ClientSlot, ClientSlots};
use uwumail_store::Store;

const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CONNECTIONS: usize = 2000;
/// Connections at once from one client address (IPv4, or IPv6 /64). Without it one address could
/// hold all [`MAX_CONNECTIONS`] with a TLS handshake or a greeting it never answers. More than SMTP
/// allows: every mail app keeps a few IMAP connections open, and an office shares one address.
pub const MAX_CONNECTIONS_PER_CLIENT: usize = 50;

/// Which addresses hand on connections of many clients and so are not counted as one.
type Trusted = Arc<dyn Fn(IpAddr) -> bool + Send + Sync>;

/// Connections per client address, for IMAP and ManageSieve each.
#[derive(Clone)]
pub(crate) struct ClientLimit {
    slots: ClientSlots,
    max: usize,
    trusted: Trusted,
}

impl ClientLimit {
    fn new(max: usize, trusted: Trusted) -> ClientLimit {
        ClientLimit { slots: ClientSlots::new(), max, trusted }
    }

    /// A count of its own with another limit, trusting the same addresses.
    pub(crate) fn with_max(&self, max: usize) -> ClientLimit {
        ClientLimit::new(max, self.trusted.clone())
    }

    /// `Some(None)` for a trusted address, which is not counted; `None` when `peer` has all its
    /// connections open already.
    pub(crate) fn admit(&self, peer: IpAddr) -> Option<Option<ClientSlot>> {
        if (self.trusted)(peer.to_canonical()) {
            return Some(None);
        }
        self.slots.take(peer, self.max).map(Some)
    }
}

/// Everything IMAP connections share. Cheap to clone.
#[derive(Clone)]
pub struct Imap {
    store: Store,
    limiter: Arc<AuthLimiter>,
    /// The biggest message APPEND takes, like the biggest message SMTP takes.
    max_append: usize,
    connections: Arc<Semaphore>,
    clients: ClientLimit,
    /// The server's name, for the address of the OpenID configuration a failed token login points to.
    hostname: Option<String>,
}

impl Imap {
    pub fn new(store: Store, max_append: usize) -> Imap {
        Imap {
            limiter: store.auth_limiter().clone(),
            store,
            max_append,
            connections: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            clients: ClientLimit::new(MAX_CONNECTIONS_PER_CLIENT, Arc::new(|_| false)),
            hostname: None,
        }
    }

    /// The server's name: apps whose token was refused learn where to get a new one.
    pub fn with_hostname(mut self, hostname: &str) -> Imap {
        self.hostname = Some(hostname.to_owned());
        self
    }

    /// Another limit of connections at once from one client address than
    /// [`MAX_CONNECTIONS_PER_CLIENT`]; for tests, and for ManageSieve's own.
    pub fn with_client_limit(mut self, max: usize) -> Imap {
        self.clients = self.clients.with_max(max);
        self
    }

    /// Addresses for which `trusted` says yes are not limited like one client: a proxy in front
    /// that makes every connection come from its own address (`smtp.trusted_relays`, as for SMTP).
    /// Connections through the UwUMail Gateway carry the client's own address and count as its.
    pub fn trusting(mut self, trusted: impl Fn(IpAddr) -> bool + Send + Sync + 'static) -> Imap {
        self.clients = ClientLimit::new(self.clients.max, Arc::new(trusted));
        self
    }

    /// Hands every network this turns away to `reporter` as well, so it can be kept out further
    /// away than this server — at the UwUMail Gateway, where it never reaches the house at all.
    pub fn report_blocks_to(&self, reporter: Option<uwumail_smtp::Reporter>) {
        self.limiter.report_to(reporter);
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
        // Counted before the handshake: a handshake nobody finishes holds a slot as well. There
        // is no way to say why on port 993 before TLS, so the connection is just closed.
        let Some(slot) = self.clients.admit(peer.ip()) else {
            tracing::debug!(%peer, "too many imap connections from one client");
            return;
        };
        let imap = self.clone();
        tokio::spawn(async move {
            let _slot = slot;
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

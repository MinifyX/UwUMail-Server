use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Anything that reads and writes like a TCP connection: a socket, or a connection carried
/// through the UwUMail Gateway.
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

pub type BoxIo = Box<dyn Io>;

/// A connection that may be upgraded to TLS in the middle of a session.
pub enum Stream {
    Plain(BoxIo),
    Server(Box<tokio_rustls::server::TlsStream<BoxIo>>),
    Client(Box<tokio_rustls::client::TlsStream<BoxIo>>),
}

impl Stream {
    pub fn is_tls(&self) -> bool {
        !matches!(self, Stream::Plain(_))
    }

    /// TLS protocol version and cipher suite, for Received headers.
    pub fn tls_description(&self) -> Option<String> {
        let (version, suite) = match self {
            Stream::Plain(_) => return None,
            Stream::Server(tls) => (tls.get_ref().1.protocol_version(), tls.get_ref().1.negotiated_cipher_suite()),
            Stream::Client(tls) => (tls.get_ref().1.protocol_version(), tls.get_ref().1.negotiated_cipher_suite()),
        };
        Some(format!(
            "{} with cipher {}",
            version.map(|v| format!("{v:?}").replace('_', ".")).unwrap_or_else(|| "TLS".into()),
            suite.map(|s| format!("{:?}", s.suite())).unwrap_or_else(|| "unknown".into()),
        ))
    }
}

impl AsyncRead for Stream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Server(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
            Stream::Client(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Server(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
            Stream::Client(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_flush(cx),
            Stream::Server(s) => Pin::new(s.as_mut()).poll_flush(cx),
            Stream::Client(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Server(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
            Stream::Client(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

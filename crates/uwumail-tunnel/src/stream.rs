use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use quinn::{RecvStream, SendStream, VarInt};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Error code for streams that end because the other end of the carried connection broke off.
const ABORTED: VarInt = VarInt::from_u32(1);

/// One connection carried through the tunnel. Reads and writes like a TCP socket; shutting down
/// the write side ends the stream cleanly, like a TCP FIN.
#[derive(Debug)]
pub struct TunnelStream {
    send: SendStream,
    recv: RecvStream,
}

impl TunnelStream {
    pub fn new(send: SendStream, recv: RecvStream) -> TunnelStream {
        TunnelStream { send, recv }
    }

    /// Ends both directions at once without a clean close, so the other side notices the break.
    pub fn abort(&mut self) {
        let _ = self.send.reset(ABORTED);
        let _ = self.recv.stop(ABORTED);
    }

    /// The two directions, for copying each one on its own.
    pub fn into_parts(self) -> (SendStream, RecvStream) {
        (self.send, self.recv)
    }
}

// The quinn streams have inherent methods with the same names and other error types, so the
// trait methods are called by their full names.
impl AsyncRead for TunnelStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        AsyncRead::poll_read(Pin::new(&mut self.get_mut().recv), cx, buf)
    }
}

impl AsyncWrite for TunnelStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.get_mut().send), cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.get_mut().send), cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.get_mut().send), cx)
    }
}

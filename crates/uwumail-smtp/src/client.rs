//! A small SMTP client for delivering to other servers and relays.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rustls::ClientConfig;
use rustls_pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;

use crate::Context;
use crate::stream::{BoxIo, Stream};

/// Makes the connections to other servers, for delivery and for the checks. Without one, they
/// start from this machine.
pub trait Connector: Send + Sync {
    /// Connects to `address`, giving up after `limit`.
    fn connect(
        &self,
        address: SocketAddr,
        limit: Duration,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<BoxIo>> + Send + '_>>;
}

/// A plain TCP connection from this machine.
pub async fn connect_directly(address: SocketAddr, limit: Duration) -> std::io::Result<BoxIo> {
    let socket = timeout(limit, TcpStream::connect(address)).await.map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::TimedOut, format!("connecting to {address} timed out"))
    })??;
    let _ = socket.set_nodelay(true);
    Ok(Box::new(socket))
}

#[derive(Debug, Clone)]
pub struct Reply {
    pub code: u16,
    pub text: String,
}

impl Reply {
    pub fn is_positive(&self) -> bool {
        (200..400).contains(&self.code)
    }

    pub fn is_permanent(&self) -> bool {
        self.code >= 500
    }
}

impl std::fmt::Display for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.code, self.text)
    }
}

#[derive(Debug, Default, Clone)]
pub struct Capabilities {
    pub starttls: bool,
    pub size: Option<usize>,
    pub smtputf8: bool,
    pub eight_bit_mime: bool,
    pub auth_plain: bool,
    pub auth_login: bool,
}

pub struct Client {
    stream: Stream,
    pending: Vec<u8>,
    command_timeout: Duration,
}

impl Client {
    /// Connects through the server's [`Connector`], if it has one.
    pub async fn connect(
        ctx: &Context,
        addr: SocketAddr,
        connect_timeout: Duration,
        command_timeout: Duration,
    ) -> std::io::Result<Client> {
        let stream = match ctx.connector() {
            Some(connector) => connector.connect(addr, connect_timeout).await?,
            None => connect_directly(addr, connect_timeout).await?,
        };
        Ok(Client { stream: Stream::Plain(stream), pending: Vec::new(), command_timeout })
    }

    pub fn is_tls(&self) -> bool {
        self.stream.is_tls()
    }

    /// Upgrades the connection to TLS (after STARTTLS, or right away for implicit TLS).
    pub async fn tls_handshake(self, config: Arc<ClientConfig>, host: &str) -> std::io::Result<Client> {
        let Client { stream, pending, command_timeout } = self;
        let Stream::Plain(socket) = stream else {
            return Ok(Client { stream, pending, command_timeout });
        };
        let name = ServerName::try_from(host.trim_end_matches('.').to_owned())
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?;
        let tls = timeout(Duration::from_secs(30), TlsConnector::from(config).connect(name, socket))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "TLS handshake timed out"))??;
        // Nothing the server sent before the handshake may be trusted afterwards.
        Ok(Client { stream: Stream::Client(Box::new(tls)), pending: Vec::new(), command_timeout })
    }

    /// Reads one complete (possibly multi-line) reply.
    pub async fn read_reply(&mut self) -> std::io::Result<Reply> {
        self.read_reply_with(self.command_timeout).await
    }

    pub async fn read_reply_with(&mut self, limit: Duration) -> std::io::Result<Reply> {
        let mut lines: Vec<String> = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            while let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.pending.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line).trim_end().to_owned();
                if line.len() < 3 || !line.as_bytes()[..3].iter().all(u8::is_ascii_digit) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("bad SMTP reply: {line}"),
                    ));
                }
                let last = line.as_bytes().get(3) != Some(&b'-');
                lines.push(line);
                if last {
                    let code = lines[0][..3].parse().unwrap_or(0);
                    let text = lines.iter().map(|l| l.get(4..).unwrap_or_default()).collect::<Vec<_>>().join("\n");
                    return Ok(Reply { code, text });
                }
                if lines.len() > 100 {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "SMTP reply too long"));
                }
            }
            if self.pending.len() > 64 * 1024 {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "SMTP reply line too long"));
            }
            let read = timeout(limit, self.stream.read(&mut buf)).await.map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "the server did not answer in time")
            })??;
            if read == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the server closed the connection"));
            }
            self.pending.extend_from_slice(&buf[..read]);
        }
    }

    pub async fn send(&mut self, command: &str) -> std::io::Result<Reply> {
        self.write(command.as_bytes()).await?;
        self.read_reply().await
    }

    async fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        timeout(self.command_timeout, async {
            self.stream.write_all(bytes).await?;
            self.stream.flush().await
        })
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "sending to the server timed out"))?
    }

    pub async fn ehlo(&mut self, hostname: &str) -> std::io::Result<(Reply, Capabilities)> {
        let reply = self.send(&format!("EHLO {hostname}\r\n")).await?;
        let mut caps = Capabilities::default();
        if reply.is_positive() {
            for line in reply.text.lines().skip(1) {
                let upper = line.to_ascii_uppercase();
                let mut words = upper.split_whitespace();
                match words.next() {
                    Some("STARTTLS") => caps.starttls = true,
                    Some("SMTPUTF8") => caps.smtputf8 = true,
                    Some("8BITMIME") => caps.eight_bit_mime = true,
                    Some("SIZE") => caps.size = words.next().and_then(|s| s.parse().ok()).filter(|&s| s > 0),
                    Some("AUTH") => {
                        for mechanism in words {
                            caps.auth_plain |= mechanism == "PLAIN";
                            caps.auth_login |= mechanism == "LOGIN";
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok((reply, caps))
    }

    pub async fn auth_plain(&mut self, username: &str, password: &str) -> std::io::Result<Reply> {
        let mut token = Vec::with_capacity(username.len() + password.len() + 2);
        token.push(0);
        token.extend_from_slice(username.as_bytes());
        token.push(0);
        token.extend_from_slice(password.as_bytes());
        self.send(&format!("AUTH PLAIN {}\r\n", BASE64.encode(token))).await
    }

    /// Sends the message body after a 354 and returns the final reply.
    pub async fn data(&mut self, raw: &[u8], data_timeout: Duration) -> std::io::Result<Reply> {
        let body = dot_stuff(raw);
        timeout(data_timeout, async {
            self.stream.write_all(&body).await?;
            self.stream.flush().await
        })
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "sending the message timed out"))??;
        self.read_reply_with(data_timeout).await
    }

    pub async fn quit(mut self) {
        let _ = timeout(Duration::from_secs(5), self.send("QUIT\r\n")).await;
        let _ = self.stream.shutdown().await;
    }
}

/// Dot-stuffs a message and terminates it with `CRLF.CRLF`.
pub fn dot_stuff(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 64);
    let mut line_start = true;
    let mut previous = 0u8;
    for &byte in raw {
        if byte == b'\n' && previous != b'\r' {
            out.push(b'\r');
        }
        if line_start && byte == b'.' {
            out.push(b'.');
        }
        out.push(byte);
        line_start = byte == b'\n';
        previous = byte;
    }
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b".\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stuffs_dots_and_terminates() {
        assert_eq!(dot_stuff(b"a\r\n.b\r\n..c"), b"a\r\n..b\r\n...c\r\n.\r\n");
        assert_eq!(dot_stuff(b".\nx\n"), b"..\r\nx\r\n.\r\n");
    }
}

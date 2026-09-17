//! Messages in the tunnel. Each is a JSON object behind a four-byte big-endian length; after the
//! header of a stream, only the bytes of the carried connection follow.

use std::io;
use std::net::{IpAddr, SocketAddr};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The tunnel protocol version. Raised only when the two sides would misunderstand each other.
pub const VERSION: u32 = 1;
const MAX_MESSAGE: usize = 64 * 1024;

/// What a connection that arrived at the gateway is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Service {
    /// Port 25: mail from other servers.
    Smtp,
    /// Port 587: mail apps with STARTTLS.
    Submission,
    /// Port 465: mail apps with TLS.
    Submissions,
    /// Port 80: certificate challenges and the redirect to HTTPS.
    Http,
    /// Port 443: the web portal, web mail and JMAP.
    Https,
    /// Port 993: mail apps reading mail with IMAP over TLS.
    Imaps,
}

impl Service {
    pub const ALL: [Service; 6] =
        [Service::Smtp, Service::Submission, Service::Submissions, Service::Http, Service::Https, Service::Imaps];

    /// What servers from before `Hello::services` existed take: they cannot even read the others.
    pub const FIRST: [Service; 5] =
        [Service::Smtp, Service::Submission, Service::Submissions, Service::Http, Service::Https];

    pub fn as_str(self) -> &'static str {
        match self {
            Service::Smtp => "smtp",
            Service::Submission => "submission",
            Service::Submissions => "submissions",
            Service::Http => "http",
            Service::Https => "https",
            Service::Imaps => "imaps",
        }
    }
}

/// The first message on the control stream, from the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub version: u32,
    /// The server's host name, used in the gateway's answers while the server is away.
    pub hostname: String,
    pub software: String,
    /// The one-time token of a pairing code, while pairing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The services the server takes. Missing from older servers, which take [`Service::FIRST`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub services: Option<Vec<Service>>,
}

impl Hello {
    pub fn services(&self) -> &[Service] {
        self.services.as_deref().unwrap_or(&Service::FIRST)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HelloReply {
    Welcome(Welcome),
    Refused {
        reason: Refusal,
        #[serde(default)]
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Welcome {
    pub version: u32,
    pub software: String,
    /// The gateway's public addresses: where the server's host name has to point.
    pub addresses: Vec<IpAddr>,
    /// The public ports that are open, by service.
    pub services: Vec<Service>,
    /// Ports the gateway connects to for outgoing mail.
    pub outbound_ports: Vec<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Refusal {
    /// The gateway waits for a pairing code, and none was sent.
    NotPaired,
    /// The token does not belong to the gateway's current pairing code.
    WrongToken,
    /// The gateway belongs to another server.
    OtherServer,
    /// The two sides speak different tunnel versions.
    Version,
}

/// Starts each stream the gateway opens: a connection arrived from `client`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Open {
    pub service: Service,
    pub client: SocketAddr,
    /// The gateway address the client connected to.
    pub local: SocketAddr,
}

/// Starts each stream the server opens: connect to `address` from the gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connect {
    pub address: SocketAddr,
    pub timeout_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ConnectReply {
    Connected {
        /// The gateway's address other servers see.
        local: SocketAddr,
    },
    Failed {
        reason: ConnectFailure,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectFailure {
    /// The gateway does not connect there (a private address, or a port that is not for mail).
    Policy,
    Refused,
    Timeout,
    Unreachable,
}

impl ConnectFailure {
    pub fn io_kind(self) -> io::ErrorKind {
        match self {
            ConnectFailure::Policy => io::ErrorKind::PermissionDenied,
            ConnectFailure::Refused => io::ErrorKind::ConnectionRefused,
            ConnectFailure::Timeout => io::ErrorKind::TimedOut,
            ConnectFailure::Unreachable => io::ErrorKind::HostUnreachable,
        }
    }

    pub fn from_io(error: &io::Error) -> ConnectFailure {
        match error.kind() {
            io::ErrorKind::ConnectionRefused => ConnectFailure::Refused,
            io::ErrorKind::TimedOut => ConnectFailure::Timeout,
            _ => ConnectFailure::Unreachable,
        }
    }
}

pub async fn write_message<W, T>(writer: &mut W, message: &T) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = serde_json::to_vec(message).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    if body.len() > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "tunnel message too big"));
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    writer.write_all(&frame).await?;
    writer.flush().await
}

/// Reads exactly one message, never more, so the carried connection's bytes stay in the stream.
pub async fn read_message<R, T>(reader: &mut R) -> io::Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut length = [0u8; 4];
    reader.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "tunnel message too big"));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn messages_leave_the_following_bytes_alone() {
        let open = Open {
            service: Service::Smtp,
            client: "192.0.2.7:40000".parse().unwrap(),
            local: "192.0.2.10:25".parse().unwrap(),
        };
        let mut buffer = Vec::new();
        write_message(&mut buffer, &open).await.unwrap();
        buffer.extend_from_slice(b"EHLO client.example\r\n");

        let mut reader = buffer.as_slice();
        let read: Open = read_message(&mut reader).await.unwrap();
        assert_eq!(read, open);
        assert_eq!(reader, b"EHLO client.example\r\n");
    }

    #[tokio::test]
    async fn oversized_messages_are_refused() {
        let mut frame = ((MAX_MESSAGE + 1) as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(b"{}");
        let error = read_message::<_, serde_json::Value>(&mut frame.as_slice()).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn replies_are_tagged() {
        let reply = HelloReply::Refused { reason: Refusal::OtherServer, message: String::new() };
        assert_eq!(serde_json::to_value(&reply).unwrap()["type"], "refused");
        assert_eq!(serde_json::to_value(Service::Submissions).unwrap(), "submissions");
    }

    #[test]
    fn servers_without_a_service_list_take_the_first_services() {
        let old: Hello = serde_json::from_str(r#"{"version":1,"hostname":"mail.example.de","software":"x"}"#).unwrap();
        assert_eq!(old.services(), Service::FIRST);
        let hello = Hello {
            version: VERSION,
            hostname: "mail.example.de".into(),
            software: "x".into(),
            token: None,
            services: Some(Service::ALL.to_vec()),
        };
        let json = serde_json::to_value(&hello).unwrap();
        assert_eq!(json["services"][5], "imaps");
    }
}

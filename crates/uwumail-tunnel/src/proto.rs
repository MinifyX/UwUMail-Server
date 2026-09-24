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
    /// Whether this server speaks on the control stream after the handshake. Older servers never
    /// read it again, so the gateway stays quiet for them.
    #[serde(default)]
    pub control: bool,
    /// Whether this server wants the gateway's log lines on the control stream. Older servers would
    /// only skip them, so the gateway keeps them to itself unless asked.
    #[serde(default)]
    pub logs: bool,
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
    /// Whether this gateway speaks on the control stream after the handshake. Older gateways
    /// never read it again, so the server keeps its bans to itself.
    #[serde(default)]
    pub control: bool,
    /// Whether this gateway can be asked to update or restart its machine. A gateway that cannot
    /// simply ignores the ask, so this is only here to keep the portal from offering a button that
    /// would do nothing.
    #[serde(default)]
    pub tasks: bool,
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

/// What the gateway says on the control stream once both sides agreed on it. Only sent when the
/// server's [`Hello::control`] says it listens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum GatewayMessage {
    Status(Box<GatewayStatus>),
    /// What the gateway logged since the last batch, oldest first. Only sent when the server's
    /// [`Hello::logs`] asks for it; a batch always fits into one message.
    Logs {
        lines: Vec<GatewayLogLine>,
    },
}

/// One line of the gateway's log, the way the server's own lines are kept.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GatewayLogLine {
    /// Milliseconds since 1970.
    pub at: u64,
    /// `error`, `warn`, `info`, `debug` or `trace`.
    pub level: String,
    pub message: String,
    pub fields: Vec<(String, String)>,
}

impl GatewayLogLine {
    /// Longest message and field value the gateway sends; the rest is cut, so one chatty line can
    /// never take a whole batch.
    pub const MAX_TEXT: usize = 2048;
    /// Most fields per line.
    pub const MAX_FIELDS: usize = 24;
    /// Room a batch leaves inside [`MAX_MESSAGE`] for the envelope around the lines.
    pub const BATCH_BYTES: usize = MAX_MESSAGE - 4 * 1024;

    /// Cuts the line down to what may travel.
    pub fn bounded(mut self) -> GatewayLogLine {
        cut(&mut self.message, Self::MAX_TEXT);
        self.fields.truncate(Self::MAX_FIELDS);
        for (key, value) in &mut self.fields {
            cut(key, 64);
            cut(value, Self::MAX_TEXT);
        }
        self
    }

    /// Roughly how many bytes the line takes as JSON, to fill a batch without going over.
    pub fn size(&self) -> usize {
        serde_json::to_vec(self).map(|json| json.len()).unwrap_or(GatewayLogLine::BATCH_BYTES)
    }
}

fn cut(text: &mut String, max: usize) {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
}

/// What the server asks of the gateway on the control stream. Only sent when the gateway's
/// [`Welcome::control`] says it listens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerMessage {
    /// Keep `ip` away from the gateway's public ports for `seconds`. The gateway refuses addresses
    /// its own tunnel comes from, whatever the server asks: a mail app at home getting a password
    /// wrong must never cut the household off from its own gateway.
    Ban {
        ip: IpAddr,
        seconds: u32,
        reason: String,
    },
    Unban {
        ip: IpAddr,
    },
    /// Something the VPS should do: install its updates, restart, or fetch a new gateway.
    ///
    /// `verb` is one of a fixed list the gateway checks again, and `version` has to look like a
    /// version. Never a command, never a path, never an address: the gateway's helper runs as root,
    /// and it builds the address it downloads from out of its own constants. A server somebody
    /// broke into must not be able to hand a VPS something to run.
    Task {
        id: String,
        verb: String,
        #[serde(default)]
        version: Option<String>,
    },
}

/// What became of a [`ServerMessage::Task`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TaskState {
    pub id: String,
    /// `running`, `done`, `failed` or `refused`.
    pub state: String,
    pub error: String,
    pub at: i64,
    /// The tail of what it printed, for the portal to show.
    pub log: String,
}

/// How the machine the gateway runs on is doing. The portal shows it, so a VPS that needs updates
/// or lost its firewall says so where it is noticed, instead of waiting for the next SSH login.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GatewayStatus {
    pub software: String,
    pub system: Option<System>,
    pub protection: Option<Protection>,
    /// The addresses the gateway keeps out of every ban list, this server's among them.
    pub trusted: Vec<IpAddr>,
    /// Unix time the gateway last looked at the machine.
    pub checked_at: i64,
    /// The task asked for last, while there is one. It rides along with the status instead of
    /// having a message of its own: the status is already sent regularly and already kept by the
    /// server, and the gateway simply reports more often while something is running.
    pub job: Option<TaskState>,
}

/// The operating system of the VPS and what it waits for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct System {
    /// As the machine calls itself, for example `Ubuntu 26.04.1 LTS`.
    pub name: String,
    pub updates: u32,
    pub security_updates: u32,
    pub reboot_required: bool,
    /// Whether security updates install themselves.
    pub automatic_security: bool,
    /// A newer release of the operating system, when one waits.
    pub new_release: Option<String>,
    /// What to run on the VPS to install the updates.
    pub command: String,
}

/// What keeps the VPS itself safe: the firewall and fail2ban.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Protection {
    /// The firewall in use, for example `ufw`; empty when none was found.
    pub firewall: String,
    pub firewall_active: bool,
    pub fail2ban: bool,
    /// Addresses fail2ban keeps out right now, over all jails.
    pub banned: u32,
    /// The jails that watch, by name.
    pub jails: Vec<String>,
    /// Of those bans, the ones this server asked for.
    pub from_server: u32,
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

/// Reads one message on a stream that carries a conversation, telling a broken stream apart from a
/// message this version does not know yet. Unknown ones come back as `None` and the reader stays in
/// step, so a newer other side never has to break off the control stream to stay understood.
pub async fn read_known<R, T>(reader: &mut R) -> io::Result<Option<T>>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let value: serde_json::Value = read_message(reader).await?;
    Ok(serde_json::from_value(value).ok())
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
        let old: Hello = serde_json::from_str(r#"{"version":1,"hostname":"mail.example.org","software":"x"}"#).unwrap();
        assert_eq!(old.services(), Service::FIRST);
        let hello = Hello {
            version: VERSION,
            hostname: "mail.example.org".into(),
            software: "x".into(),
            token: None,
            services: Some(Service::ALL.to_vec()),
            control: true,
            logs: true,
        };
        let json = serde_json::to_value(&hello).unwrap();
        assert_eq!(json["services"][5], "imaps");
    }

    #[test]
    fn sides_from_before_the_control_stream_stay_quiet() {
        let old: Hello = serde_json::from_str(r#"{"version":1,"hostname":"mail.example.org","software":"x"}"#).unwrap();
        assert!(!old.control, "an older server does not read the control stream");
        let old: Welcome =
            serde_json::from_str(r#"{"version":1,"software":"x","addresses":[],"services":[],"outboundPorts":[]}"#)
                .unwrap();
        assert!(!old.control, "an older gateway does not read the control stream");
    }

    #[test]
    fn control_messages_are_tagged() {
        let ban = ServerMessage::Ban { ip: "192.0.2.7".parse().unwrap(), seconds: 3600, reason: "imap".into() };
        let json = serde_json::to_value(&ban).unwrap();
        assert_eq!(json["type"], "ban");
        assert_eq!(json["ip"], "192.0.2.7");

        let status = GatewayMessage::Status(Box::default());
        assert_eq!(serde_json::to_value(&status).unwrap()["type"], "status");
        // Fields the other side does not know yet must not make the whole message unreadable.
        let grown = r#"{"type":"status","software":"uwumail-gateway 9.9.9","whatIsThis":true}"#;
        let read: GatewayMessage = serde_json::from_str(grown).unwrap();
        let GatewayMessage::Status(status) = read else { panic!("a status") };
        assert_eq!(status.software, "uwumail-gateway 9.9.9");
    }

    #[tokio::test]
    async fn an_unknown_message_is_skipped_instead_of_ending_the_talk() {
        let mut buffer = Vec::new();
        write_message(&mut buffer, &serde_json::json!({ "type": "somethingNewer" })).await.unwrap();
        write_message(&mut buffer, &ServerMessage::Unban { ip: "192.0.2.7".parse().unwrap() }).await.unwrap();

        let mut reader = buffer.as_slice();
        assert!(read_known::<_, ServerMessage>(&mut reader).await.unwrap().is_none(), "skipped, not fatal");
        let next = read_known::<_, ServerMessage>(&mut reader).await.unwrap();
        assert_eq!(next, Some(ServerMessage::Unban { ip: "192.0.2.7".parse().unwrap() }), "still in step");
    }

    #[test]
    fn log_lines_travel_only_when_asked_and_stay_small() {
        let old: Hello =
            serde_json::from_str(r#"{"version":1,"hostname":"mail.example.org","software":"x","control":true}"#)
                .unwrap();
        assert!(!old.logs, "an older server did not ask for the gateway's log");

        let logs = GatewayMessage::Logs {
            lines: vec![GatewayLogLine {
                at: 1,
                level: "info".into(),
                message: "ready".into(),
                fields: vec![("fingerprint".into(), "ab:cd".into())],
            }],
        };
        let json = serde_json::to_value(&logs).unwrap();
        assert_eq!(json["type"], "logs");
        assert_eq!(json["lines"][0]["fields"][0], serde_json::json!(["fingerprint", "ab:cd"]));

        let long = GatewayLogLine {
            message: "ä".repeat(GatewayLogLine::MAX_TEXT),
            fields: (0..100).map(|n| (format!("k{n}"), "v".into())).collect(),
            ..GatewayLogLine::default()
        }
        .bounded();
        assert!(long.message.len() <= GatewayLogLine::MAX_TEXT + '…'.len_utf8());
        assert!(long.message.ends_with('…'), "cut on a character boundary");
        assert_eq!(long.fields.len(), GatewayLogLine::MAX_FIELDS);
        assert!(long.size() < GatewayLogLine::BATCH_BYTES);
    }
}

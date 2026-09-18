//! Handing a message to a ClamAV daemon (clamd) before it is taken.
//!
//! clamd runs beside the server in its own container: it wants a writable place for its
//! signatures and about a gigabyte of memory for them, neither of which the read-only UwUMail
//! image has. Only its address is configured here, the message travels over the `INSTREAM` command,
//! and nothing of it is kept on either side.

use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::config::AntivirusConfig;

/// How much of the message goes over the wire at a time.
const CHUNK: usize = 32 * 1024;
/// clamd answers in one short line; anything longer is not an answer we understand.
const MAX_REPLY: usize = 4096;
/// How long the portal waits for the scanner to say what it is.
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);

/// What clamd said about one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scan {
    Clean,
    /// What was found, e.g. `Eicar-Test-Signature`.
    Found(String),
    /// Bigger than the scanner looks at, so nobody looked.
    TooBig,
}

/// What the scanner itself is, for the portal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// The whole version line, e.g. `ClamAV 1.4.2/27700/Wed Sep 17 08:32:11 2026`.
    pub version: String,
    /// The number of the signature database, which grows with every update.
    pub signatures: Option<u32>,
    /// When that database was built, in unix seconds.
    pub signatures_at: Option<i64>,
}

/// A clamd somewhere on the network.
pub struct Clamav {
    address: String,
    timeout: Duration,
    max_size: usize,
}

impl Clamav {
    pub fn new(config: &AntivirusConfig) -> Clamav {
        Clamav {
            address: config.address.trim().to_owned(),
            timeout: Duration::from_secs(config.timeout_secs.max(1)),
            max_size: config.max_size,
        }
    }

    async fn connect(&self) -> Result<TcpStream, String> {
        if self.address.is_empty() {
            return Err("no address for the virus scanner".to_owned());
        }
        let stream = tokio::time::timeout(self.timeout, TcpStream::connect(&self.address))
            .await
            .map_err(|_| "the virus scanner did not answer in time".to_owned())?
            .map_err(|err| format!("the virus scanner cannot be reached: {err}"))?;
        Ok(stream)
    }

    /// Reads clamd's answer, which ends at the NUL byte the `z` commands ask for.
    async fn reply(stream: &mut TcpStream) -> Result<String, String> {
        let mut buffer = Vec::new();
        let mut byte = [0u8; 1];
        while buffer.len() < MAX_REPLY {
            match stream.read(&mut byte).await {
                Ok(0) => break,
                Ok(_) if byte[0] == 0 => break,
                Ok(_) => buffer.push(byte[0]),
                Err(err) => return Err(format!("the virus scanner broke off: {err}")),
            }
        }
        Ok(String::from_utf8_lossy(&buffer).trim().to_owned())
    }

    /// Looks at one message. An error means nobody looked, not that it is clean.
    pub async fn scan(&self, raw: &[u8]) -> Result<Scan, String> {
        if raw.len() > self.max_size {
            return Ok(Scan::TooBig);
        }
        let answer = tokio::time::timeout(self.timeout, self.instream(raw))
            .await
            .map_err(|_| "the virus scanner did not answer in time".to_owned())??;
        verdict(&answer)
    }

    async fn instream(&self, raw: &[u8]) -> Result<String, String> {
        let mut stream = self.connect().await?;
        let send = async {
            stream.write_all(b"zINSTREAM\0").await?;
            for chunk in raw.chunks(CHUNK) {
                stream.write_all(&(chunk.len() as u32).to_be_bytes()).await?;
                stream.write_all(chunk).await?;
            }
            // A length of zero closes the stream and makes clamd answer.
            stream.write_all(&0u32.to_be_bytes()).await?;
            stream.flush().await
        };
        send.await.map_err(|err: std::io::Error| format!("the virus scanner broke off: {err}"))?;
        Clamav::reply(&mut stream).await
    }

    /// What clamd answers to `VERSION`, for the portal.
    ///
    /// The portal asks this while someone waits for a page, so it gives up much sooner than a
    /// scan does: a scanner that hangs must not hold up the overview.
    pub async fn status(&self) -> Result<Status, String> {
        let answer = tokio::time::timeout(self.timeout.min(STATUS_TIMEOUT), async {
            let mut stream = self.connect().await?;
            stream.write_all(b"zVERSION\0").await.map_err(|err| format!("the virus scanner broke off: {err}"))?;
            Clamav::reply(&mut stream).await
        })
        .await
        .map_err(|_| "the virus scanner did not answer in time".to_owned())??;
        if answer.is_empty() {
            return Err("the virus scanner said nothing".to_owned());
        }
        let mut parts = answer.split('/');
        let _ = parts.next();
        Ok(Status {
            signatures: parts.next().and_then(|number| number.trim().parse().ok()),
            signatures_at: parts.next().and_then(built_at),
            version: answer,
        })
    }
}

/// The EICAR test file: harmless, but every scanner reports it. It is put together at runtime so
/// this binary does not carry the whole signature around and set off scanners on its own.
pub fn test_file() -> String {
    ["X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR", "-STANDARD-ANTIVIRUS-", "TEST-FILE!$H+H*"].concat()
}

/// What the check decided about one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checked {
    /// The scanner is switched off. Nobody looked, and nothing is claimed.
    Off,
    Clean,
    Found(String),
    /// Nobody looked: the scanner is away, too slow, or the message is bigger than it takes.
    /// The message goes on and says so in its headers, because post has to keep moving.
    Unchecked(&'static str),
}

/// Hands one message to the scanner. An error is never a clean verdict.
pub async fn check(config: &AntivirusConfig, raw: &[u8]) -> Checked {
    if !config.enabled {
        return Checked::Off;
    }
    match Clamav::new(config).scan(raw).await {
        Ok(Scan::Clean) => Checked::Clean,
        Ok(Scan::Found(name)) => Checked::Found(plain(&name)),
        Ok(Scan::TooBig) => Checked::Unchecked("the message is bigger than the scanner takes"),
        Err(err) => {
            tracing::warn!(%err, "the virus scanner could not look at a message");
            Checked::Unchecked("the virus scanner did not answer")
        }
    }
}

/// The header a delivered message carries, so the reader can see whether anyone looked.
pub fn header(checked: &Checked) -> Option<String> {
    match checked {
        Checked::Off | Checked::Found(_) => None,
        Checked::Clean => Some("X-Virus-Scanned: yes (ClamAV)\r\n".to_owned()),
        Checked::Unchecked(reason) => Some(format!("X-Virus-Scanned: no ({reason})\r\n")),
    }
}

/// clamd's own words end up in an SMTP reply and in the history, so only plain text passes.
fn plain(name: &str) -> String {
    let kept: String = name.chars().filter(|c| c.is_ascii_graphic() || *c == ' ').take(80).collect();
    let kept = kept.trim();
    if kept.is_empty() { "a virus".to_owned() } else { kept.to_owned() }
}

/// Turns clamd's answer to one message into a verdict.
fn verdict(answer: &str) -> Result<Scan, String> {
    let line = answer.trim();
    if line.ends_with("ERROR") {
        // The one error that is about the message and not about the scanner.
        if line.contains("size limit exceeded") {
            return Ok(Scan::TooBig);
        }
        return Err(line.trim_end_matches("ERROR").trim().trim_end_matches(':').to_owned());
    }
    let Some(rest) = line.strip_prefix("stream:") else {
        return Err(format!("the virus scanner answered \"{line}\""));
    };
    let rest = rest.trim();
    if rest == "OK" {
        return Ok(Scan::Clean);
    }
    match rest.strip_suffix("FOUND") {
        Some(name) => Ok(Scan::Found(name.trim().to_owned())),
        None => Err(format!("the virus scanner answered \"{line}\"")),
    }
}

/// The date clamd puts into its version line, e.g. `Wed Sep 17 08:32:11 2026`, in unix seconds.
fn built_at(date: &str) -> Option<i64> {
    let parts: Vec<&str> = date.split_whitespace().collect();
    let [_weekday, month, day, time, year] = parts.as_slice() else { return None };
    let months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let month = months.iter().position(|name| name.eq_ignore_ascii_case(month))? as i64 + 1;
    let day: i64 = day.parse().ok()?;
    let year: i64 = year.parse().ok()?;
    let clock: Vec<i64> = time.split(':').map(|part| part.parse().unwrap_or(-1)).collect();
    let [hour, minute, second] = clock.as_slice() else { return None };
    // Nothing here is trusted enough to multiply without looking: a made-up year would run over
    // what an i64 holds. A date outside these bounds is not a date.
    if !(1970..=9999).contains(&year) || !(1..=31).contains(&day) {
        return None;
    }
    if !(0..=23).contains(hour) || !(0..=59).contains(minute) || !(0..=60).contains(second) {
        return None;
    }
    Some(days_since_epoch(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Days from 1970-01-01 to that date, after Howard Hinnant's `days_from_civil`.
fn days_since_epoch(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;

    use super::*;

    fn config(address: String) -> AntivirusConfig {
        AntivirusConfig { enabled: true, address, timeout_secs: 5, max_size: 1024 }
    }

    #[test]
    fn answers_become_verdicts() {
        assert_eq!(verdict("stream: OK"), Ok(Scan::Clean));
        assert_eq!(verdict("stream: Eicar-Test-Signature FOUND"), Ok(Scan::Found("Eicar-Test-Signature".into())));
        assert_eq!(verdict("INSTREAM size limit exceeded. ERROR"), Ok(Scan::TooBig));
        // Anything else means nobody looked, which must never read as clean.
        assert!(verdict("Unknown command. ERROR").is_err());
        assert!(verdict("").is_err());
        assert!(verdict("stream: something odd").is_err());
    }

    #[tokio::test]
    async fn a_scanner_that_is_off_or_away_never_calls_a_message_clean() {
        let mut off = config(String::new());
        off.enabled = false;
        assert_eq!(check(&off, b"whatever").await, Checked::Off);
        assert_eq!(header(&Checked::Off), None);

        let away = config("127.0.0.1:1".into());
        let checked = check(&away, b"whatever").await;
        assert_eq!(checked, Checked::Unchecked("the virus scanner did not answer"));
        assert_eq!(header(&checked).unwrap(), "X-Virus-Scanned: no (the virus scanner did not answer)\r\n");
        assert_eq!(header(&Checked::Clean).unwrap(), "X-Virus-Scanned: yes (ClamAV)\r\n");

        // A message with a virus is refused, so it never carries a header of ours.
        assert_eq!(header(&Checked::Found("Eicar-Test-Signature".into())), None);
        // Whatever clamd calls it, it cannot break the SMTP reply or the history entry.
        assert_eq!(plain("Eicar-Test-Signature"), "Eicar-Test-Signature");
        assert_eq!(plain("bad\r\n550 nothing here"), "bad550 nothing here");
        assert_eq!(plain("\r\n"), "a virus");
    }

    #[test]
    fn the_signature_date_is_read() {
        // What a real clamd 1.5.4 answered on the test instance, 18 September 2026.
        assert_eq!(built_at("Fri Sep 18 06:25:28 2026"), Some(1_789_712_728));
        assert_eq!(built_at("Thu Jan 1 00:00:00 1970"), Some(0));
        assert_eq!(built_at("Wed Sep 17 08:32:11 2026"), Some(1_789_633_931));
        assert_eq!(built_at("Wed Sep 17 2026"), None);
        assert_eq!(built_at("Wed Sep 17 08:xx:11 2026"), None);
        // A made-up date is refused instead of being multiplied into an overflow.
        assert_eq!(built_at("Wed Sep 17 08:32:11 9223372036854775807"), None);
        assert_eq!(built_at("Wed Sep 17 9223372036854775807:32:11 2026"), None);
        assert_eq!(built_at("Wed Sep 999 08:32:11 2026"), None);
        assert_eq!(built_at("Wed Mai 17 08:32:11 2026"), None);
    }

    /// A stand-in clamd that answers `reply` to whatever it is asked.
    async fn fake(reply: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut command = Vec::new();
                let mut byte = [0u8; 1];
                while let Ok(1) = stream.read(&mut byte).await {
                    if byte[0] == 0 {
                        break;
                    }
                    command.push(byte[0]);
                }
                // INSTREAM keeps going until a chunk announces no bytes at all.
                if command == b"zINSTREAM" {
                    loop {
                        let mut length = [0u8; 4];
                        if stream.read_exact(&mut length).await.is_err() {
                            break;
                        }
                        let length = u32::from_be_bytes(length) as usize;
                        if length == 0 {
                            break;
                        }
                        let mut chunk = vec![0u8; length];
                        if stream.read_exact(&mut chunk).await.is_err() {
                            break;
                        }
                    }
                }
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.write_all(b"\0").await;
            }
        });
        address
    }

    #[tokio::test]
    async fn messages_travel_to_the_scanner_and_come_back_judged() {
        let clean = Clamav::new(&config(fake("stream: OK").await));
        assert_eq!(clean.scan(b"Subject: hi\r\n\r\nnothing here\r\n").await.unwrap(), Scan::Clean);
        // Larger than the scanner looks at: it is not asked at all, and nothing is claimed.
        assert_eq!(clean.scan(&vec![b'x'; 2048]).await.unwrap(), Scan::TooBig);

        let found = Clamav::new(&config(fake("stream: Eicar-Test-Signature FOUND").await));
        assert_eq!(found.scan(b"whatever").await.unwrap(), Scan::Found("Eicar-Test-Signature".into()));

        let version = Clamav::new(&config(fake("ClamAV 1.4.2/27700/Wed Sep 17 08:32:11 2026").await));
        let status = version.status().await.unwrap();
        assert_eq!((status.signatures, status.signatures_at), (Some(27700), Some(1_789_633_931)));

        // Nothing listening: an error, never a clean verdict.
        let away = Clamav::new(&config("127.0.0.1:1".into()));
        assert!(away.scan(b"whatever").await.is_err());
        let nowhere = Clamav::new(&config(String::new()));
        assert!(nowhere.status().await.is_err());
    }
}

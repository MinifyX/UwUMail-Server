//! What the gateway knows about the machine it runs on, and how it reaches the parts of the
//! installation that need root.
//!
//! The gateway has no rights on its machine on purpose: the systemd unit leaves it nothing but the
//! low ports, so it cannot ask apt what is missing and it cannot ban anyone. `install.sh` sets up
//! two small helpers that run as root and meet the gateway in its state directory:
//!
//! - `machine.json` — written by the helper, read here, and passed on to the server for the portal.
//! - `bans.jsonl` — written here, read by the helper, which hands each line to fail2ban.
//! - `trusted` — written here, read by both helpers: the addresses the tunnel comes from, which
//!   no ban may ever touch.
//! - `jobs.jsonl` — written here: what the portal asked this machine for. `job-<id>.log` and
//!   `job-<id>.json` come back the other way, and being files they survive the gateway being
//!   restarted by the very update it asked for.
//!
//! Every file is a plain one: nothing here can run a command, and a missing helper only means the
//! portal has less to show. What crosses over for a job is a verb from a fixed list and a version
//! number that has to look like one — never a command, a path or an address.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uwumail_tunnel::proto::GatewayStatus;

const REPORT: &str = "machine.json";
const BANS: &str = "bans.jsonl";
const TRUSTED: &str = "trusted";
const JOBS: &str = "jobs.jsonl";

/// The most of a task's output that is passed on to the portal at once.
const LOG_MAX: usize = 16 * 1024;
/// The verbs the gateway will pass on. The helper checks them again -- it is the side with the
/// rights -- but a word the gateway has never heard of never reaches a file at all.
const VERBS: [&str; 3] = ["os-update", "reboot", "gateway-update"];

/// Past this the report is too old to mean anything: the helper runs far more often, so an older
/// file says its timer stopped rather than that all is well.
const REPORT_STALE_SECS: i64 = 36 * 3600;
/// How long an address stays trusted after the tunnel was last seen on it. Short on purpose, and
/// only a grace period for a reconnect that flaps or a gateway that restarts — while the tunnel is
/// up, the gateway says so again every few minutes, so the time never runs out under a connection
/// that is in use.
///
/// It must stay short because the address is not the server's to keep: a home connection is handed
/// a new one every night, and the one it gave up goes to the next customer. Trusting it for hours
/// would hand that stranger hours of being unbannable. Nothing is lost by dropping it quickly,
/// because the server never needs the trust to get back in — bans are TCP only and the tunnel is
/// QUIC over UDP, so it reaches the gateway even from an address that is banned, and says where it
/// is now the moment it arrives.
const TRUSTED_SECS: i64 = 15 * 60;
/// A ban file that nobody empties means the helper is not running. Dropping the oldest lines keeps
/// a flood of failed logins from filling the disk.
const BANS_MAX_BYTES: u64 = 256 * 1024;

/// What the privileged helper reports about the machine, as it writes it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Report {
    written_at: i64,
    system: Option<uwumail_tunnel::proto::System>,
    protection: Option<uwumail_tunnel::proto::Protection>,
}

#[derive(Debug, Clone)]
pub struct Machine {
    dir: PathBuf,
}

impl Machine {
    pub fn new(dir: &Path) -> Machine {
        Machine { dir: dir.to_owned() }
    }

    /// The status for the server's portal. Without the helper this is still worth sending: it tells
    /// the portal which gateway version runs and which addresses it protects.
    pub fn status(&self, software: String) -> GatewayStatus {
        let report = self.report();
        GatewayStatus {
            software,
            system: report.as_ref().and_then(|report| report.system.clone()),
            protection: report.as_ref().and_then(|report| report.protection.clone()),
            trusted: self.trusted(),
            checked_at: report.map(|report| report.written_at).unwrap_or_default(),
            job: self.job(),
        }
    }

    /// Whether a helper is installed at all. Without one the portal keeps showing the commands.
    pub fn has_helper(&self) -> bool {
        self.dir.join(REPORT).is_file()
    }

    fn report(&self) -> Option<Report> {
        let bytes = std::fs::read(self.dir.join(REPORT)).ok()?;
        let report: Report = serde_json::from_slice(&bytes)
            .map_err(|err| tracing::warn!(%err, "the machine report could not be read"))
            .ok()?;
        // A report from a helper that stopped would keep saying everything is fine.
        (unix_now() - report.written_at < REPORT_STALE_SECS).then_some(report)
    }

    /// Notes that the tunnel comes from `ip`, so no ban may lock the server out. Addresses that
    /// have not been seen for [`TRUSTED_SECS`] fall out.
    pub fn trust(&self, ip: IpAddr) {
        let ip = ip.to_canonical();
        // Only ever the address in use, never a list that grows. The one the server dialled from
        // yesterday belongs to somebody else by now, and leaving it here would make that stranger
        // unbannable. Called again on every report while the tunnel is up, which keeps the time
        // below fresh; when the address changes, this line is simply the new one.
        //
        // Each line is "<address> <unix time> <range>". The helper matches the address itself for
        // the common case and hands the range to fail2ban, which does range matching properly
        // rather than by comparing text — IPv6 has too many ways to write the same address.
        let line = format!("{ip} {} {}\n", unix_now(), trusted_range(ip));
        if self.trusted_with_times().first().is_some_and(|(seen, at)| *seen == ip && unix_now() - at < 30) {
            // Nothing has changed and the note is fresh. Writing anyway would wake the helper
            // through its path unit for no reason.
            return;
        }
        if let Err(err) = write_replacing(&self.dir.join(TRUSTED), line.as_bytes()) {
            // Only a warning: the tunnel works, but a ban could now reach the server.
            tracing::warn!(%err, "could not write the trusted address; the firewall may not know this server");
        }
    }

    /// The addresses no ban may touch, newest first.
    pub fn trusted(&self) -> Vec<IpAddr> {
        self.trusted_with_times().into_iter().map(|(ip, _)| ip).collect()
    }

    /// Whether `ip` is the server's own address, or sits in the same /64 as one of them.
    pub fn is_trusted(&self, ip: IpAddr) -> bool {
        self.trusted_with_times().into_iter().any(|(trusted, _)| covers(trusted, ip))
    }

    fn trusted_with_times(&self) -> Vec<(IpAddr, i64)> {
        let Ok(text) = std::fs::read_to_string(self.dir.join(TRUSTED)) else {
            return Vec::new();
        };
        let now = unix_now();
        text.lines()
            .filter_map(|line| {
                // "<address> <unix time> <range>"; the range is for the helper, not for here.
                let mut fields = line.split_whitespace();
                Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
            })
            .filter(|(_, at)| now - at < TRUSTED_SECS)
            .collect()
    }

    /// Hands a ban to the helper. Returns whether it was written; a refused address is not an
    /// error, it is the point.
    pub fn ban(&self, ip: IpAddr, seconds: u32, reason: &str) -> bool {
        self.write_ban(&Ban { action: "ban", ip, seconds, reason: clean_reason(reason), at: unix_now() })
    }

    pub fn unban(&self, ip: IpAddr) -> bool {
        self.write_ban(&Ban { action: "unban", ip, seconds: 0, reason: String::new(), at: unix_now() })
    }

    /// Hands a task to the helper. Says whether it was written down.
    ///
    /// Nothing here is ever run: the verb has to be one of [`VERBS`], the id has to be letters and
    /// digits because it names a file, and the version has to look like a version. What reaches the
    /// helper is three checked words in a JSON line, and the helper checks them all over again.
    pub fn ask(&self, id: &str, verb: &str, version: Option<&str>) -> bool {
        if !VERBS.contains(&verb) || !is_id(id) {
            tracing::warn!(%verb, "the server asked for something this gateway does not do");
            return false;
        }
        if version.is_some_and(|version| !is_version(version)) {
            tracing::warn!("the server named a version that is not one");
            return false;
        }
        if self.job().is_some_and(|job| job.state == "running") {
            tracing::info!("something is already running on this machine");
            return false;
        }
        let job = Job { id, verb, version, at: unix_now() };
        let Ok(mut line) = serde_json::to_vec(&job) else { return false };
        line.push(b'\n');
        let path = self.dir.join(JOBS);
        // Opened fresh each time, like the bans: the helper carries the file off by renaming it.
        match OpenOptions::new().create(true).append(true).open(&path).and_then(|mut file| file.write_all(&line)) {
            Ok(()) => {
                let _ = std::fs::remove_file(self.dir.join(format!("job-{id}.log")));
                tracing::info!(%verb, %id, "the server asked this machine for a job");
                true
            }
            Err(err) => {
                tracing::warn!(%err, "could not write down what the server asked for");
                false
            }
        }
    }

    /// The task asked for last, with whatever it has printed so far.
    fn job(&self) -> Option<uwumail_tunnel::proto::TaskState> {
        let id = self.newest_job()?;
        let answer: JobAnswer = std::fs::read(self.dir.join(format!("job-{id}.json")))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(JobAnswer { id: id.clone(), state: "running".into(), ..JobAnswer::default() });
        let log = std::fs::read_to_string(self.dir.join(format!("job-{id}.log")))
            .map(|text| match text.char_indices().nth_back(LOG_MAX) {
                Some((at, _)) => text[at..].to_owned(),
                None => text,
            })
            .unwrap_or_default();
        Some(uwumail_tunnel::proto::TaskState { id, state: answer.state, error: answer.error, at: answer.at, log })
    }

    /// The task whose files were touched last. After a gateway restart -- which an update causes --
    /// this is how the answer is found again.
    fn newest_job(&self) -> Option<String> {
        let mut newest: Option<(std::time::SystemTime, String)> = None;
        for entry in std::fs::read_dir(&self.dir).ok()?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let id =
                name.strip_prefix("job-").and_then(|rest| rest.strip_suffix(".json").or(rest.strip_suffix(".log")));
            let Some(id) = id else { continue };
            let Ok(at) = entry.metadata().and_then(|data| data.modified()) else { continue };
            if newest.as_ref().is_none_or(|(seen, _)| at > *seen) {
                newest = Some((at, id.to_owned()));
            }
        }
        newest.map(|(_, id)| id)
    }

    fn write_ban(&self, ban: &Ban) -> bool {
        let path = self.dir.join(BANS);
        // A file nobody empties means the helper is gone; writing on would only fill the disk.
        if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > BANS_MAX_BYTES) {
            tracing::warn!("bans are piling up; the gateway's fail2ban helper does not seem to run");
            return false;
        }
        let Ok(mut line) = serde_json::to_vec(ban) else {
            return false;
        };
        line.push(b'\n');
        // Opened fresh each time: the helper takes the file away by renaming it, and an old handle
        // would keep writing into the copy it already carried off.
        match OpenOptions::new().create(true).append(true).open(&path).and_then(|mut file| file.write_all(&line)) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!(%err, "could not pass a ban to fail2ban");
                false
            }
        }
    }
}

/// What the helper writes back about a task, as it writes it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct JobAnswer {
    id: String,
    state: String,
    error: String,
    at: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Job<'a> {
    id: &'a str,
    verb: &'a str,
    version: Option<&'a str>,
    at: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Ban {
    action: &'static str,
    ip: IpAddr,
    seconds: u32,
    reason: String,
    at: i64,
}

/// How much around an address the tunnel came from is trusted with it.
///
/// IPv6: the whole /64. That is what one home connection is handed, the server moves around inside
/// it on its own — privacy extensions hand out a new address every day — and everything in it
/// belongs to the same household anyway.
///
/// IPv4: the single address, deliberately. Behind carrier-grade NAT a whole neighbourhood shares
/// one, and trusting a range there would mean never banning any of them.
fn trusted_range(ip: IpAddr) -> String {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let prefix = std::net::Ipv6Addr::from(u128::from(v6) & (u128::MAX << 64));
            format!("{prefix}/64")
        }
        v4 => format!("{v4}/32"),
    }
}

/// Whether `candidate` is covered by a trusted address: the same one, or inside the same /64.
fn covers(trusted: IpAddr, candidate: IpAddr) -> bool {
    let (trusted, candidate) = (trusted.to_canonical(), candidate.to_canonical());
    if trusted == candidate {
        return true;
    }
    match (trusted, candidate) {
        (IpAddr::V6(a), IpAddr::V6(b)) => (u128::from(a) ^ u128::from(b)) >> 64 == 0,
        _ => false,
    }
}

/// An id names a file, so it may only be what an id is made of.
fn is_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40 && id.chars().all(|letter| letter.is_ascii_alphanumeric())
}

/// `1.2.3` or `1.2.3-beta.4`, and nothing else. The helper builds the address it downloads from out
/// of its own constants and this; anything that is not a version has no business getting that far.
fn is_version(version: &str) -> bool {
    let (core, pre) = version.split_once('-').map_or((version, None), |(core, pre)| (core, Some(pre)));
    let numbers: Vec<&str> = core.split('.').collect();
    if numbers.len() != 3
        || !numbers
            .iter()
            .all(|part| !part.is_empty() && part.len() <= 4 && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return false;
    }
    match pre {
        None => true,
        Some(pre) => {
            !pre.is_empty() && pre.len() <= 20 && pre.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'.')
        }
    }
}

/// The reason goes to a root helper and from there into a log, so it stays short and plain.
fn clean_reason(reason: &str) -> String {
    reason.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.')).take(60).collect()
}

/// Writes through a temporary file, so a crash never leaves half a list of trusted addresses
/// behind — half a list could lock the server out.
fn write_replacing(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_address_in_use_is_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let machine = Machine::new(dir.path());
        assert!(machine.trusted().is_empty());

        let yesterday: IpAddr = "192.0.2.7".parse().unwrap();
        let today: IpAddr = "198.51.100.4".parse().unwrap();
        machine.trust(yesterday);
        assert_eq!(machine.trusted(), vec![yesterday]);

        // The nightly reconnect. Yesterday's address now belongs to somebody else, and keeping it
        // would make that somebody unbannable.
        machine.trust(today);
        assert_eq!(machine.trusted(), vec![today], "the old one is gone, not kept beside the new one");
        assert!(!machine.is_trusted(yesterday));

        // A note nobody refreshed: the tunnel says where it is every few minutes, so this means
        // the tunnel is not there any more.
        let old = unix_now() - TRUSTED_SECS - 60;
        std::fs::write(dir.path().join(TRUSTED), format!("198.51.100.4 {old} 198.51.100.4/32\n")).unwrap();
        assert!(machine.trusted().is_empty());
    }

    #[test]
    fn a_home_connection_is_trusted_across_its_whole_ipv6_range() {
        let dir = tempfile::tempdir().unwrap();
        let machine = Machine::new(dir.path());
        machine.trust("2001:db8:1234:5678::5".parse().unwrap());

        // The server moves around inside its own /64 — privacy extensions do that daily.
        assert!(machine.is_trusted("2001:db8:1234:5678::5".parse().unwrap()));
        assert!(machine.is_trusted("2001:db8:1234:5678:aaaa:bbbb:cccc:dddd".parse().unwrap()));
        assert!(!machine.is_trusted("2001:db8:1234:9999::5".parse().unwrap()), "another /64 is someone else");
        let written = std::fs::read_to_string(dir.path().join(TRUSTED)).unwrap();
        assert!(written.contains("2001:db8:1234:5678::/64"), "the range fail2ban is given");

        // IPv4 stays exact: behind carrier-grade NAT the neighbours share the address.
        machine.trust("198.51.100.47".parse().unwrap());
        assert!(machine.is_trusted("198.51.100.47".parse().unwrap()));
        assert!(machine.is_trusted("::ffff:198.51.100.47".parse().unwrap()), "however it is written");
        assert!(!machine.is_trusted("198.51.100.48".parse().unwrap()), "not the whole range");
        let written = std::fs::read_to_string(dir.path().join(TRUSTED)).unwrap();
        assert!(written.contains("198.51.100.47/32"));
    }

    #[test]
    fn bans_are_written_as_lines_until_the_helper_stops_reading() {
        let dir = tempfile::tempdir().unwrap();
        let machine = Machine::new(dir.path());
        assert!(machine.ban("192.0.2.7".parse().unwrap(), 3600, "imap: 10 wrong passwords\n\rand a newline"));
        assert!(machine.unban("192.0.2.7".parse().unwrap()));

        let written = std::fs::read_to_string(dir.path().join(BANS)).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(!written.contains('\r'), "nothing that could forge a second line");
        let ban: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(ban["action"], "ban");
        assert_eq!(ban["ip"], "192.0.2.7");
        assert_eq!(ban["reason"], "imap 10 wrong passwordsand a newline");

        std::fs::write(dir.path().join(BANS), vec![b'x'; BANS_MAX_BYTES as usize + 1]).unwrap();
        assert!(!machine.ban("192.0.2.8".parse().unwrap(), 60, "no"), "stops instead of filling the disk");
    }

    #[test]
    fn a_report_nobody_refreshes_is_not_shown_as_news() {
        let dir = tempfile::tempdir().unwrap();
        let machine = Machine::new(dir.path());
        let status = machine.status("uwumail-gateway 1.0.0".into());
        assert_eq!(status.software, "uwumail-gateway 1.0.0");
        assert!(status.system.is_none(), "without the helper there is nothing to say about the machine");

        let fresh = serde_json::json!({
            "writtenAt": unix_now(),
            "system": { "name": "Ubuntu 26.04.1 LTS", "updates": 12, "securityUpdates": 3 },
        });
        std::fs::write(dir.path().join(REPORT), fresh.to_string()).unwrap();
        let system = machine.status(String::new()).system.expect("the report is fresh");
        assert_eq!(system.name, "Ubuntu 26.04.1 LTS");
        assert_eq!(system.security_updates, 3);
        assert!(!system.reboot_required, "fields the helper left out take their defaults");

        let stale = serde_json::json!({ "writtenAt": unix_now() - REPORT_STALE_SECS - 60, "system": { "updates": 1 } });
        std::fs::write(dir.path().join(REPORT), stale.to_string()).unwrap();
        assert!(machine.status(String::new()).system.is_none(), "a stopped helper must not look reassuring");
    }

    #[test]
    fn only_three_words_and_a_version_ever_reach_the_helper() {
        let dir = tempfile::tempdir().unwrap();
        let machine = Machine::new(dir.path());

        assert!(machine.ask("abc1", "os-update", None));
        assert!(machine.ask("abc2", "gateway-update", Some("0.2.3")));
        assert!(machine.ask("abc3", "gateway-update", Some("1.0.0-beta.2")));

        // Everything that is not one of the three words, or not a version.
        assert!(!machine.ask("abc4", "rm -rf /", None), "not a verb");
        assert!(!machine.ask("abc5", "server-update", None), "the server's verb, not this machine's");
        assert!(!machine.ask("abc6", "gateway-update", Some("0.2.3; id")), "a command after the version");
        assert!(!machine.ask("abc7", "gateway-update", Some("$(id)")), "a substitution instead of a version");
        assert!(!machine.ask("abc8", "gateway-update", Some("../../etc/passwd")), "a path instead of a version");
        assert!(!machine.ask("../../etc/passwd", "os-update", None), "an id that is a path");
        assert!(!machine.ask("", "os-update", None), "no id at all");

        // Only what passed is on the way over, and in the order it was asked for.
        let written = std::fs::read_to_string(dir.path().join(JOBS)).unwrap();
        let ids: Vec<String> = written
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .map(|line| line["id"].as_str().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(ids, ["abc1", "abc2", "abc3"]);

        // While one runs, nothing else starts.
        std::fs::write(dir.path().join("job-abc3.json"), r#"{"id":"abc3","state":"running"}"#).unwrap();
        assert!(!machine.ask("abc9", "os-update", None), "one at a time");
        let job = machine.status(String::new()).job.expect("the running one is reported");
        assert_eq!((job.id.as_str(), job.state.as_str()), ("abc3", "running"));
    }
}

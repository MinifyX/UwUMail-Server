//! The server's side of the directory it shares with the helper on the machine (`deploy/host/`).
//!
//! The container can do nothing to the machine itself, and should not be able to: it is distroless,
//! read-only, unprivileged and has every capability dropped but the one it needs for the low ports.
//! So it writes what it would like done into a file, and a small root helper beside it does the
//! deed and writes back what came of it.
//!
//! Files, not a socket, for one reason that decides it: installing the system's updates can renew
//! docker itself, and recreating the container is the whole point of an update. Either one takes
//! down whoever is holding a connection. A file that a systemd unit finishes writing survives both,
//! so the answer is still there when the new container comes up and looks.
//!
//! Without a helper the directory is simply not there, and the portal goes on showing the commands
//! to copy, exactly as before.

use std::path::PathBuf;
use std::sync::Mutex;

use uwumail_web::host::{HostBackend, HostFuture, HostJob, HostMachine, HostView};

/// Where the shared directory is mounted in the container.
const DEFAULT_DIR: &str = "/host";
/// The most of a job's output the portal is handed at once.
const LOG_MAX: usize = 64 * 1024;
/// A machine report older than this means the helper stopped writing, and saying nothing is better
/// than saying something a week old as if it were now.
const STALE_SECS: i64 = 6 * 3600;

pub struct HostBridge {
    dir: PathBuf,
    /// The job asked for last, so the portal can follow it. Picked up again after a restart, which
    /// is the case this whole arrangement exists for.
    current: Mutex<Option<String>>,
}

impl HostBridge {
    /// The bridge, if a helper made one. `UWUMAIL_HOST_DIR` moves it, for testing.
    pub fn find() -> Option<HostBridge> {
        let dir = std::env::var("UWUMAIL_HOST_DIR").unwrap_or_else(|_| DEFAULT_DIR.into());
        let dir = PathBuf::from(dir);
        if !dir.is_dir() {
            return None;
        }
        let bridge = HostBridge { dir, current: Mutex::new(None) };
        *bridge.current.lock().expect("host job poisoned") = bridge.newest_job();
        tracing::info!(dir = %bridge.dir.display(), "the machine's helper is there");
        Some(bridge)
    }

    /// The job whose answer was written last. After a restart this is how the portal learns how the
    /// update that replaced it ended.
    fn newest_job(&self) -> Option<String> {
        let mut newest: Option<(std::time::SystemTime, String)> = None;
        for entry in std::fs::read_dir(&self.dir).ok()?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_prefix("job-").and_then(|rest| rest.strip_suffix(".json")) else { continue };
            let Ok(at) = entry.metadata().and_then(|data| data.modified()) else { continue };
            if newest.as_ref().is_none_or(|(seen, _)| at > *seen) {
                newest = Some((at, id.to_owned()));
            }
        }
        newest.map(|(_, id)| id)
    }

    fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join(name)).ok()
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64)
}

/// An id that names a file, so nothing but letters and digits. It only has to be one of a kind, not
/// hard to guess: the shared directory belongs to root and this container, and whoever can write
/// there already has everything this could protect. The helper checks the shape again anyway,
/// because it is the side with the rights.
fn job_id() -> String {
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_nanos() as u64);
    format!("{nanos:016x}{:04x}", std::process::id() & 0xffff)
}

impl HostBackend for HostBridge {
    fn view(&self) -> HostView {
        let machine: Option<HostMachine> = self
            .read("machine.json")
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .filter(|machine: &HostMachine| {
                // A report nobody refreshed is not news. Better to show nothing than something old
                // dressed up as current.
                machine.checked_at > 0 && unix_now() - machine.checked_at < STALE_SECS
            });
        let id = self.current.lock().expect("host job poisoned").clone();
        let job: Option<HostJob> = id
            .as_ref()
            .and_then(|id| self.read(&format!("job-{id}.json")))
            .and_then(|raw| serde_json::from_str(&raw).ok());
        let log = id
            .as_ref()
            .and_then(|id| self.read(&format!("job-{id}.log")))
            .map(|text| match text.char_indices().nth_back(LOG_MAX) {
                Some((at, _)) => text[at..].to_owned(),
                None => text,
            })
            .unwrap_or_default();
        HostView { available: true, machine, job, log, command: String::new() }
    }

    fn ask<'a>(&'a self, verb: &'a str) -> HostFuture<'a> {
        Box::pin(async move {
            if !matches!(verb, "os-update" | "reboot") {
                return Err(format!("this server does not ask for {verb}"));
            }
            // One at a time. Two updates at once is never what anyone meant.
            if let Some(job) = self.view().job
                && job.state == "running"
            {
                return Err("something is already running on this machine".into());
            }
            let id = job_id();
            let line = serde_json::json!({ "id": id, "verb": verb, "at": unix_now() });
            let path = self.dir.join("jobs.jsonl");
            let mut text = format!("{line}\n");
            if let Ok(waiting) = std::fs::read_to_string(&path) {
                text = format!("{waiting}{text}");
            }
            // Written whole and then moved into place: the helper watches this file, and half a
            // line would be a job nobody can read.
            let tmp = path.with_extension("jsonl.tmp");
            std::fs::write(&tmp, &text).map_err(|err| format!("the job could not be written: {err}"))?;
            std::fs::rename(&tmp, &path).map_err(|err| format!("the job could not be handed over: {err}"))?;
            // No answer file yet means "asked for, not started"; the portal shows that as waiting.
            let _ = std::fs::remove_file(self.dir.join(format!("job-{id}.log")));
            *self.current.lock().expect("host job poisoned") = Some(id.clone());
            tracing::info!(%verb, %id, "asked the machine's helper for a job");
            Ok(id)
        })
    }
}

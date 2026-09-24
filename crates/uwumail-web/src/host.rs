//! The machine the server runs on, as far as the portal can see it.
//!
//! The container itself cannot see any of this: it is distroless, read-only, unprivileged and has
//! every capability dropped but the one it needs for the low ports, which is exactly what makes a
//! break-in worth little. A small helper beside it can (`deploy/host/`), and the two share a
//! directory. Without that helper everything here is simply absent, and the portal goes on showing
//! the commands to copy, as it always did.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

/// What the helper wrote down about the machine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HostMachine {
    /// `debian` for a machine apt can update, `unknown` for anything else.
    pub kind: String,
    /// As the machine calls itself, e.g. `Ubuntu 24.04.1 LTS`.
    pub name: String,
    pub updates: u32,
    pub security_updates: u32,
    pub reboot_required: bool,
    /// What asked for the restart.
    pub reboot_packages: Vec<String>,
    /// Whether UwUMail has this machine to itself. `None` means the helper could not tell, which is
    /// not the same as yes and is never shown as yes.
    pub alone: Option<bool>,
    /// What else runs here, when the helper could see it.
    pub others: Vec<String>,
    pub image: String,
    pub digest: String,
    pub compose_dir: String,
    pub checked_at: i64,
    /// The helper's version; 2 and later know the VPN.
    pub helper: String,
    /// What the helper is willing to do.
    pub verbs: Vec<String>,
    /// The gluetun container, when the helper knows about it.
    pub vpn: Option<HostVpn>,
}

/// The VPN container beside the server, as the helper sees it. Never any of its keys.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HostVpn {
    /// Whether `.env.vpn` exists.
    pub configured: bool,
    pub provider: String,
    #[serde(rename = "type")]
    pub kind: String,
    /// Docker's word for it: `running`, `exited`, ..., or `missing` without a container.
    pub state: String,
    /// `healthy`, `starting`, `unhealthy` or empty.
    pub health: String,
    /// Whether it comes along with every `docker compose up -d`.
    pub always: bool,
}

/// A job the helper is carrying out, or has.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HostJob {
    pub id: String,
    /// `waiting` (asked for, not started yet), `running`, `done`, `failed` or `rolledBack`.
    pub state: String,
    pub error: String,
    pub at: i64,
}

/// What the portal shows about the machine.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostView {
    /// Whether a helper is installed and the two can see each other at all.
    pub available: bool,
    pub machine: Option<HostMachine>,
    /// The job that was asked for last, while there is one.
    pub job: Option<HostJob>,
    /// What the job has printed so far, capped.
    pub log: String,
    /// The command to run by hand, for a machine without a helper.
    pub command: String,
}

pub type HostFuture<'a> = Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;

/// The helper on the machine, as the portal may use it.
pub trait HostBackend: Send + Sync + 'static {
    fn view(&self) -> HostView;
    /// Asks for `os-update`, `reboot`, `vpn-apply` or `vpn-stop`; the mail server itself is updated on
    /// the machine, with update.sh. Returns the id of the job, to follow it with [`HostBackend::view`].
    fn ask<'a>(&'a self, verb: &'a str) -> HostFuture<'a>;
    /// Puts a file into the shared directory for the next job to read, e.g. `vpn.json`.
    fn hand_over(&self, name: &'static str, contents: &str) -> Result<(), String> {
        let _ = (name, contents);
        Err("this helper takes no files".into())
    }
}

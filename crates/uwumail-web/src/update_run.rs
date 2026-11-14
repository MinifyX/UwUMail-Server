//! An update run: the steps that bring the gateway's machine, the gateway, this machine, its helper
//! and the server up to date, one after the other, from one button in the portal.
//!
//! The server never does any of it itself. Each step is one job for a helper that runs as root --
//! the one beside the container (`deploy/host/`), or the one on the gateway's VPS through the tunnel
//! -- and all the server ever hands over is a word from a fixed list. What is new here is the order
//! and the memory: the run is saved, so it goes on after the server was replaced by its own update,
//! and it stops at the first step that fails instead of piling the next one on top.
//!
//! A restart of a machine is a step like any other, but never part of "everything": the portal asks
//! first, every time.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::Web;
use crate::health::unix_now;
use crate::host::HELPER_VERSION;
use crate::updates::{build, compare_versions, tag};

const RUN_KEY: &str = "updates.run";
/// How long a job may stay silent. The helpers give up after 45 minutes themselves.
const ANSWER_SECS: i64 = 50 * 60;
const LOG_MAX: usize = 16 * 1024;

/// The steps, in the order a run takes them: the gateway first, because the server's own update
/// ends the process that would ask for the others; restarts last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepKind {
    GatewaySystem,
    Gateway,
    HostSystem,
    Helper,
    Server,
    GatewayReboot,
    HostReboot,
}

impl StepKind {
    fn on_gateway(self) -> bool {
        matches!(self, StepKind::GatewaySystem | StepKind::Gateway | StepKind::GatewayReboot)
    }

    /// The helper's word for it.
    fn verb(self) -> &'static str {
        match self {
            StepKind::GatewaySystem | StepKind::HostSystem => "os-update",
            StepKind::GatewayReboot | StepKind::HostReboot => "reboot",
            StepKind::Gateway => "gateway-update",
            StepKind::Helper => "helper-update",
            StepKind::Server => "uwumail-update",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepState {
    #[default]
    Pending,
    Running,
    Done,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub kind: StepKind,
    #[serde(default)]
    pub state: StepState,
    /// The helper's job, once asked for.
    #[serde(default)]
    pub job: String,
    #[serde(default)]
    pub error: String,
    /// Why it failed, where the portal can offer more than "again": `compose` (the compose file
    /// was changed by hand), `rolledBack` or `timeout`.
    #[serde(default)]
    pub reason: String,
    /// What the job printed, capped.
    #[serde(default)]
    pub log: String,
    #[serde(default)]
    pub started_at: i64,
    /// The admin's answer to `compose`: `keep` or `replace`.
    #[serde(default)]
    pub compose: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// Who pressed the button.
    pub by: String,
    pub steps: Vec<Step>,
}

/// What the admin does with a step that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepAction {
    Retry,
    Skip,
    /// Give the rest of the run up.
    Stop,
}

fn tail(text: String) -> String {
    if text.len() <= LOG_MAX {
        return text;
    }
    let mut at = text.len() - LOG_MAX;
    while !text.is_char_boundary(at) {
        at += 1;
    }
    text[at..].to_owned()
}

impl Web {
    pub(crate) async fn update_run(&self) -> Option<Run> {
        let raw = self.store().setting(RUN_KEY).await.ok().flatten()?;
        serde_json::from_str(&raw).ok()
    }

    async fn save_run(&self, run: &Run) {
        let raw = serde_json::to_string(run).expect("run serializes");
        if let Err(err) = self.store().set_setting(RUN_KEY, &raw).await {
            tracing::warn!(%err, "saving the update run failed");
        }
    }

    /// Which steps there is something to do for (what "everything" means right now), and which
    /// ones can be asked for at all.
    pub(crate) async fn update_steps(&self) -> (Vec<StepKind>, Vec<StepKind>) {
        let (mut plan, mut possible) = (Vec::new(), Vec::new());
        let mut add = |kind: StepKind, can: bool, waiting: bool| {
            if can {
                possible.push(kind);
                if waiting {
                    plan.push(kind);
                }
            }
        };

        let gateway = self.gateway().map(|gateway| gateway.view()).filter(|view| view.can_install);
        let system =
            gateway.as_ref().and_then(|view| view.machine.as_ref()).and_then(|machine| machine.system.as_ref());
        add(StepKind::GatewaySystem, system.is_some(), system.is_some_and(|system| system.updates > 0));
        let newer = match &gateway {
            Some(view) => crate::routes::gateway::newer_gateway(self, view.software.as_deref()).await,
            None => None,
        };
        add(StepKind::Gateway, newer.is_some(), true);

        let machine = self.host().and_then(|host| host.view().machine);
        let can = |verb: &str| machine.as_ref().is_some_and(|machine| machine.can(verb));
        let debian = machine.as_ref().is_some_and(|machine| machine.kind == "debian");
        add(
            StepKind::HostSystem,
            can("os-update") && debian,
            machine.as_ref().is_some_and(|machine| machine.updates > 0),
        );
        let old =
            machine.as_ref().is_some_and(|machine| machine.helper.parse::<u32>().is_ok_and(|v| v < HELPER_VERSION));
        add(StepKind::Helper, can("helper-update") && old, true);
        let info = self.update_info().await;
        let news = info.behind.unwrap_or(0) > 0
            || info
                .releases
                .iter()
                .any(|release| compare_versions(&release.version, build().version) == Ordering::Greater);
        add(StepKind::Server, can("uwumail-update"), news);

        add(StepKind::GatewayReboot, system.is_some(), false);
        add(StepKind::HostReboot, can("reboot"), false);
        (plan, possible)
    }

    /// Starts a run over `steps`, or over everything that waits. One run at a time.
    pub(crate) async fn start_update_run(&self, steps: Option<Vec<StepKind>>, by: &str) -> Result<Run, String> {
        let _turn = self.update_turn().lock().await;
        if self.update_run().await.is_some_and(|run| run.finished_at.is_none()) {
            return Err("an update is already running".into());
        }
        let (plan, possible) = self.update_steps().await;
        let mut kinds = steps.unwrap_or(plan);
        kinds.sort();
        kinds.dedup();
        if kinds.is_empty() {
            return Err("there is nothing to update".into());
        }
        if let Some(kind) = kinds.iter().find(|kind| !possible.contains(kind)) {
            return Err(format!("{kind:?} cannot be done from here right now"));
        }
        let steps = kinds
            .into_iter()
            .map(|kind| Step {
                kind,
                state: StepState::Pending,
                job: String::new(),
                error: String::new(),
                reason: String::new(),
                log: String::new(),
                started_at: 0,
                compose: String::new(),
            })
            .collect();
        let mut run = Run { started_at: unix_now(), finished_at: None, by: by.to_owned(), steps };
        self.walk(&mut run).await;
        self.save_run(&run).await;
        Ok(run)
    }

    /// Looks after the run: follows the step that runs and starts the next one. Called every few
    /// seconds by the server and whenever the portal looks.
    pub async fn advance_update_run(&self) -> Option<Run> {
        let _turn = self.update_turn().lock().await;
        let mut run = self.update_run().await?;
        if run.finished_at.is_none() {
            let before = serde_json::to_string(&run).ok();
            self.walk(&mut run).await;
            if serde_json::to_string(&run).ok() != before {
                self.save_run(&run).await;
            }
        }
        Some(run)
    }

    /// The admin's answer to a step that failed.
    pub(crate) async fn answer_update_step(&self, action: StepAction, compose: Option<&str>) -> Result<Run, String> {
        let _turn = self.update_turn().lock().await;
        let mut run = self.update_run().await.filter(|run| run.finished_at.is_none()).ok_or("no update is running")?;
        let step = run
            .steps
            .iter_mut()
            .find(|step| step.state == StepState::Failed)
            .ok_or("no step is waiting for an answer")?;
        match action {
            StepAction::Retry => {
                step.state = StepState::Pending;
                step.compose = match compose {
                    Some(choice @ ("keep" | "replace")) => choice.to_owned(),
                    Some(_) => return Err("unknown choice for the compose file".into()),
                    None => String::new(),
                };
            }
            StepAction::Skip => step.state = StepState::Skipped,
            StepAction::Stop => {
                for step in run.steps.iter_mut().filter(|step| step.state != StepState::Done) {
                    step.state = StepState::Skipped;
                }
            }
        }
        self.walk(&mut run).await;
        self.save_run(&run).await;
        Ok(run)
    }

    async fn walk(&self, run: &mut Run) {
        loop {
            let Some(step) =
                run.steps.iter_mut().find(|step| !matches!(step.state, StepState::Done | StepState::Skipped))
            else {
                run.finished_at = Some(unix_now());
                return;
            };
            match step.state {
                StepState::Pending => match self.ask_step(step).await {
                    Ok(job) => {
                        step.state = StepState::Running;
                        step.job = job;
                        step.started_at = unix_now();
                        step.error.clear();
                        step.reason.clear();
                        step.log.clear();
                        return;
                    }
                    Err(error) => {
                        step.state = StepState::Failed;
                        step.error = error;
                        return;
                    }
                },
                StepState::Running => {
                    self.follow_step(step);
                    // Also a helper that began and then died must not hold the run for ever.
                    if step.state == StepState::Running && unix_now() - step.started_at > ANSWER_SECS {
                        step.state = StepState::Failed;
                        step.reason = "timeout".into();
                        step.error = "the machine never answered".into();
                    }
                    if step.state != StepState::Done {
                        return;
                    }
                }
                _ => return,
            }
        }
    }

    async fn ask_step(&self, step: &Step) -> Result<String, String> {
        let verb = step.kind.verb();
        if step.kind.on_gateway() {
            let gateway = self.gateway().ok_or("there is no gateway")?;
            // The version is never the admin's to pick: it is the newest release the server found.
            let version = match step.kind {
                StepKind::Gateway => Some(
                    crate::routes::gateway::newer_gateway(self, gateway.view().software.as_deref())
                        .await
                        .ok_or("there is no newer gateway to install")?,
                ),
                _ => None,
            };
            return gateway.ask(verb, version.as_deref()).await;
        }
        let host = self.host().ok_or("there is no helper on this machine")?;
        if host.view().machine.is_some_and(|machine| !machine.can(verb)) {
            return Err(format!("the helper on this machine cannot do {verb} yet"));
        }
        let mut options = Vec::new();
        if step.kind == StepKind::Server {
            // The tag of the channel, every time: that is what makes a change of channel count,
            // and what lets go of the version an earlier rollback pinned.
            options.push(("channel", tag(build().release, self.update_settings().await.channel)));
            if !step.compose.is_empty() {
                options.push(("compose", step.compose.as_str()));
            }
        }
        host.ask(verb, &options).await
    }

    fn follow_step(&self, step: &mut Step) {
        // shortcut: follows the helper's newest job only; a VPN job asked for in the middle of a
        // run hides this one until the run's timeout. Look jobs up by id if that ever bites.
        let found = if step.kind.on_gateway() {
            self.gateway()
                .and_then(|gateway| gateway.view().machine)
                .and_then(|machine| machine.job)
                .filter(|job| job.id == step.job)
                .map(|job| (job.state, job.error, job.log))
        } else {
            self.host().map(|host| host.view()).and_then(|view| {
                let log = view.log;
                view.job.filter(|job| job.id == step.job).map(|job| (job.state, job.error, log))
            })
        };
        let Some((state, error, log)) = found else {
            return;
        };
        step.log = tail(log);
        match state.as_str() {
            "done" => step.state = StepState::Done,
            "failed" | "refused" | "rolledBack" => {
                step.state = StepState::Failed;
                step.reason = match (state.as_str(), error.as_str()) {
                    ("rolledBack", _) => "rolledBack".into(),
                    (_, "compose") => "compose".into(),
                    _ => String::new(),
                };
                step.error = error;
            }
            _ => {}
        }
    }
}

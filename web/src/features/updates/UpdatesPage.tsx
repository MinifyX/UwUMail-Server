import { Button, Field, Segmented, Toggle, Card, ICONS, Icon, Dialog, cx } from "@uwusuite/design";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState, type ReactNode } from "react";
import { RotateCw } from "lucide-react";
import { LoadError, Loading } from "@/components/StatusViews";
import { CopyButton, PageHeader } from "@/components/ui/Page";
import { helperCan, helperOutdated, updateCommand } from "@/features/admin/host";
import { useT } from "@/i18n";
import {
  api,
  type UpdateChannel,
  type UpdateRun,
  type UpdateSettings,
  type UpdateStep,
  type UpdateStepKind,
  type UpdatesView,
} from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDateTime } from "@/lib/format";
import { toast } from "@/state/toasts";
import { runActive, updatesKey, useUpdateRun, useUpdates } from "./queries";

function Command({ label, command }: { label: string; command: string }) {
  return (
    <div className="flex flex-col gap-1.5">
      <p className="text-[13px] font-semibold text-muted">{label}</p>
      <div className="flex items-start gap-2 rounded-control bg-canvas px-3 py-2">
        <code className="min-w-0 flex-1 font-mono text-[12px] break-all">{command}</code>
        <CopyButton value={command} />
      </div>
    </div>
  );
}

/** What is new: the releases of the channel, or the commits an edge build is behind. */
function News({ view }: { view: UpdatesView }) {
  const { t, i18n } = useT();
  const { build, info } = view;
  const edge = !build.release;
  const behind = info.behind ?? 0;
  const available = edge ? behind > 0 : info.releases.length > 0;

  if (info.error) {
    return (
      <p className="rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning-ink">
        {t("updates.checkFailed", { error: info.error })}
      </p>
    );
  }
  if (!info.checkedAt) return <p className="text-[13px] text-muted">{t("updates.notChecked")}</p>;
  if (!available) {
    return (
      <p className="text-[13px] text-muted">
        {t("updates.upToDate", { time: formatDateTime(info.checkedAt, i18n.language) })}
      </p>
    );
  }
  if (edge) {
    return (
      <div className="flex flex-col gap-2">
        <p className="text-sm font-semibold">{t("updates.edgeBehind", { count: behind })}</p>
        <ul className="flex flex-col gap-1 text-[13px]">
          {info.commits.map((commit) => (
            <li key={commit.sha} className="flex gap-2">
              <code className="font-mono text-faint">{commit.sha}</code>
              <span className="min-w-0 break-words">{commit.message}</span>
            </li>
          ))}
        </ul>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-3">
      {info.releases.map((release) => (
        <section key={release.version} className="flex flex-col gap-1">
          <h3 className="flex flex-wrap items-center gap-2 text-sm font-semibold">
            {release.name}
            {release.prerelease && (
              <span className="rounded-full bg-pink-tint px-2 text-[11px] text-pink-ink">{t("updates.beta")}</span>
            )}
            <a
              href={release.url}
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center gap-1 text-[12px] font-normal text-pink-ink hover:underline"
            >
              {t("updates.onGitHub")}
              <Icon icon={ICONS.openExternal} className="size-3" aria-hidden />
            </a>
          </h3>
          <pre className="max-h-56 overflow-y-auto font-sans text-[13px] break-words whitespace-pre-wrap text-muted">
            {release.notes}
          </pre>
        </section>
      ))}
    </div>
  );
}

type RunControl = ReturnType<typeof useUpdateRun>;

const STEP_TONE: Record<UpdateStep["state"], string> = {
  pending: "bg-canvas text-muted",
  running: "bg-pink-tint text-pink-ink",
  done: "bg-success-tint text-success-ink",
  failed: "bg-danger-tint text-danger-ink",
  skipped: "bg-canvas text-muted",
};

/** What to do about the step the run stopped at: again, without it, or not at all. */
function FailedStep({ step, control }: { step: UpdateStep; control: RunControl }) {
  const { t } = useT();
  const { answer } = control;
  const compose = step.reason === "compose";
  return (
    <div className="flex flex-col gap-3 rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning-ink">
      <p>
        {step.reason
          ? t(`updates.run.reasons.${step.reason}`, { defaultValue: step.error })
          : step.error || t("updates.run.failedHint")}
      </p>
      <div className="flex flex-wrap gap-2">
        {compose ? (
          <>
            <Button
              size="sm"
              variant="primary"
              disabled={answer.isPending}
              onClick={() => answer.mutate({ action: "retry", compose: "keep" })}
            >
              {t("updates.run.composeKeep")}
            </Button>
            <Button
              size="sm"
              disabled={answer.isPending}
              onClick={() => answer.mutate({ action: "retry", compose: "replace" })}
            >
              {t("updates.run.composeReplace")}
            </Button>
          </>
        ) : (
          <Button
            size="sm"
            variant="primary"
            icon={ICONS.refresh}
            disabled={answer.isPending}
            onClick={() => answer.mutate({ action: "retry" })}
          >
            {t("updates.run.retry")}
          </Button>
        )}
        <Button size="sm" disabled={answer.isPending} onClick={() => answer.mutate({ action: "skip" })}>
          {t("updates.run.skip")}
        </Button>
        <Button size="sm" variant="ghost" disabled={answer.isPending} onClick={() => answer.mutate({ action: "stop" })}>
          {t("updates.run.stop")}
        </Button>
      </div>
    </div>
  );
}

/** The run, step by step, with what the step at hand prints. */
function RunCard({ run, control, away }: { run: UpdateRun; control: RunControl; away: boolean }) {
  const { t, i18n } = useT();
  const active = run.finishedAt === null;
  const serverDone = run.steps.some((step) => step.kind === "server" && step.state === "done");
  // The log worth reading is the one of the step that runs or failed; after the run, the last one.
  const shown =
    run.steps.find((step) => step.state === "running" || step.state === "failed") ??
    run.steps.findLast((step) => step.log !== "");

  return (
    <Card title={t("updates.run.title")}>
      <div className="flex flex-col gap-4">
        <p className="text-[13px] text-muted">
          {active
            ? t("updates.run.started", { time: formatDateTime(run.startedAt, i18n.language), by: run.by })
            : t("updates.run.finished", { time: formatDateTime(run.finishedAt ?? 0, i18n.language) })}
        </p>
        <ol className="flex flex-col gap-2">
          {run.steps.map((step) => (
            <li
              key={step.kind}
              className="flex flex-wrap items-center gap-2 text-sm"
              aria-current={step.state === "running" ? "step" : undefined}
            >
              <span className="min-w-0 flex-1 font-semibold">{t(`updates.steps.${step.kind}`)}</span>
              <span className={cx("rounded-full px-2 py-0.5 text-[12px]", STEP_TONE[step.state])}>
                {t(`updates.stepStates.${step.state}`)}
              </span>
            </li>
          ))}
        </ol>
        {/* Says each change of step aloud, not every line of the log. */}
        <p role="status" className="sr-only">
          {shown && `${t(`updates.steps.${shown.kind}`)}: ${t(`updates.stepStates.${shown.state}`)}`}
        </p>
        {away && active && <p className="text-[13px] text-muted">{t("updates.run.away")}</p>}
        {shown?.state === "failed" && active && <FailedStep step={shown} control={control} />}
        {shown?.log && (
          <details open={active} className="rounded-control bg-canvas px-3 py-2">
            <summary className="cursor-pointer text-[13px] font-semibold">
              {t("updates.run.log", { step: t(`updates.steps.${shown.kind}`) })}
            </summary>
            <pre tabIndex={0} className="mt-2 max-h-64 overflow-auto font-mono text-[12px] whitespace-pre-wrap">
              {shown.log}
            </pre>
          </details>
        )}
        {serverDone && (
          <div className="flex flex-wrap items-center gap-3">
            <Button variant="primary" icon={RotateCw} onClick={() => window.location.reload()}>
              {t("updates.reload")}
            </Button>
            <p className="text-[13px] text-muted">{t("updates.run.reloadHint")}</p>
          </div>
        )}
      </div>
    </Card>
  );
}

/** One thing that can be updated: what it is, how it stands, and its own button. */
function Row({ title, status, children }: { title: string; status: ReactNode; children?: ReactNode }) {
  return (
    <section className="flex flex-col gap-2 border-t border-hairline pt-4 first:border-t-0 first:pt-0">
      <div className="flex flex-wrap items-start gap-x-4 gap-y-2">
        <div className="min-w-0 flex-1 basis-64">
          <h3 className="text-sm font-semibold">{title}</h3>
          <div className="text-[13px] text-muted">{status}</div>
        </div>
      </div>
      {children}
    </section>
  );
}

function Warning({ children }: { children: ReactNode }) {
  return <p className="rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning-ink">{children}</p>;
}

/** Everything there is to update, the one button for all of it, and a button for each. */
function ComponentsCard({ view, control }: { view: UpdatesView; control: RunControl }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [restart, setRestart] = useState<"hostReboot" | "gatewayReboot" | null>(null);
  const { start } = control;

  const check = useMutation({
    mutationFn: () => api<UpdatesView>("/api/admin/updates/check", { method: "POST", body: {} }),
    onSuccess: (next) => queryClient.setQueryData(updatesKey, next),
    onError: (error) => toast(errorText(error), "error"),
  });

  const { build, info, host, gateway } = view;
  const machine = host.machine;
  const system = gateway?.machine?.system;
  const busy = runActive(view) || start.isPending;
  const edge = !build.release;
  const serverNews = edge ? (info.behind ?? 0) > 0 : info.releases.length > 0;
  const outdated = helperOutdated(machine);

  /** The button of one step; absent where nothing here could carry it out. */
  const act = (kind: UpdateStepKind, label: string, danger = false) =>
    view.possible.includes(kind) && (
      <Button
        size="sm"
        variant={danger ? "danger" : undefined}
        icon={danger ? ICONS.reset : ICONS.download}
        disabled={busy}
        className="self-start"
        onClick={() => (danger ? setRestart(kind as "hostReboot" | "gatewayReboot") : start.mutate([kind]))}
      >
        {label}
      </Button>
    );
  const waiting = (count: number, security: number) =>
    count === 0 ? t("host.upToDate") : t("host.waiting", { count, security });

  return (
    <Card title={t("updates.all.title")}>
      <div className="flex flex-col gap-4">
        <div className="flex flex-col gap-2">
          <div className="flex flex-wrap gap-2">
            <Button
              variant="primary"
              icon={ICONS.download}
              busy={start.isPending}
              disabled={busy || view.plan.length === 0}
              onClick={() => start.mutate(undefined)}
            >
              {t("updates.all.button")}
            </Button>
            <Button variant="ghost" icon={ICONS.refresh} busy={check.isPending} onClick={() => check.mutate()}>
              {t("updates.checkNow")}
            </Button>
          </div>
          <p className="text-[13px] text-muted">
            {view.plan.length === 0
              ? t("updates.all.nothing")
              : t("updates.all.plan", { steps: view.plan.map((kind) => t(`updates.steps.${kind}`)).join(" → ") })}
          </p>
        </div>

        <Row
          title={t("updates.steps.server")}
          status={t(edge ? "updates.runningEdge" : "updates.running", {
            version: build.version,
            commit: build.commit?.slice(0, 7) ?? "–",
          })}
        >
          <News view={view} />
          {serverNews && act("server", t("updates.one"))}
          {!host.available ? (
            <>
              <p className="text-[13px] text-muted">{t("updates.noHelper")}</p>
              <Command label={t("updates.steps.server")} command={view.serverCommand} />
            </>
          ) : (
            machine &&
            !helperCan(machine, "uwumail-update") && (
              <>
                <p className="text-[13px] text-muted">{t("updates.helperOld")}</p>
                <Command label={t("updates.steps.server")} command={updateCommand(machine)} />
              </>
            )
          )}
        </Row>

        {host.available && !machine && <Row title={t("host.title")} status={t("host.noReport")} />}
        {machine && (
          <>
            <Row
              title={t("updates.steps.helper")}
              status={t(outdated ? "host.helperNewer" : "updates.helperCurrent", { version: machine.helper ?? "1" })}
            >
              {outdated && act("helper", t("updates.one"))}
            </Row>
            <Row
              title={t("host.title")}
              status={
                <>
                  {machine.name || t("host.unknownSystem")} ·{" "}
                  {machine.kind === "debian" ? waiting(machine.updates, machine.securityUpdates) : t("host.notApt")}
                </>
              }
            >
              {machine.rebootRequired && (
                <Warning>
                  {t("host.rebootNeeded")}
                  {machine.rebootPackages.length > 0 && ` (${machine.rebootPackages.slice(0, 5).join(", ")})`}
                </Warning>
              )}
              {/* Never "alone" unless the helper said so: a warning that sometimes stays quiet is worse than none. */}
              {machine.alone !== true && (
                <Warning>
                  {machine.alone === false
                    ? t("host.notAlone", { others: machine.others.join(", ") })
                    : t("host.cannotTell")}
                </Warning>
              )}
              {machine.newRelease && (
                <p className="text-[13px] text-muted">{t("updates.newRelease", { release: machine.newRelease })}</p>
              )}
              <div className="flex flex-wrap gap-2">
                {machine.updates > 0 && act("hostSystem", t("updates.one"))}
                {act("hostReboot", t("updates.restart"), true)}
              </div>
              <p className="text-[12px] text-muted">{t("host.liability")}</p>
            </Row>
          </>
        )}

        {gateway && (
          <>
            <Row
              title={t("updates.steps.gateway")}
              status={
                <>
                  {gateway.software}
                  {view.gatewayVersion && ` · ${t("updates.newer", { version: view.gatewayVersion })}`}
                </>
              }
            >
              {view.gatewayVersion && act("gateway", t("updates.one"))}
              {view.gatewayCommand && (
                <>
                  <p className="text-[13px] text-muted">{t("updates.gatewayNoHelper")}</p>
                  <Command label={t("updates.gatewayCommand")} command={view.gatewayCommand} />
                </>
              )}
            </Row>
            {system && (
              <Row
                title={t("updates.gatewayMachine")}
                status={
                  <>
                    {system.name} · {waiting(system.updates, system.securityUpdates)}
                  </>
                }
              >
                {system.rebootRequired && <Warning>{t("host.rebootNeeded")}</Warning>}
                {system.newRelease && (
                  <p className="text-[13px] text-muted">{t("updates.newRelease", { release: system.newRelease })}</p>
                )}
                <div className="flex flex-wrap gap-2">
                  {system.updates > 0 && act("gatewaySystem", t("updates.one"))}
                  {act("gatewayReboot", t("updates.restart"), true)}
                </div>
              </Row>
            )}
          </>
        )}
      </div>

      <Dialog
        open={restart !== null}
        onClose={() => setRestart(null)}
        title={restart && t(`updates.restartAsk.${restart}.title`)}
        width="sm"
        tone="warning"
        footer={
          <>
            <Button onClick={() => setRestart(null)}>{t("common.cancel")}</Button>
            <Button
              variant="danger"
              icon={ICONS.reset}
              onClick={() => {
                if (restart) start.mutate([restart]);
                setRestart(null);
              }}
            >
              {t("updates.restart")}
            </Button>
          </>
        }
      >
        <p className="px-6 pb-2 text-sm text-muted">{restart && t(`updates.restartAsk.${restart}.body`)}</p>
      </Dialog>
    </Card>
  );
}

const CHANNELS: UpdateChannel[] = ["stable", "beta"];

/** Whether the server asks GitHub at all, and which releases it counts as newer. */
function CheckCard({ view }: { view: UpdatesView }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();

  const save = useMutation({
    mutationFn: (settings: UpdateSettings) => api<UpdatesView>("/api/admin/updates", { method: "PUT", body: settings }),
    onSuccess: (next) => {
      queryClient.setQueryData(updatesKey, next);
      toast(t("common.saved"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const change = (changes: Partial<UpdateSettings>) => save.mutate({ ...view.settings, ...changes });

  return (
    <Card title={t("updates.checkTitle")}>
      <div className="flex flex-col gap-4">
        <Toggle
          checked={view.settings.check}
          onChange={(check) => change({ check })}
          label={t("updates.check")}
          description={t("updates.checkHint")}
        />
        {view.settings.check && (
          <Field label={t("updates.channel")}>
            {() => (
              <Segmented<string>
                label={t("updates.channel")}
                value={view.settings.channel}
                onChange={(channel) => change({ channel: channel as UpdateChannel })}
                options={CHANNELS.map((value) => ({ value, label: t(`updates.${value}`) }))}
              />
            )}
          </Field>
        )}
      </div>
    </Card>
  );
}

/** Server → Updates: the one place where anything is updated or restarted. */
export function UpdatesPage() {
  const { t } = useT();
  const query = useUpdates();
  const control = useUpdateRun();

  return (
    <div className="flex flex-col gap-5">
      <PageHeader title={t("nav.updates")} intro={t("updates.intro")} />
      {/* The last answer stays while the server's own update takes it away. */}
      {query.data ? (
        <>
          {query.data.run && <RunCard run={query.data.run} control={control} away={query.isError} />}
          <ComponentsCard view={query.data} control={control} />
          <CheckCard view={query.data} />
        </>
      ) : query.isPending ? (
        <Loading />
      ) : (
        <LoadError error={query.error} onRetry={() => void query.refetch()} />
      )}
      {control.dialog}
    </div>
  );
}

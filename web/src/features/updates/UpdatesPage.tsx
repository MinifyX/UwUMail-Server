import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Download, ExternalLink, RefreshCw, RotateCw } from "lucide-react";
import { NyuScene } from "@/components/nyu/scenes";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card, CopyButton, PageHeader } from "@/components/ui/Card";
import { Field, Segmented, Toggle } from "@/components/ui/Field";
import { helperCan, helperOutdated, JobBox, jobBusy, updateCommand, useAskHost, useHost } from "@/features/admin/host";
import { useT } from "@/i18n";
import { api, type UpdateChannel, type UpdateSettings, type UpdatesView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDateTime } from "@/lib/format";
import { toast } from "@/state/toasts";
import { updatesKey, useUpdates } from "./queries";

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
      <p className="rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning">
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
              <ExternalLink className="size-3" aria-hidden />
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

/**
 * How the new version gets here: with the machine's helper a button, which runs update.sh there
 * (backup, compose file, images, and back to the old version when the new one does not come up);
 * without it, the command to run by hand.
 */
function Install({ view }: { view: UpdatesView }) {
  const { t } = useT();
  const host = useHost();
  // Only an update asked for from this page reloads it; the new version brings a new portal.
  const [asked, setAsked] = useState(false);
  const { ask, dialog } = useAskHost((verb) => setAsked(verb === "uwumail-update"));
  const machine = host.data?.machine;
  const job = host.data?.job;
  const busy = jobBusy(job?.state);

  if (helperCan(machine, "uwumail-update")) {
    return (
      <div className="flex flex-col gap-3">
        <p className="text-[13px] text-muted">{t("updates.byButton")}</p>
        {asked && host.data && <JobBox view={host.data} />}
        <div className="flex flex-wrap gap-2">
          {asked && job?.state === "done" ? (
            <Button variant="primary" icon={RotateCw} onClick={() => window.location.reload()}>
              {t("updates.reload")}
            </Button>
          ) : (
            <Button
              variant="primary"
              icon={Download}
              busy={ask.isPending || (asked && busy)}
              disabled={busy}
              onClick={() => ask.mutate("uwumail-update")}
            >
              {t("updates.installNow")}
            </Button>
          )}
        </div>
        {dialog}
      </div>
    );
  }
  const old = host.data?.available && helperOutdated(machine);
  return (
    <div className="flex flex-col gap-3">
      <p className="text-[13px] text-muted">{t(old ? "updates.helperOld" : "updates.onTheMachine")}</p>
      <Command label={t("updates.serverCommand")} command={old ? updateCommand(machine) : view.serverCommand} />
    </div>
  );
}

/** What runs here, what is newer, and how it gets installed. */
function VersionCard({ view }: { view: UpdatesView }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();

  const check = useMutation({
    mutationFn: () => api<UpdatesView>("/api/admin/updates/check", { method: "POST", body: {} }),
    onSuccess: (next) => queryClient.setQueryData(updatesKey, next),
    onError: (error) => toast(errorText(error), "error"),
  });

  const { build, info } = view;
  const edge = !build.release;
  const available = edge ? (info.behind ?? 0) > 0 : info.releases.length > 0;

  return (
    <Card title={t("updates.version.title")}>
      <div className="flex flex-col gap-4">
        <p className="text-sm">
          {t(edge ? "updates.runningEdge" : "updates.running", {
            version: build.version,
            commit: build.commit?.slice(0, 7) ?? "–",
          })}
        </p>

        <News view={view} />

        {available && (
          <div className="flex flex-col gap-3 border-t border-hairline pt-4">
            <Install view={view} />
            {view.gatewayCommand && <Command label={t("updates.gatewayCommand")} command={view.gatewayCommand} />}
          </div>
        )}
        {view.gateway?.software && (
          <p className="text-[12px] text-muted">{t("updates.gatewayRunning", { software: view.gateway.software })}</p>
        )}

        <div>
          <Button variant="ghost" icon={RefreshCw} busy={check.isPending} onClick={() => check.mutate()}>
            {t("updates.checkNow")}
          </Button>
        </div>
      </div>
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

export function UpdatesPage() {
  const { t } = useT();
  const query = useUpdates();

  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  const view = query.data;

  return (
    <div className="flex flex-col gap-5">
      <PageHeader
        title={t("updates.title")}
        intro={t("updates.intro")}
        art={view.info.releases.length === 0 ? <NyuScene name="done" className="h-auto w-[130px]" /> : undefined}
      />
      <VersionCard view={view} />
      <CheckCard view={view} />
    </div>
  );
}

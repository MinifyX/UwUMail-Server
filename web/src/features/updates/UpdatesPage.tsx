import { useMutation, useQueryClient } from "@tanstack/react-query";
import { CalendarClock, Check, Download, ExternalLink, RefreshCw } from "lucide-react";
import { useState } from "react";
import { NyuScene } from "@/components/nyu/scenes";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card, CopyButton, PageHeader } from "@/components/ui/Card";
import { Dialog } from "@/components/ui/Dialog";
import { Field, Segmented, Select, Toggle } from "@/components/ui/Field";
import { Cancelled, usePasswordConfirmation } from "@/features/security/ConfirmPassword";
import { useT } from "@/i18n";
import { api, type UpdateChannel, type UpdateSettings, type UpdatesView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDateTime } from "@/lib/format";
import { clock, localWeekly, minuteOptions, utcWeekly, weekdayNames } from "@/lib/time";
import { toast } from "@/state/toasts";
import { busy, updatesKey, useUpdates } from "./queries";

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
 * The dialog before an update: what it will do, and the one deliberate way to do it without a
 * backup. That way out is here because a server with nowhere to back up to should still be able to
 * update — but only by saying so out loud.
 */
function InstallDialog({
  view,
  open,
  onClose,
  onInstall,
}: {
  view: UpdatesView;
  open: boolean;
  onClose: () => void;
  onInstall: (backup: boolean) => void;
}) {
  const { t } = useT();
  const [without, setWithout] = useState(false);
  const wanted = view.settings.backupFirst && !without;
  const impossible = wanted && !view.backupReady;

  return (
    <Dialog open={open} onClose={onClose} title={t("updates.install.title")}>
      <div className="flex flex-col gap-4 px-6 pb-6">
        <p className="text-sm">
          {view.target
            ? t("updates.install.to", { version: view.target })
            : t("updates.install.toEdge", { image: view.image })}
        </p>
        <p className="text-[13px] text-muted">{t("updates.install.explain")}</p>

        {view.settings.backupFirst ? (
          <div className="flex flex-col gap-2">
            <p className="rounded-control bg-canvas px-3 py-2 text-[13px]">
              {view.backupReady ? t("updates.install.backupFirst") : t("updates.install.noTarget")}
            </p>
            <Toggle
              checked={without}
              onChange={setWithout}
              label={t("updates.install.without")}
              description={t("updates.install.withoutHint")}
            />
          </div>
        ) : (
          <p className="rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning">
            {t("updates.install.noBackupSetting")}
          </p>
        )}

        <div className="flex flex-wrap justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button variant="primary" icon={Download} disabled={impossible} onClick={() => onInstall(wanted)}>
            {t("updates.install.go")}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

/** The running version, what is newer, and the button — or the commands, without a helper. */
function VersionCard({ view }: { view: UpdatesView }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const { confirmed, dialog } = usePasswordConfirmation();
  const [asking, setAsking] = useState(false);

  const check = useMutation({
    mutationFn: () => api<UpdatesView>("/api/admin/updates/check", { method: "POST", body: {} }),
    onSuccess: (next) => queryClient.setQueryData(updatesKey, next),
    onError: (error) => toast(errorText(error), "error"),
  });
  const install = useMutation({
    mutationFn: (backup: boolean) =>
      confirmed((password) =>
        api<UpdatesView>("/api/admin/updates/run", { method: "POST", body: { backup, password } }),
      ),
    onSuccess: (next) => {
      setAsking(false);
      queryClient.setQueryData(updatesKey, next);
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
    },
    onError: (error) => {
      if (!(error instanceof Cancelled)) toast(errorText(error), "error");
    },
  });

  const { build, info } = view;
  const edge = !build.release;
  const available = edge ? (info.behind ?? 0) > 0 : info.releases.length > 0;
  const running = busy(view.status);

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
            {view.canInstall ? (
              <>
                <p className="text-[13px] text-muted">{t("updates.install.hint")}</p>
                <div>
                  <Button variant="primary" icon={Download} disabled={running} onClick={() => setAsking(true)}>
                    {t("updates.install.now")}
                  </Button>
                </div>
              </>
            ) : (
              <>
                <p className="text-[13px] text-muted">{t("updates.noHelper")}</p>
                <Command label={t("updates.serverCommand")} command={view.serverCommand} />
                {view.gatewayCommand && <Command label={t("updates.gatewayCommand")} command={view.gatewayCommand} />}
              </>
            )}
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
      <InstallDialog
        view={view}
        open={asking}
        onClose={() => setAsking(false)}
        onInstall={(backup) => install.mutate(backup)}
      />
      {dialog}
    </Card>
  );
}

/** What the update is doing, or what became of it. Absent while nothing has ever run. */
function RunCard({ view, reconnecting }: { view: UpdatesView; reconnecting: boolean }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const { status } = view;
  const forget = useMutation({
    mutationFn: () => api<UpdatesView>("/api/admin/updates/run", { method: "DELETE" }),
    onSuccess: (next) => queryClient.setQueryData(updatesKey, next),
    onError: (error) => toast(errorText(error), "error"),
  });

  if (status.state === "idle") return null;
  const running = busy(status);
  const bad = status.state === "failed" || status.state === "rolledBack";
  const at = status.finishedAt ?? status.startedAt;

  return (
    <Card title={t("updates.run.title")}>
      <div className="flex flex-col gap-3">
        <p
          className={
            bad
              ? "rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning"
              : running
                ? "rounded-control bg-pink-tint px-3 py-2 text-[13px] text-pink-ink"
                : "text-sm"
          }
        >
          {t("updates.run.states." + status.state, {
            version: status.to ?? t("updates.run.theNewest"),
            from: status.from,
          })}
          {status.error && <span className="block font-normal">{status.error}</span>}
        </p>

        {reconnecting && <p className="text-[13px] text-muted">{t("updates.run.reconnecting")}</p>}

        <p className="text-[12px] text-muted">
          {status.by === "schedule" ? t("updates.run.bySchedule") : t("updates.run.byHand")}
          {at ? " · " + formatDateTime(at, i18n.language) : ""}
          {status.backup ? " · " + t("updates.run.backups." + status.backup) : ""}
        </p>

        {status.state === "rolledBack" && (
          <p className="rounded-control bg-canvas px-3 py-2 text-[13px]">{t("updates.run.rolledBackHint")}</p>
        )}

        {view.log && (
          <pre className="max-h-64 overflow-auto rounded-control bg-canvas px-3 py-2 font-mono text-[12px] whitespace-pre-wrap">
            {view.log}
          </pre>
        )}

        {!running && (
          <div>
            <Button variant="ghost" icon={Check} busy={forget.isPending} onClick={() => forget.mutate()}>
              {t("updates.run.forget")}
            </Button>
          </div>
        )}
      </div>
    </Card>
  );
}

/** Looking for updates, which channel, and the schedule that installs them by itself. */
function ScheduleCard({ view }: { view: UpdatesView }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const { settings } = view;
  const here = localWeekly(settings.weekday, settings.hour, settings.minute);
  const [weekday, setWeekday] = useState<number | null>(here.weekday);
  const [time, setTime] = useState(here.time);
  const days = weekdayNames(i18n.language);

  const save = useMutation({
    mutationFn: (next: UpdateSettings) => api<UpdatesView>("/api/admin/updates", { method: "PUT", body: next }),
    onSuccess: (next) => queryClient.setQueryData(updatesKey, next),
    onError: (error) => toast(errorText(error), "error"),
  });
  const change = (patch: Partial<UpdateSettings>) => save.mutate({ ...settings, ...patch });
  const changeWhen = (day: number | null, minutes: number) => {
    setWeekday(day);
    setTime(minutes);
    change(utcWeekly(day, minutes));
  };

  const edge = !view.build.release;

  return (
    <Card title={t("updates.schedule.title")}>
      <div className="flex flex-col gap-4">
        <Toggle
          checked={settings.check}
          onChange={(on) => change({ check: on })}
          label={t("updates.check")}
          description={t("updates.checkHint")}
        />
        {!edge && (
          <Segmented<UpdateChannel>
            label={t("updates.channel")}
            value={settings.channel}
            onChange={(channel) => change({ channel })}
            options={[
              { value: "stable", label: t("updates.stable") },
              { value: "beta", label: t("updates.beta") },
            ]}
          />
        )}

        <div className="flex flex-col gap-3 border-t border-hairline pt-4">
          {edge || !view.canInstall ? (
            <p className="text-[13px] text-muted">{edge ? t("updates.schedule.notForEdge") : t("updates.noHelper")}</p>
          ) : (
            <>
              <Toggle
                checked={settings.auto}
                onChange={(on) => change({ auto: on })}
                label={t("updates.schedule.auto")}
                description={t("updates.schedule.autoHint")}
              />
              <div className="grid gap-3 sm:grid-cols-2">
                <Field label={t("updates.schedule.day")}>
                  {(id) => (
                    <Select
                      id={id}
                      value={weekday === null ? "" : weekday}
                      onChange={(event) =>
                        changeWhen(event.target.value === "" ? null : Number(event.target.value), time)
                      }
                    >
                      <option value="">{t("updates.schedule.everyDay")}</option>
                      {days.map((name, day) => (
                        <option key={name} value={day}>
                          {name}
                        </option>
                      ))}
                    </Select>
                  )}
                </Field>
                <Field label={t("updates.schedule.time")} hint={t("updates.schedule.timeHint")}>
                  {(id) => (
                    <div className="flex items-center gap-1">
                      <Select
                        id={id}
                        value={Math.floor(time / 60)}
                        onChange={(event) => changeWhen(weekday, Number(event.target.value) * 60 + (time % 60))}
                      >
                        {Array.from({ length: 24 }, (_, value) => (
                          <option key={value} value={value}>
                            {String(value).padStart(2, "0")}
                          </option>
                        ))}
                      </Select>
                      <span aria-hidden className="text-muted">
                        :
                      </span>
                      <Select
                        aria-label={t("updates.schedule.time")}
                        value={time % 60}
                        onChange={(event) =>
                          changeWhen(weekday, Math.floor(time / 60) * 60 + Number(event.target.value))
                        }
                      >
                        {minuteOptions(time % 60).map((value) => (
                          <option key={value} value={value}>
                            {String(value).padStart(2, "0")}
                          </option>
                        ))}
                      </Select>
                    </div>
                  )}
                </Field>
              </div>
              <p className="flex items-start gap-2 text-[13px] text-muted">
                <CalendarClock className="mt-0.5 size-4 shrink-0" aria-hidden />
                <span>{t("updates.schedule.window", { time: clock(time) })}</span>
              </p>
              {view.nearBackup && <p className="text-[13px] text-muted">{t("updates.schedule.nearBackup")}</p>}
            </>
          )}
          <Toggle
            checked={settings.backupFirst}
            onChange={(on) => change({ backupFirst: on })}
            label={t("updates.schedule.backupFirst")}
            description={t("updates.schedule.backupFirstHint")}
          />
          {settings.backupFirst && !view.backupReady && (
            <p className="rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning">
              {t("updates.schedule.noTarget")}
            </p>
          )}
        </div>
      </div>
    </Card>
  );
}

/** Updates: what is running, what is newer, the button that installs it and the schedule. */
export function UpdatesPage() {
  const { t } = useT();
  const query = useUpdates();

  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  const view = query.data;
  // While the new container comes up there is nobody to answer, and that is not an error.
  const reconnecting = query.isFetching && query.failureCount > 0;

  return (
    <div className="flex flex-col gap-5">
      <PageHeader
        title={t("updates.title")}
        intro={t("updates.intro")}
        art={
          view.status.state === "idle" && view.info.releases.length === 0 ? (
            <NyuScene name="done" className="h-auto w-[130px]" />
          ) : undefined
        }
      />
      <RunCard view={view} reconnecting={reconnecting} />
      <VersionCard view={view} />
      <ScheduleCard view={view} />
    </div>
  );
}

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { DatabaseBackup, KeyRound, Plug, RefreshCw } from "lucide-react";
import { type FormEvent, useState } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card, CopyButton, PageHeader } from "@/components/ui/Card";
import { Dialog } from "@/components/ui/Dialog";
import { Field, Segmented, Select, TextInput, Toggle } from "@/components/ui/Field";
import { Cancelled, usePasswordConfirmation } from "@/features/security/ConfirmPassword";
import { useT } from "@/i18n";
import { ApiError, api, type BackupSnapshot, type BackupsView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatBytes, formatDateTime } from "@/lib/format";
import { toast } from "@/state/toasts";

const key = ["admin", "backups"] as const;

/** The hour in the browser's time zone for an hour in UTC, and back. */
const offsetHours = () => -Math.round(new Date().getTimezoneOffset() / 60);
const localHour = (utc: number) => (((utc + offsetHours()) % 24) + 24) % 24;
const utcHour = (local: number) => (((local - offsetHours()) % 24) + 24) % 24;

function RecoveryKeyDialog({ recoveryKey, onClose }: { recoveryKey: string | null; onClose: () => void }) {
  const { t } = useT();
  const [written, setWritten] = useState(false);
  return (
    <Dialog open={recoveryKey !== null} onClose={() => written && onClose()} title={t("backups.recovery.title")}>
      <div className="flex flex-col gap-4 px-6 pb-6">
        <p className="text-[13px] text-muted">{t("backups.recovery.explain")}</p>
        <div className="flex items-center gap-2 rounded-control bg-canvas px-3 py-3">
          <code className="flex-1 font-mono text-[13px] break-all">{recoveryKey}</code>
          {recoveryKey && <CopyButton value={recoveryKey} />}
        </div>
        <Toggle checked={written} onChange={setWritten} label={t("backups.recovery.written")} />
        <div className="flex justify-end">
          <Button variant="primary" disabled={!written} onClick={onClose}>
            {t("common.done")}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

function StatusCard({ view }: { view: BackupsView }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const run = useMutation({
    mutationFn: () => api<void>("/api/admin/backups/run", { method: "POST", body: {} }),
    onSuccess: () => {
      toast(t("backups.status.started"), "success");
      window.setTimeout(() => void queryClient.invalidateQueries({ queryKey: key }), 1500);
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const { status } = view;
  const report = status.lastReport;
  const failed = status.lastError && (status.lastAttemptAt ?? 0) >= (status.lastSuccessAt ?? 0);
  return (
    <Card title={t("backups.status.title")}>
      <div className="flex flex-col gap-3">
        {view.running ? (
          <p className="rounded-control bg-pink-tint px-3 py-2 text-[13px] text-pink-ink">
            {t("backups.status.running")}
          </p>
        ) : status.lastSuccessAt ? (
          <p className="text-sm">
            {t("backups.status.last", { time: formatDateTime(status.lastSuccessAt, i18n.language) })}
            {report &&
              ` · ${t("backups.status.sizes", {
                total: formatBytes(report.total, i18n.language),
                uploaded: formatBytes(report.uploaded, i18n.language),
              })}`}
          </p>
        ) : (
          <p className="text-sm text-muted">{t("backups.status.never")}</p>
        )}
        {failed && (
          <p role="alert" className="rounded-control bg-danger-tint px-3 py-2 text-[13px] text-danger">
            {t("backups.status.failed", { error: status.lastError })}
          </p>
        )}
        {!view.enabled && view.target && <p className="text-[13px] text-muted">{t("backups.status.paused")}</p>}
        <div className="flex flex-wrap gap-2">
          <Button
            icon={DatabaseBackup}
            busy={run.isPending || view.running}
            disabled={!view.target}
            onClick={() => run.mutate()}
          >
            {t("backups.status.runNow")}
          </Button>
          <Button
            variant="ghost"
            icon={RefreshCw}
            onClick={() => void queryClient.invalidateQueries({ queryKey: key })}
          >
            {t("backups.status.refresh")}
          </Button>
        </div>
      </div>
    </Card>
  );
}

function SettingsCard({ view, onRecoveryKey }: { view: BackupsView; onRecoveryKey: (key: string) => void }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const target = view.target;
  const [host, setHost] = useState(target?.host ?? "");
  const [port, setPort] = useState(String(target?.port ?? 22));
  const [user, setUser] = useState(target?.user ?? "");
  const [path, setPath] = useState(target?.path ?? "uwumail-backup");
  const [method, setMethod] = useState<"key" | "password">(target?.method ?? "key");
  const [password, setPassword] = useState("");
  const [enabled, setEnabled] = useState(view.target ? view.enabled : true);
  const [hour, setHour] = useState(localHour(view.hour));
  const [encrypted, setEncrypted] = useState(view.target ? view.encrypted : true);
  const [daily, setDaily] = useState(String(view.retention.daily));
  const [weekly, setWeekly] = useState(String(view.retention.weekly));
  const [monthly, setMonthly] = useState(String(view.retention.monthly));

  const save = useMutation({
    mutationFn: () =>
      api<BackupsView & { recoveryKey?: string }>("/api/admin/backups", {
        method: "PUT",
        body: {
          enabled,
          hour: utcHour(hour),
          retention: { daily: Number(daily), weekly: Number(weekly), monthly: Number(monthly) },
          encrypted,
          target: {
            host: host.trim(),
            port: Number(port),
            user: user.trim(),
            path: path.trim(),
            method,
            ...(method === "password" && password ? { password } : {}),
          },
        },
      }),
    onSuccess: (next) => {
      queryClient.setQueryData(key, next);
      setPassword("");
      toast(t("common.saved"), "success");
      if (next.recoveryKey) onRecoveryKey(next.recoveryKey);
    },
  });
  const test = useMutation({
    mutationFn: () => api<{ hostKey: string; known: boolean }>("/api/admin/backups/test", { method: "POST", body: {} }),
    onSuccess: (result) => {
      void queryClient.invalidateQueries({ queryKey: key });
      toast(
        result.known ? t("backups.target.testOk") : t("backups.target.testFirst", { hostKey: result.hostKey }),
        "success",
      );
    },
  });
  const forget = useMutation({
    mutationFn: () => api<BackupsView>("/api/admin/backups/forget-host-key", { method: "POST", body: {} }),
    onSuccess: (next) => {
      queryClient.setQueryData(key, next);
      test.reset();
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    save.mutate();
  };
  const hostKeyChanged = test.error instanceof ApiError && test.error.code === "backupHostKeyChanged";
  const retentionField = (label: string, value: string, set: (value: string) => void) => (
    <Field label={label}>
      {(id) => (
        <TextInput id={id} type="number" min={0} max={60} value={value} onChange={(event) => set(event.target.value)} />
      )}
    </Field>
  );

  return (
    <Card title={t("backups.target.title")}>
      <form className="flex flex-col gap-4" onSubmit={submit}>
        <p className="-mt-1 text-[13px] text-muted">{t("backups.target.explain")}</p>
        <div className="grid gap-3 sm:grid-cols-[1fr_110px]">
          <Field label={t("backups.target.host")}>
            {(id) => (
              <TextInput
                id={id}
                required
                autoCapitalize="none"
                spellCheck={false}
                placeholder="nas.example.com"
                value={host}
                onChange={(event) => setHost(event.target.value)}
              />
            )}
          </Field>
          <Field label={t("backups.target.port")}>
            {(id) => (
              <TextInput
                id={id}
                type="number"
                min={1}
                max={65535}
                required
                value={port}
                onChange={(event) => setPort(event.target.value)}
              />
            )}
          </Field>
        </div>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label={t("backups.target.user")}>
            {(id) => (
              <TextInput
                id={id}
                required
                autoCapitalize="none"
                spellCheck={false}
                value={user}
                onChange={(event) => setUser(event.target.value)}
              />
            )}
          </Field>
          <Field label={t("backups.target.path")} hint={t("backups.target.pathHint")}>
            {(id) => (
              <TextInput id={id} spellCheck={false} value={path} onChange={(event) => setPath(event.target.value)} />
            )}
          </Field>
        </div>
        <Segmented
          label={t("backups.target.method")}
          value={method}
          onChange={setMethod}
          options={[
            { value: "key", label: t("backups.target.methodKey") },
            { value: "password", label: t("backups.target.methodPassword") },
          ]}
        />
        {method === "key" ? (
          target?.method === "key" && target.publicKey ? (
            <div className="flex flex-col gap-1.5">
              <p className="text-[13px] text-muted">{t("backups.target.publicKeyHint")}</p>
              <div className="flex items-center gap-2 rounded-control bg-canvas px-3 py-2">
                <code className="flex-1 font-mono text-[12px] break-all">{target.publicKey}</code>
                <CopyButton value={target.publicKey} />
              </div>
            </div>
          ) : (
            <p className="text-[13px] text-muted">{t("backups.target.keyAfterSave")}</p>
          )
        ) : (
          <Field
            label={t("backups.target.password")}
            hint={target?.passwordSet ? t("backups.target.passwordKept") : undefined}
          >
            {(id) => (
              <TextInput
                id={id}
                type="password"
                autoComplete="new-password"
                required={!target?.passwordSet}
                value={password}
                onChange={(event) => setPassword(event.target.value)}
              />
            )}
          </Field>
        )}
        {target?.hostKey && (
          <p className="text-[12px] text-muted">
            {t("backups.target.hostKey")} <code className="font-mono break-all">{target.hostKey}</code>
          </p>
        )}

        <div className="flex flex-col gap-3 border-t border-hairline pt-4">
          <Toggle checked={enabled} onChange={setEnabled} label={t("backups.schedule.enabled")} />
          <div className="grid gap-3 sm:grid-cols-4">
            <Field label={t("backups.schedule.hour")} className="sm:col-span-1">
              {(id) => (
                <Select id={id} value={hour} onChange={(event) => setHour(Number(event.target.value))}>
                  {Array.from({ length: 24 }, (_, value) => (
                    <option key={value} value={value}>
                      {String(value).padStart(2, "0")}:00
                    </option>
                  ))}
                </Select>
              )}
            </Field>
            {retentionField(t("backups.schedule.daily"), daily, setDaily)}
            {retentionField(t("backups.schedule.weekly"), weekly, setWeekly)}
            {retentionField(t("backups.schedule.monthly"), monthly, setMonthly)}
          </div>
          <Toggle
            checked={encrypted}
            onChange={setEncrypted}
            label={t("backups.schedule.encrypted")}
            description={
              view.status.lastSuccessAt ? t("backups.schedule.encryptedFixed") : t("backups.schedule.encryptedHint")
            }
          />
        </div>

        {(save.isError || test.isError) && (
          <p role="alert" className="text-[13px] text-danger">
            {errorText(save.error ?? test.error)}
          </p>
        )}
        <div className="flex flex-wrap justify-end gap-2">
          {hostKeyChanged && (
            <Button variant="danger" busy={forget.isPending} onClick={() => forget.mutate()}>
              {t("backups.target.trustNewKey")}
            </Button>
          )}
          <Button variant="ghost" icon={Plug} disabled={!target} busy={test.isPending} onClick={() => test.mutate()}>
            {t("backups.target.test")}
          </Button>
          <Button type="submit" variant="primary" busy={save.isPending}>
            {t("common.save")}
          </Button>
        </div>
      </form>
    </Card>
  );
}

function SnapshotsCard({ view }: { view: BackupsView }) {
  const { t, i18n } = useT();
  const [open, setOpen] = useState(false);
  const snapshots = useQuery({
    queryKey: [...key, "snapshots"],
    queryFn: () => api<BackupSnapshot[]>("/api/admin/backups/snapshots"),
    enabled: open,
  });
  const { confirmed, dialog } = usePasswordConfirmation();
  const errorText = useErrorText();
  const [shownKey, setShownKey] = useState<string | null>(null);
  const reveal = useMutation({
    mutationFn: () =>
      confirmed((password) =>
        api<{ recoveryKey: string }>("/api/admin/backups/recovery-key", { method: "POST", body: { password } }),
      ),
    onSuccess: (result) => setShownKey(result.recoveryKey),
    onError: (error) => {
      if (!(error instanceof Cancelled)) toast(errorText(error), "error");
    },
  });

  return (
    <Card title={t("backups.snapshots.title")}>
      <div className="flex flex-col gap-3">
        <p className="-mt-1 text-[13px] text-muted">{t("backups.snapshots.restoreHint")}</p>
        <code className="rounded-control bg-canvas px-3 py-2 font-mono text-[12px] break-all">
          uwumail-server backup restore --sftp user@host:/path --into /data
        </code>
        <div className="flex flex-wrap gap-2">
          {!open && (
            <Button disabled={!view.target} onClick={() => setOpen(true)}>
              {t("backups.snapshots.load")}
            </Button>
          )}
          {view.encrypted && (
            <Button variant="ghost" icon={KeyRound} busy={reveal.isPending} onClick={() => reveal.mutate()}>
              {t("backups.snapshots.showKey")}
            </Button>
          )}
        </div>
        {!open ? null : snapshots.isPending ? (
          <Loading />
        ) : snapshots.isError ? (
          <LoadError error={snapshots.error} onRetry={() => void snapshots.refetch()} />
        ) : snapshots.data.length === 0 ? (
          <p className="text-sm text-muted">{t("backups.snapshots.none")}</p>
        ) : (
          <ul className="flex flex-col divide-y divide-hairline">
            {snapshots.data.map((snapshot) => (
              <li key={snapshot.name} className="flex flex-wrap items-baseline justify-between gap-2 py-2 text-sm">
                <span className="font-semibold">{formatDateTime(snapshot.createdAt, i18n.language)}</span>
                <span className="text-[13px] text-muted">
                  {t("backups.snapshots.line", {
                    mails: snapshot.mails,
                    size: formatBytes(snapshot.size, i18n.language),
                    uploaded: formatBytes(snapshot.uploaded, i18n.language),
                  })}
                </span>
                <code className="w-full font-mono text-[11px] text-faint">{snapshot.name}</code>
              </li>
            ))}
          </ul>
        )}
      </div>
      {dialog}
      <RecoveryKeyDialog key={shownKey ?? ""} recoveryKey={shownKey} onClose={() => setShownKey(null)} />
    </Card>
  );
}

/** Backups to an SFTP server: where to, when, and what is there. */
export function BackupsPage() {
  const { t } = useT();
  const query = useQuery({
    queryKey: key,
    queryFn: () => api<BackupsView>("/api/admin/backups"),
    refetchInterval: (current) => (current.state.data?.running ? 3000 : false),
  });
  const [recoveryKey, setRecoveryKey] = useState<string | null>(null);
  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  const view = query.data;
  return (
    <div className="flex flex-col gap-5">
      <PageHeader title={t("backups.title")} intro={t("backups.intro")} />
      <StatusCard view={view} />
      <SettingsCard view={view} onRecoveryKey={setRecoveryKey} />
      <SnapshotsCard view={view} />
      <RecoveryKeyDialog key={recoveryKey ?? ""} recoveryKey={recoveryKey} onClose={() => setRecoveryKey(null)} />
    </div>
  );
}

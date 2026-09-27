import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArchiveRestore, Check, DatabaseBackup, FolderOpen, History, KeyRound, Plug, RefreshCw, X } from "lucide-react";
import { type FormEvent, useState } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card, CopyButton } from "@/components/ui/Card";
import { Dialog } from "@/components/ui/Dialog";
import { Field, Segmented, Select, TextInput, Toggle } from "@/components/ui/Field";
import { Cancelled, usePasswordConfirmation } from "@/features/security/ConfirmPassword";
import { useT } from "@/i18n";
import { ApiError, api, type BackupSnapshot, type BackupTarget, type BackupsView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatBytes, formatDateTime } from "@/lib/format";
import { localTime, minuteOptions, utcTime } from "@/lib/time";
import { toast } from "@/state/toasts";

const key = ["admin", "backups"] as const;

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
  const sftp = target?.kind === "sftp" ? target : null;
  const s3 = target?.kind === "s3" ? target : null;
  const [kind, setKind] = useState<BackupTarget["kind"]>(target?.kind ?? "sftp");
  const [host, setHost] = useState(sftp?.host ?? "");
  const [port, setPort] = useState(String(sftp?.port ?? 22));
  const [user, setUser] = useState(sftp?.user ?? "");
  const [path, setPath] = useState(sftp?.path ?? "uwumail-backup");
  const [method, setMethod] = useState<"key" | "password">(sftp?.method ?? "key");
  const [password, setPassword] = useState("");
  const [endpoint, setEndpoint] = useState(s3?.endpoint ?? "");
  const [region, setRegion] = useState(s3?.region ?? "");
  const [bucket, setBucket] = useState(s3?.bucket ?? "");
  const [prefix, setPrefix] = useState(s3?.prefix ?? "uwumail");
  const [accessKey, setAccessKey] = useState(s3?.accessKey ?? "");
  const [secretKey, setSecretKey] = useState("");
  const [pathStyle, setPathStyle] = useState(s3?.pathStyle ?? false);
  const [folder, setFolder] = useState(target?.kind === "folder" ? target.path : "/backup");
  const [enabled, setEnabled] = useState(view.target ? view.enabled : true);
  const [time, setTime] = useState(localTime(view.hour, view.minute));
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
          hour: Math.floor(utcTime(time) / 60),
          minute: utcTime(time) % 60,
          retention: { daily: Number(daily), weekly: Number(weekly), monthly: Number(monthly) },
          encrypted,
          target:
            kind === "s3"
              ? {
                  kind,
                  endpoint: endpoint.trim(),
                  region: region.trim(),
                  bucket: bucket.trim(),
                  prefix: prefix.trim(),
                  accessKey: accessKey.trim(),
                  pathStyle,
                  ...(secretKey ? { secretKey } : {}),
                }
              : kind === "folder"
                ? { kind, path: folder.trim() }
                : {
                    kind,
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
      setSecretKey("");
      toast(t("common.saved"), "success");
      if (next.recoveryKey) onRecoveryKey(next.recoveryKey);
    },
  });
  const test = useMutation({
    mutationFn: () =>
      api<{ kind: BackupTarget["kind"]; hostKey: string | null; known: boolean }>("/api/admin/backups/test", {
        method: "POST",
        body: {},
      }),
    onSuccess: (result) => {
      void queryClient.invalidateQueries({ queryKey: key });
      toast(
        result.kind !== "sftp"
          ? t("backups.target.testWritable")
          : result.known
            ? t("backups.target.testOk")
            : t("backups.target.testFirst", { hostKey: result.hostKey }),
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
        <Segmented
          label={t("backups.target.kind")}
          value={kind}
          onChange={setKind}
          options={[
            { value: "sftp", label: t("backups.target.kindSftp") },
            { value: "s3", label: t("backups.target.kindS3") },
            { value: "folder", label: t("backups.target.kindFolder") },
          ]}
        />
        <p className="-mt-1 text-[13px] text-muted">
          {kind === "s3"
            ? t("backups.target.explainS3")
            : kind === "folder"
              ? t("backups.target.explainFolder")
              : t("backups.target.explain")}
        </p>
        {kind === "sftp" && (
          <>
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
                  <TextInput
                    id={id}
                    spellCheck={false}
                    value={path}
                    onChange={(event) => setPath(event.target.value)}
                  />
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
              sftp?.method === "key" && sftp.publicKey ? (
                <div className="flex flex-col gap-1.5">
                  <p className="text-[13px] text-muted">{t("backups.target.publicKeyHint")}</p>
                  <div className="flex items-center gap-2 rounded-control bg-canvas px-3 py-2">
                    <code className="flex-1 font-mono text-[12px] break-all">{sftp.publicKey}</code>
                    <CopyButton value={sftp.publicKey} />
                  </div>
                </div>
              ) : (
                <p className="text-[13px] text-muted">{t("backups.target.keyAfterSave")}</p>
              )
            ) : (
              <Field
                label={t("backups.target.password")}
                hint={sftp?.passwordSet ? t("backups.target.passwordKept") : undefined}
              >
                {(id) => (
                  <TextInput
                    id={id}
                    type="password"
                    autoComplete="new-password"
                    required={!sftp?.passwordSet}
                    value={password}
                    onChange={(event) => setPassword(event.target.value)}
                  />
                )}
              </Field>
            )}
            {sftp?.hostKey && (
              <p className="text-[12px] text-muted">
                {t("backups.target.hostKey")} <code className="font-mono break-all">{sftp.hostKey}</code>
              </p>
            )}
          </>
        )}
        {kind === "s3" && (
          <>
            <Field label={t("backups.target.endpoint")} hint={t("backups.target.endpointHint")}>
              {(id) => (
                <TextInput
                  id={id}
                  required
                  type="url"
                  autoCapitalize="none"
                  spellCheck={false}
                  placeholder="https://s3.example.com"
                  value={endpoint}
                  onChange={(event) => setEndpoint(event.target.value)}
                />
              )}
            </Field>
            <div className="grid gap-3 sm:grid-cols-3">
              <Field label={t("backups.target.bucket")}>
                {(id) => (
                  <TextInput
                    id={id}
                    required
                    autoCapitalize="none"
                    spellCheck={false}
                    value={bucket}
                    onChange={(event) => setBucket(event.target.value)}
                  />
                )}
              </Field>
              <Field label={t("backups.target.prefix")} hint={t("backups.target.prefixHint")}>
                {(id) => (
                  <TextInput
                    id={id}
                    autoCapitalize="none"
                    spellCheck={false}
                    value={prefix}
                    onChange={(event) => setPrefix(event.target.value)}
                  />
                )}
              </Field>
              <Field label={t("backups.target.region")} hint={t("backups.target.regionHint")}>
                {(id) => (
                  <TextInput
                    id={id}
                    autoCapitalize="none"
                    spellCheck={false}
                    placeholder="us-east-1"
                    value={region}
                    onChange={(event) => setRegion(event.target.value)}
                  />
                )}
              </Field>
            </div>
            <div className="grid gap-3 sm:grid-cols-2">
              <Field label={t("backups.target.accessKey")}>
                {(id) => (
                  <TextInput
                    id={id}
                    required
                    autoCapitalize="none"
                    autoComplete="off"
                    spellCheck={false}
                    value={accessKey}
                    onChange={(event) => setAccessKey(event.target.value)}
                  />
                )}
              </Field>
              <Field
                label={t("backups.target.secretKey")}
                hint={s3?.secretKeySet ? t("backups.target.secretKeyKept") : undefined}
              >
                {(id) => (
                  <TextInput
                    id={id}
                    type="password"
                    autoComplete="new-password"
                    required={!s3?.secretKeySet}
                    value={secretKey}
                    onChange={(event) => setSecretKey(event.target.value)}
                  />
                )}
              </Field>
            </div>
            <Toggle
              checked={pathStyle}
              onChange={setPathStyle}
              label={t("backups.target.pathStyle")}
              description={t("backups.target.pathStyleHint")}
            />
          </>
        )}
        {kind === "folder" && (
          <Field label={t("backups.target.folder")} hint={t("backups.target.folderHint")}>
            {(id) => (
              <TextInput
                id={id}
                required
                autoCapitalize="none"
                spellCheck={false}
                placeholder="/backup"
                value={folder}
                onChange={(event) => setFolder(event.target.value)}
              />
            )}
          </Field>
        )}

        <div className="flex flex-col gap-3 border-t border-hairline pt-4">
          <Toggle checked={enabled} onChange={setEnabled} label={t("backups.schedule.enabled")} />
          <div className="grid gap-3 sm:grid-cols-4">
            <Field label={t("backups.schedule.hour")} className="sm:col-span-1">
              {(id) => (
                <div className="flex items-center gap-1">
                  <Select
                    id={id}
                    value={Math.floor(time / 60)}
                    onChange={(event) => setTime(Number(event.target.value) * 60 + (time % 60))}
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
                    aria-label={t("backups.schedule.hour")}
                    value={time % 60}
                    onChange={(event) => setTime(Math.floor(time / 60) * 60 + Number(event.target.value))}
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

/**
 * Putting a backup back: what the last one did, what is being fetched, and what is waiting.
 *
 * The buttons that start one sit in the snapshot list below, beside the snapshot they would put
 * back. This card is what happens afterwards, which is the part that needs explaining: the server
 * fetches the snapshot, stops itself, and the start after that puts the files in place — because
 * while it runs, the database it would replace is the one it is running on.
 */
function RestoreCard({ view }: { view: BackupsView }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const restore = view.restore;
  const forget = useMutation({
    mutationFn: () => api<BackupsView>("/api/admin/backups/restore", { method: "DELETE" }),
    onSuccess: (next) => queryClient.setQueryData(key, next),
    onError: (error) => toast(errorText(error), "error"),
  });

  const fetching = restore.fetching;
  const busy = fetching.state === "fetching";
  const nothing = !restore.last && !restore.staged && fetching.state === "idle";
  if (!restore.available || nothing) return null;

  return (
    <Card title={t("backups.restore.title")}>
      <div className="flex flex-col gap-3">
        {busy && (
          <p className="rounded-control bg-pink-tint px-3 py-2 text-[13px] text-pink-ink">
            {t("backups.restore.fetching", {
              snapshot: fetching.snapshot,
              size: formatBytes(fetching.totalBytes, i18n.language),
            })}
          </p>
        )}
        {fetching.state === "failed" && (
          <p className="rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning">
            {t("backups.restore.fetchFailed", { error: fetching.error })}
          </p>
        )}
        {restore.staged && (
          <p className="rounded-control bg-pink-tint px-3 py-2 text-[13px] text-pink-ink">
            {t("backups.restore.staged", {
              hostname: restore.staged.hostname,
              time: formatDateTime(restore.staged.createdAt, i18n.language),
            })}
          </p>
        )}
        {restore.last && (
          <div className="flex flex-col gap-2">
            <p
              className={
                restore.last.error
                  ? "rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning"
                  : "rounded-control bg-success-tint px-3 py-2 text-[13px] text-success"
              }
            >
              {restore.last.error
                ? t("backups.restore.lastFailed", { error: restore.last.error })
                : t("backups.restore.lastDone", {
                    hostname: restore.last.hostname,
                    time: formatDateTime(restore.last.createdAt, i18n.language),
                  })}
            </p>
            {!restore.last.error && (
              <>
                {/* The one thing nobody must find out by surprise weeks later. */}
                <p className="text-[13px] text-muted">{t("backups.restore.backupsOff")}</p>
                {restore.last.keptGateway && (
                  <p className="text-[13px] text-muted">{t("backups.restore.keptGateway")}</p>
                )}
                <p className="text-[13px] text-muted">{t("backups.restore.oldDatabase")}</p>
              </>
            )}
            <div>
              <Button variant="ghost" size="sm" icon={Check} busy={forget.isPending} onClick={() => forget.mutate()}>
                {t("backups.restore.forget")}
              </Button>
            </div>
          </div>
        )}
      </div>
    </Card>
  );
}

/** The command that puts a snapshot back on a machine without a running server. */
function restoreCommand(target: BackupTarget | null): string {
  if (target?.kind === "s3") {
    const where = `s3://${target.bucket}${target.prefix ? `/${target.prefix}` : ""}`;
    const style = target.pathStyle ? " --path-style" : "";
    return (
      "UWUMAIL_BACKUP_S3_ACCESS_KEY=… UWUMAIL_BACKUP_S3_SECRET_KEY=… uwumail-server backup restore " +
      `--s3 ${where} --endpoint ${target.endpoint} --region ${target.region}${style} --into /data`
    );
  }
  if (target?.kind === "folder") return `uwumail-server backup restore --folder ${target.path} --into /data`;
  return "uwumail-server backup restore --sftp user@host:/path --into /data";
}

function ProgressBar({ done, total, label }: { done: number; total: number; label: string }) {
  const share = total > 0 ? Math.min(1, done / total) : 0;
  return (
    <div
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(share * 100)}
      className="h-2.5 overflow-hidden rounded-full bg-canvas"
    >
      <div className="h-full rounded-full bg-pink transition-[width]" style={{ width: `${share * 100}%` }} />
    </div>
  );
}

/**
 * One person's mail out of a snapshot, next to what they have now: open a snapshot (its database
 * comes here), pick a person and folders, and the mail comes back into a folder "Restored <date>".
 */
function MailboxRestoreCard({ view }: { view: BackupsView }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const job = view.mailboxRestore;
  const [choosing, setChoosing] = useState(false);
  const [snapshot, setSnapshot] = useState("latest");
  const [login, setLogin] = useState("");
  const [into, setInto] = useState("");
  const [everything, setEverything] = useState(true);
  const [chosen, setChosen] = useState<number[]>([]);
  const snapshots = useQuery({
    queryKey: [...key, "snapshots"],
    queryFn: () => api<BackupSnapshot[]>("/api/admin/backups/snapshots"),
    enabled: choosing,
  });
  const open = useMutation({
    mutationFn: () => api<BackupsView>("/api/admin/backups/mailbox/open", { method: "POST", body: { snapshot } }),
    onSuccess: (next) => queryClient.setQueryData(key, next),
    onError: (error) => toast(errorText(error), "error"),
  });
  const restore = useMutation({
    mutationFn: () =>
      api<BackupsView>("/api/admin/backups/mailbox/restore", {
        method: "POST",
        body: { account: person?.login, into: into.trim() || null, folders: everything ? null : chosen },
      }),
    onSuccess: (next) => queryClient.setQueryData(key, next),
    onError: (error) => toast(errorText(error), "error"),
  });
  const close = useMutation({
    mutationFn: () => api<BackupsView>("/api/admin/backups/mailbox", { method: "DELETE" }),
    onSuccess: (next) => {
      queryClient.setQueryData(key, next);
      setLogin("");
      setChosen([]);
      setEverything(true);
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  const people = job.people;
  const person = people.find((candidate) => candidate.login === login) ?? people[0];
  const busy = job.state === "opening" || job.state === "restoring";
  const toggle = (id: number) =>
    setChosen((current) => (current.includes(id) ? current.filter((other) => other !== id) : [...current, id]));
  const folderName = (path: string[], role: string | null) =>
    role === "inbox" ? t("backups.mailbox.inbox") : (path[path.length - 1] ?? "");

  return (
    <Card title={t("backups.mailbox.title")}>
      <div className="flex flex-col gap-3">
        <p className="-mt-1 text-[13px] text-muted">{t("backups.mailbox.explain")}</p>
        {(job.state === "" || job.state === "failed") && (
          <>
            {job.state === "failed" && (
              <p role="alert" className="rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning">
                {t("backups.mailbox.openFailed", { error: job.error })}
              </p>
            )}
            <div className="flex flex-wrap items-end gap-2">
              <Field label={t("backups.mailbox.snapshot")} className="min-w-[220px] flex-1">
                {(id) => (
                  <Select
                    id={id}
                    value={snapshot}
                    onFocus={() => setChoosing(true)}
                    onChange={(event) => setSnapshot(event.target.value)}
                  >
                    <option value="latest">{t("backups.mailbox.latest")}</option>
                    {(snapshots.data ?? [])
                      .slice()
                      .reverse()
                      .map((entry) => (
                        <option key={entry.name} value={entry.name}>
                          {formatDateTime(entry.createdAt, i18n.language)}
                        </option>
                      ))}
                  </Select>
                )}
              </Field>
              <Button
                icon={FolderOpen}
                disabled={!view.target || view.running}
                busy={open.isPending}
                onClick={() => open.mutate()}
              >
                {t("backups.mailbox.open")}
              </Button>
            </div>
          </>
        )}
        {job.state === "opening" && (
          <div className="flex flex-col gap-2">
            <p className="text-[13px]">
              {t("backups.mailbox.opening", {
                done: formatBytes(job.doneBytes, i18n.language),
                total: formatBytes(job.totalBytes, i18n.language),
              })}
            </p>
            <ProgressBar done={job.doneBytes} total={job.totalBytes} label={t("backups.mailbox.title")} />
          </div>
        )}
        {(job.state === "open" || job.state === "restoring") && (
          <p className="text-sm">
            {t("backups.mailbox.opened", { time: formatDateTime(job.createdAt, i18n.language) })}
          </p>
        )}
        {job.state === "restoring" && (
          <div className="flex flex-col gap-2">
            <p className="text-[13px]">
              {t("backups.mailbox.restoring", { account: job.account, done: job.done, total: job.total })}
            </p>
            <ProgressBar done={job.done} total={job.total} label={t("backups.mailbox.title")} />
          </div>
        )}
        {job.last && !busy && (
          <p
            className={
              job.last.error
                ? "rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning"
                : "rounded-control bg-success-tint px-3 py-2 text-[13px] text-success"
            }
          >
            {job.last.error
              ? t("backups.mailbox.lastFailed", {
                  account: job.last.into,
                  error: job.last.error,
                  count: job.last.restored,
                })
              : job.last.restored === 0
                ? // Nothing came back, so no folder was made either: say so instead of naming one.
                  job.last.skipped > 0
                  ? t("backups.mailbox.nothingMissing", { count: job.last.skipped, account: job.last.into })
                  : t("backups.mailbox.nothingThere", { account: job.last.into })
                : t("backups.mailbox.lastDone", {
                    count: job.last.restored,
                    skipped: job.last.skipped,
                    account: job.last.into,
                    folder: job.last.folder,
                  })}
          </p>
        )}
        {job.state === "open" && people.length === 0 && (
          <p className="text-sm text-muted">{t("backups.mailbox.nobody")}</p>
        )}
        {job.state === "open" && person && (
          <div className="flex flex-col gap-3 border-t border-hairline pt-3">
            <div className="grid gap-3 sm:grid-cols-2">
              <Field label={t("backups.mailbox.person")}>
                {(id) => (
                  <Select
                    id={id}
                    value={person.login}
                    onChange={(event) => {
                      setLogin(event.target.value);
                      setChosen([]);
                    }}
                  >
                    {people.map((candidate) => (
                      <option key={candidate.login} value={candidate.login}>
                        {t("backups.mailbox.personLine", { login: candidate.login, count: candidate.emails })}
                      </option>
                    ))}
                  </Select>
                )}
              </Field>
              <Field label={t("backups.mailbox.into")} hint={t("backups.mailbox.intoHint")}>
                {(id) => (
                  <TextInput
                    id={id}
                    autoCapitalize="none"
                    spellCheck={false}
                    placeholder={person.login}
                    value={into}
                    onChange={(event) => setInto(event.target.value)}
                  />
                )}
              </Field>
            </div>
            <Segmented
              label={t("backups.mailbox.which")}
              value={everything ? "all" : "some"}
              onChange={(value) => setEverything(value === "all")}
              options={[
                { value: "all", label: t("backups.mailbox.allFolders") },
                { value: "some", label: t("backups.mailbox.someFolders") },
              ]}
            />
            {!everything && (
              <ul className="flex max-h-72 flex-col gap-1 overflow-y-auto rounded-control bg-canvas p-2">
                {person.folders.map((entry) => (
                  <li key={entry.id} style={{ paddingLeft: `${(entry.path.length - 1) * 16}px` }}>
                    <label className="flex items-center gap-2 text-sm">
                      <input
                        type="checkbox"
                        className="size-4 accent-pink"
                        checked={chosen.includes(entry.id)}
                        onChange={() => toggle(entry.id)}
                      />
                      <span className="min-w-0 flex-1 truncate">{folderName(entry.path, entry.role)}</span>
                      <span className="text-[12px] text-muted">{entry.emails}</span>
                    </label>
                  </li>
                ))}
              </ul>
            )}
            <p className="text-[13px] text-muted">{t("backups.mailbox.how")}</p>
          </div>
        )}
        {(job.state === "open" || job.state === "restoring") && (
          <div className="flex flex-wrap justify-end gap-2">
            <Button variant="ghost" icon={X} disabled={busy} busy={close.isPending} onClick={() => close.mutate()}>
              {t("backups.mailbox.close")}
            </Button>
            {person && (
              <Button
                variant="primary"
                icon={ArchiveRestore}
                disabled={busy || view.running || (!everything && chosen.length === 0)}
                busy={restore.isPending || job.state === "restoring"}
                onClick={() => restore.mutate()}
              >
                {t("backups.mailbox.restore")}
              </Button>
            )}
          </div>
        )}
      </div>
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
  const queryClient = useQueryClient();
  const [shownKey, setShownKey] = useState<string | null>(null);
  const [asking, setAsking] = useState<BackupSnapshot | null>(null);
  const [keepGateway, setKeepGateway] = useState(true);
  const restoring = useMutation({
    mutationFn: (snapshot: string) =>
      confirmed((password) =>
        api<BackupsView>("/api/admin/backups/restore", {
          method: "POST",
          body: { snapshot, keepGateway, password },
        }),
      ),
    onSuccess: (next) => {
      setAsking(null);
      queryClient.setQueryData(key, next);
    },
    onError: (error) => {
      if (!(error instanceof Cancelled)) toast(errorText(error), "error");
    },
  });
  const busy = view.restore.fetching.state === "fetching" || view.restore.staged !== null;
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
        <p className="-mt-1 text-[13px] text-muted">
          {t(view.restore.available ? "backups.snapshots.restoreHere" : "backups.snapshots.restoreHint")}
        </p>
        {/* Still worth showing: it is the way back on a machine that has no server running yet. */}
        <code className="rounded-control bg-canvas px-3 py-2 font-mono text-[12px] break-all">
          {restoreCommand(view.target)}
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
                <code className="min-w-0 flex-1 font-mono text-[11px] text-faint">{snapshot.name}</code>
                {view.restore.available && (
                  <Button size="sm" icon={History} disabled={busy} onClick={() => setAsking(snapshot)}>
                    {t("backups.restore.put")}
                  </Button>
                )}
              </li>
            ))}
          </ul>
        )}
      </div>
      <Dialog open={asking !== null} onClose={() => setAsking(null)} title={t("backups.restore.title")}>
        <div className="flex flex-col gap-4 px-6 pb-6">
          <p className="text-sm">
            {asking && t("backups.restore.from", { time: formatDateTime(asking.createdAt, i18n.language) })}
          </p>
          <p className="rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning">
            {t("backups.restore.warning")}
          </p>
          <p className="text-[13px] text-muted">{t("backups.restore.how")}</p>
          <Toggle
            checked={keepGateway}
            onChange={setKeepGateway}
            label={t("backups.restore.keepGatewayLabel")}
            description={t("backups.restore.keepGatewayHint")}
          />
          <div className="flex flex-wrap justify-end gap-2">
            <Button variant="ghost" onClick={() => setAsking(null)}>
              {t("common.cancel")}
            </Button>
            <Button
              variant="danger"
              icon={History}
              busy={restoring.isPending}
              onClick={() => asking && restoring.mutate(asking.name)}
            >
              {t("backups.restore.put")}
            </Button>
          </div>
        </div>
      </Dialog>
      {dialog}
      <RecoveryKeyDialog key={shownKey ?? ""} recoveryKey={shownKey} onClose={() => setShownKey(null)} />
    </Card>
  );
}

/** Backups to an SFTP server, an S3 bucket or a folder: where to, when, and what is there. */
export function BackupsPage() {
  const query = useQuery({
    queryKey: key,
    queryFn: () => api<BackupsView>("/api/admin/backups"),
    refetchInterval: (current) => {
      const data = current.state.data;
      // While a snapshot is being fetched the server is about to stop under us, so keep asking.
      if (data?.restore.fetching.state === "fetching") return 2000;
      const mailbox = data?.mailboxRestore.state;
      if (mailbox === "opening" || mailbox === "restoring") return 1500;
      return data?.running ? 3000 : false;
    },
    retry: (count, error) => !(error instanceof ApiError && error.status < 500) && count < 40,
    retryDelay: 3000,
  });
  const [recoveryKey, setRecoveryKey] = useState<string | null>(null);
  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  const view = query.data;
  return (
    <div className="flex flex-col gap-5">
      <RestoreCard view={view} />
      <StatusCard view={view} />
      <SettingsCard view={view} onRecoveryKey={setRecoveryKey} />
      <SnapshotsCard view={view} />
      {view.target && <MailboxRestoreCard view={view} />}
      <RecoveryKeyDialog key={recoveryKey ?? ""} recoveryKey={recoveryKey} onClose={() => setRecoveryKey(null)} />
    </div>
  );
}

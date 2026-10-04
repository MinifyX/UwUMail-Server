import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CalendarDays, CheckCircle2, Pause, Play, RefreshCw, Truck } from "lucide-react";
import { useState, type FormEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card, PageHeader } from "@/components/ui/Card";
import { EmptyState } from "@/components/ui/EmptyState";
import { Field, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { ApiError, api, type MoveJob, type MovingView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatBytes, formatDateTime } from "@/lib/format";
import { Link } from "@/lib/router";
import { toast } from "@/state/toasts";
import { LeftOut } from "./LeftOut";

const movingKey = ["account", "moving"] as const;

/** Providers that want something done on their side before a server of one's own may log in. */
export type Provider = "gmail" | "outlook" | "gmx" | "webde";

/** Which of the providers with their own rules an address belongs to, going by its domain. */
export function providerOf(address: string): Provider | null {
  const domain = address.trim().toLowerCase().split("@")[1] ?? "";
  if (domain === "gmail.com" || domain === "googlemail.com") return "gmail";
  if (domain === "msn.com" || /^(outlook|hotmail|live)\.[a-z.]+$/.test(domain)) return "outlook";
  if (/^gmx\.[a-z.]+$/.test(domain)) return "gmx";
  if (domain === "web.de") return "webde";
  return null;
}

function ProviderHint({ address }: { address: string }) {
  const { t } = useT();
  const provider = providerOf(address);
  if (!provider) return null;
  return (
    <p className="rounded-control bg-pink-tint px-3 py-2 text-[13px] text-pink-ink">{t(`moving.hints.${provider}`)}</p>
  );
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

function StartCard({ view }: { view: MovingView }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [address, setAddress] = useState("");
  const [password, setPassword] = useState("");
  const [manual, setManual] = useState(false);
  const [host, setHost] = useState("");
  const [port, setPort] = useState("993");
  const [login, setLogin] = useState("");
  const start = useMutation({
    mutationFn: () =>
      api<MoveJob>("/api/account/moving", {
        method: "POST",
        body: {
          address: address.trim(),
          password,
          ...(manual && host.trim() ? { host: host.trim(), port: Number(port), login: login.trim() || null } : {}),
        },
      }),
    onSuccess: () => {
      setAddress("");
      setPassword("");
      setManual(false);
      setHost("");
      setLogin("");
      void queryClient.invalidateQueries({ queryKey: movingKey });
      toast(t("moving.form.started"), "success");
    },
    onError: (error) => {
      // Nothing answered for this provider: the servers can still be given by hand.
      if (error instanceof ApiError && error.code === "providerNotFound") setManual(true);
    },
  });
  const full = view.jobs.length >= view.max;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    start.mutate();
  };

  return (
    <Card title={t("moving.form.title")}>
      <form className="flex flex-col gap-4" onSubmit={submit}>
        <p className="-mt-1 text-[13px] text-muted">{t("moving.form.explain")}</p>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label={t("moving.form.address")} hint={t("moving.form.addressHint")}>
            {(id) => (
              <TextInput
                id={id}
                type="email"
                required
                autoCapitalize="none"
                autoComplete="off"
                spellCheck={false}
                placeholder="name@example.com"
                value={address}
                onChange={(event) => setAddress(event.target.value)}
              />
            )}
          </Field>
          <Field label={t("moving.form.password")} hint={t("moving.form.passwordHint")}>
            {(id) => (
              <TextInput
                id={id}
                type="password"
                required
                autoComplete="off"
                value={password}
                onChange={(event) => setPassword(event.target.value)}
              />
            )}
          </Field>
        </div>
        <ProviderHint address={address} />
        {manual ? (
          <div className="grid gap-3 sm:grid-cols-[1fr_110px_1fr]">
            <Field label={t("moving.form.host")} hint={t("moving.form.hostHint")}>
              {(id) => (
                <TextInput
                  id={id}
                  autoCapitalize="none"
                  spellCheck={false}
                  placeholder="imap.example.com"
                  value={host}
                  onChange={(event) => setHost(event.target.value)}
                />
              )}
            </Field>
            <Field label={t("moving.form.port")}>
              {(id) => (
                <TextInput
                  id={id}
                  type="number"
                  min={1}
                  max={65535}
                  value={port}
                  onChange={(event) => setPort(event.target.value)}
                />
              )}
            </Field>
            <Field label={t("moving.form.login")} hint={t("moving.form.loginHint")}>
              {(id) => (
                <TextInput
                  id={id}
                  autoCapitalize="none"
                  spellCheck={false}
                  value={login}
                  onChange={(event) => setLogin(event.target.value)}
                />
              )}
            </Field>
          </div>
        ) : (
          <button
            type="button"
            className="self-start text-[13px] font-semibold text-pink-ink hover:underline"
            onClick={() => setManual(true)}
          >
            {t("moving.form.manual")}
          </button>
        )}
        {start.isError && (
          <p role="alert" className="text-[13px] text-danger">
            {errorText(start.error)}
          </p>
        )}
        <div className="flex flex-wrap items-center justify-end gap-3">
          {start.isPending && <span className="text-[13px] text-muted">{t("moving.form.searching")}</span>}
          <Button type="submit" variant="primary" icon={Truck} disabled={full} busy={start.isPending}>
            {t("moving.form.start")}
          </Button>
        </div>
      </form>
    </Card>
  );
}

/** Where a move stands, in words. */
function StateLine({ job }: { job: MoveJob }) {
  const { t, i18n } = useT();
  if (job.state === "paused") {
    return (
      <p className="flex items-start gap-1.5 text-[13px] text-warning">
        <AlertTriangle className="mt-0.5 size-3.5 shrink-0" aria-hidden />
        <span>
          {t(`moving.errors.${job.error || "failed"}`, { defaultValue: t("moving.errors.failed") })}
          {job.errorDetail && job.error !== "stopped" && (
            <span className="block text-[12px] text-muted">{job.errorDetail}</span>
          )}
        </span>
      </p>
    );
  }
  if (job.state === "done") {
    return (
      <p className="flex items-center gap-1.5 text-[13px] text-success">
        <CheckCircle2 className="size-3.5 shrink-0" aria-hidden />
        {job.finishedAt
          ? t("moving.job.doneAt", { date: formatDateTime(job.finishedAt, i18n.language) })
          : t("moving.job.done")}
      </p>
    );
  }
  return (
    <p className="text-[13px] text-pink-ink">
      {job.state === "running" ? t("moving.job.running") : t("moving.job.queued")}
    </p>
  );
}

function JobCard({ job }: { job: MoveJob }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [password, setPassword] = useState("");
  const refresh = (next?: MoveJob) => {
    if (next) {
      queryClient.setQueryData<MovingView>(movingKey, (view) =>
        view ? { ...view, jobs: view.jobs.map((other) => (other.id === next.id ? next : other)) } : view,
      );
    }
    void queryClient.invalidateQueries({ queryKey: movingKey });
  };
  const sync = useMutation({
    mutationFn: () =>
      api<MoveJob>(`/api/account/moving/${job.id}/sync`, {
        method: "POST",
        body: password ? { password } : {},
      }),
    onSuccess: (next) => {
      setPassword("");
      refresh(next);
      toast(t("moving.toasts.queued"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const pause = useMutation({
    mutationFn: () => api<MoveJob>(`/api/account/moving/${job.id}/pause`, { method: "POST", body: {} }),
    onSuccess: (next) => refresh(next),
    onError: (error) => toast(errorText(error), "error"),
  });
  const finish = useMutation({
    mutationFn: () => api<void>(`/api/account/moving/${job.id}`, { method: "DELETE" }),
    onSuccess: () => {
      refresh();
      toast(t("moving.toasts.finished"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const active = job.state === "queued" || job.state === "running";
  const passwordRefused = job.state === "paused" && job.error === "loginRefused";

  return (
    <Card title={job.address}>
      <div className="flex flex-col gap-3">
        <p className="-mt-2 text-[12px] text-muted">{t("moving.job.from", { host: job.host, login: job.login })}</p>
        <StateLine job={job} />
        {(active || job.messagesTotal > 0) && (
          <div className="flex flex-col gap-1.5">
            <ProgressBar done={job.messagesDone} total={job.messagesTotal} label={t("moving.job.progressLabel")} />
            <p className="text-[12px] text-muted">
              {job.messagesTotal > 0
                ? t("moving.job.progress", {
                    done: job.messagesDone,
                    total: job.messagesTotal,
                    size: formatBytes(job.bytesDone, i18n.language),
                  })
                : t("moving.job.looking")}
              {job.foldersTotal > 0 &&
                ` · ${t("moving.job.folders", { done: job.foldersDone, total: job.foldersTotal })}`}
            </p>
            <LeftOut counts={job} path={`/api/account/moving/${job.id}/skipped`} />
          </div>
        )}
        {passwordRefused && (
          <div className="flex flex-col gap-2">
            <ProviderHint address={job.address} />
            <Field label={t("moving.actions.newPassword")}>
              {(id) => (
                <TextInput
                  id={id}
                  type="password"
                  autoComplete="off"
                  value={password}
                  onChange={(event) => setPassword(event.target.value)}
                />
              )}
            </Field>
          </div>
        )}
        <div className="flex flex-wrap justify-end gap-2">
          {active && (
            <Button variant="ghost" icon={Pause} busy={pause.isPending} onClick={() => pause.mutate()}>
              {t("moving.actions.pause")}
            </Button>
          )}
          {job.state === "paused" && (
            <Button
              icon={Play}
              busy={sync.isPending}
              disabled={passwordRefused && !password}
              onClick={() => sync.mutate()}
            >
              {t("moving.actions.continue")}
            </Button>
          )}
          {job.state === "done" && (
            <Button icon={RefreshCw} busy={sync.isPending} onClick={() => sync.mutate()}>
              {t("moving.actions.syncAgain")}
            </Button>
          )}
          <Button
            variant={job.state === "done" ? "primary" : "ghost"}
            icon={CheckCircle2}
            busy={finish.isPending}
            onClick={() => {
              if (window.confirm(t("moving.actions.finishConfirm", { address: job.address }))) finish.mutate();
            }}
          >
            {t("moving.actions.finish")}
          </Button>
        </div>
      </div>
    </Card>
  );
}

/** My account → Moving: bringing the mail of an old mailbox over, in the background. */
export function MovingPage() {
  const { t } = useT();
  const view = useQuery({
    queryKey: movingKey,
    queryFn: () => api<MovingView>("/api/account/moving"),
    // While something is being copied the page follows it; otherwise there is nothing to ask.
    refetchInterval: (current) =>
      current.state.data?.jobs.some((job) => job.state === "queued" || job.state === "running") ? 3000 : false,
  });

  if (view.isPending) return <Loading />;
  if (view.isError) return <LoadError error={view.error} onRetry={() => void view.refetch()} />;

  return (
    <div className="flex flex-col gap-6">
      <PageHeader title={t("moving.title")} intro={t("moving.intro")} />
      {!view.data.hasMailbox ? (
        <Card>
          <EmptyState scene="inbox" title={t("moving.noMailbox.title")} body={t("moving.noMailbox.body")} />
        </Card>
      ) : (
        <>
          {view.data.jobs.map((job) => (
            <JobCard key={job.id} job={job} />
          ))}
          {view.data.jobs.length < view.data.max && <StartCard view={view.data} />}
        </>
      )}
      <Card title={t("moving.about.title")}>
        <div className="flex flex-col gap-2 text-[13px] text-muted">
          <p>{t("moving.about.what")}</p>
          <p>{t("moving.about.twice")}</p>
          <p>{t("moving.about.switch")}</p>
          <p>{t("moving.about.password")}</p>
          <p className="flex items-start gap-2">
            <CalendarDays className="mt-0.5 size-4 shrink-0" aria-hidden />
            <span>
              {t("moving.about.calendars")}{" "}
              <Link to="/account/calendars" className="font-semibold text-pink-ink hover:underline">
                {t("moving.about.calendarsLink")}
              </Link>
            </span>
          </p>
        </div>
      </Card>
    </div>
  );
}

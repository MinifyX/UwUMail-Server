import { useQuery } from "@tanstack/react-query";
import clsx from "clsx";
import { ArrowLeft, ArrowRight, Eye, EyeOff, RefreshCw } from "lucide-react";
import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import { NyuScene, type SceneName } from "@/components/nyu/scenes";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button, IconButton } from "@/components/ui/Button";
import { CopyButton } from "@/components/ui/Card";
import { EmptyState } from "@/components/ui/EmptyState";
import { Field, Segmented, TextInput } from "@/components/ui/Field";
import { Wordmark } from "@/components/ui/Logo";
import { useT } from "@/i18n";
import { api, ApiError, type DomainSummary, type Session } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { navigate } from "@/lib/router";
import { usePrefs, type Mode } from "@/state/prefs";
import { RecordList } from "@/features/domains/DnsBits";
import { useCheckDomain, useDomain } from "@/features/domains/queries";
import { useInfo, useSavePrefs, useStartSession } from "@/features/session/session";
import { AddressChecks, CheckedAt, Checking, CloudflarePanel, DeliveryChecks, TestMailPanel } from "./SetupBits";
import { useCompleteSetup, useLastServerCheck, useRunServerCheck, useSetupStatus, useVerifySetupCode } from "./queries";

const STEPS = ["welcome", "admin", "dns", "sending", "checks", "testMail", "done"] as const;
type Step = (typeof STEPS)[number];

const SCENES: Record<Step, SceneName> = {
  welcome: "welcome",
  admin: "pick",
  dns: "search",
  sending: "inbox",
  checks: "search",
  testMail: "inbox",
  done: "done",
};

const STORAGE_KEY = "uwumail-setup";
const MIN_CHARS = 10;

interface Progress {
  step: Step;
  domain: string;
}

function loadProgress(): Progress {
  try {
    const saved = JSON.parse(sessionStorage.getItem(STORAGE_KEY) ?? "{}") as Partial<Progress>;
    const step = STEPS.includes(saved.step as Step) ? (saved.step as Step) : "welcome";
    return { step, domain: typeof saved.domain === "string" ? saved.domain : "" };
  } catch {
    return { step: "welcome", domain: "" };
  }
}

function saveProgress(progress: Progress | null) {
  try {
    if (progress) sessionStorage.setItem(STORAGE_KEY, JSON.stringify(progress));
    else sessionStorage.removeItem(STORAGE_KEY);
  } catch {
    // Without storage a reload starts at the first step that fits.
  }
}

/** "mail.example.com" suggests "example.com". */
function domainFromHostname(hostname: string): string {
  const labels = hostname.split(".");
  return labels.length > 2 ? labels.slice(1).join(".") : hostname;
}

function ModeSwitch({ loggedIn }: { loggedIn: boolean }) {
  const { t } = useT();
  const mode = usePrefs((s) => s.mode);
  const save = useSavePrefs();
  return (
    <Segmented<Mode>
      label={t("mode.label")}
      value={mode}
      onChange={(value) => (loggedIn ? save.mutate({ mode: value }) : usePrefs.getState().apply({ mode: value }))}
      options={[
        { value: "simple", label: t("mode.simple") },
        { value: "pro", label: t("mode.pro") },
      ]}
    />
  );
}

function Frame({
  step,
  loggedIn,
  title,
  body,
  children,
  footer,
}: {
  step: Step;
  loggedIn: boolean;
  title: string;
  body?: string;
  children: ReactNode;
  footer?: ReactNode;
}) {
  const { t } = useT();
  const explain = usePrefs((s) => s.mode) === "simple";
  const heading = useRef<HTMLHeadingElement>(null);
  const index = STEPS.indexOf(step);

  useEffect(() => {
    window.scrollTo({ top: 0 });
    heading.current?.focus({ preventScroll: true });
  }, [step]);

  return (
    <div className="min-h-screen">
      <header className="sticky top-0 z-30 border-b border-hairline bg-canvas/85 backdrop-blur">
        <div className="mx-auto flex h-16 w-full max-w-[1000px] items-center gap-3 px-4 sm:px-6">
          <Wordmark className="text-base" />
          <span className="hidden text-[13px] font-semibold text-muted sm:inline">{t("setup.title")}</span>
          <div className="ml-auto">
            <ModeSwitch loggedIn={loggedIn} />
          </div>
        </div>
        <div className="mx-auto w-full max-w-[1000px] px-4 pb-3 sm:px-6">
          <p className="sr-only">{t("setup.progress", { step: index + 1, count: STEPS.length })}</p>
          <ol className="flex gap-1.5" aria-hidden>
            {STEPS.map((entry, position) => (
              <li key={entry} className="min-w-0 flex-1">
                <span
                  className={clsx(
                    "block h-1.5 rounded-full transition-colors duration-300",
                    position <= index ? "bg-pink" : "bg-line",
                  )}
                />
                <span
                  className={clsx(
                    "mt-1.5 hidden truncate text-[11px] font-semibold md:block",
                    position === index ? "text-pink-ink" : "text-faint",
                  )}
                >
                  {t(`setup.steps.${entry}`)}
                </span>
              </li>
            ))}
          </ol>
        </div>
      </header>

      <main className="mx-auto w-full max-w-[1000px] px-4 py-8 sm:px-6">
        <div
          key={step}
          className={clsx("grid animate-slide-up gap-6", explain && "md:grid-cols-[240px_minmax(0,1fr)] md:gap-10")}
        >
          {explain && (
            <div className="flex justify-center md:block">
              <NyuScene name={SCENES[step]} className="h-auto w-[180px] md:sticky md:top-32 md:w-full" />
            </div>
          )}
          <div className="flex min-w-0 flex-col gap-5">
            <div>
              <p className="text-[12px] font-semibold tracking-wide text-faint uppercase">
                {t("setup.progress", { step: index + 1, count: STEPS.length })}
              </p>
              <h1 ref={heading} tabIndex={-1} className="mt-1 text-[24px] font-bold tracking-[-0.01em] outline-none">
                {title}
              </h1>
              {body && explain && <p className="mt-2 text-[15px] text-muted">{body}</p>}
            </div>
            <div className="rounded-[22px] border border-hairline bg-surface p-5 shadow-float sm:p-6">{children}</div>
            {footer && <div className="flex flex-wrap items-center justify-between gap-3">{footer}</div>}
          </div>
        </div>
      </main>
    </div>
  );
}

function Nav({ onBack, onNext, nextLabel }: { onBack?: () => void; onNext: () => void; nextLabel?: string }) {
  const { t } = useT();
  return (
    <>
      {onBack ? (
        <Button variant="ghost" icon={ArrowLeft} onClick={onBack}>
          {t("setup.back")}
        </Button>
      ) : (
        <span />
      )}
      <Button variant="primary" size="lg" onClick={onNext}>
        {nextLabel ?? t("setup.next")}
        <ArrowRight className="size-4" aria-hidden />
      </Button>
    </>
  );
}

function WelcomeStep({ hostname, onCode }: { hostname: string; onCode: (code: string) => void }) {
  const { t } = useT();
  const errorText = useErrorText();
  const explain = usePrefs((s) => s.mode) === "simple";
  const verify = useVerifySetupCode();
  const [code, setCode] = useState("");
  const command = "docker compose logs uwumail | grep setup";

  const submit = (event: FormEvent) => {
    event.preventDefault();
    verify.mutate(code, { onSuccess: () => onCode(code) });
  };

  return (
    <Frame step="welcome" loggedIn={false} title={t("setup.welcome.title")} body={t("setup.welcome.body")}>
      <form className="flex flex-col gap-4" onSubmit={submit}>
        <Field
          label={t("setup.welcome.codeLabel")}
          hint={t("setup.welcome.codeHint")}
          error={verify.isError ? errorText(verify.error) : undefined}
        >
          {(id) => (
            <TextInput
              id={id}
              autoFocus
              required
              autoComplete="off"
              spellCheck={false}
              placeholder="xxxx-xxxx-xxxx"
              className="font-mono tracking-wider"
              value={code}
              onChange={(event) => setCode(event.target.value)}
            />
          )}
        </Field>
        <div className="flex flex-col gap-2 rounded-control bg-canvas p-3">
          <p className="text-[13px] text-muted">
            {explain ? t("setup.welcome.whereSimple") : t("setup.welcome.wherePro")}
          </p>
          <div className="flex items-center gap-1 rounded-control bg-surface px-2.5 py-1.5">
            <code className="min-w-0 flex-1 text-[12px] break-all select-all">{command}</code>
            <CopyButton value={command} />
          </div>
        </div>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <span className="text-[12px] text-faint">{t("setup.welcome.hostname", { hostname })}</span>
          <Button type="submit" variant="primary" size="lg" busy={verify.isPending} disabled={!code.trim()}>
            {t("setup.welcome.submit")}
            <ArrowRight className="size-4" aria-hidden />
          </Button>
        </div>
      </form>
    </Frame>
  );
}

function AdminStep({
  code,
  hostname,
  domains,
  onBack,
  onDone,
}: {
  code: string;
  hostname: string;
  domains: string[];
  onBack: () => void;
  onDone: (session: Session, domain: string) => void;
}) {
  const { t } = useT();
  const errorText = useErrorText();
  const complete = useCompleteSetup();
  const [domain, setDomain] = useState(domains[0] ?? domainFromHostname(hostname));
  const [localPart, setLocalPart] = useState("");
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [repeat, setRepeat] = useState("");
  const [visible, setVisible] = useState(false);
  const [touched, setTouched] = useState(false);

  const cleanDomain = domain.trim().toLowerCase();
  const login = `${localPart.trim().toLowerCase()}@${cleanDomain}`;
  const missing = Math.max(0, MIN_CHARS - [...password].length);
  const mismatch = touched && repeat.length > 0 && repeat !== password;
  const codeGone = complete.error instanceof ApiError && complete.error.code === "setupCodeInvalid";

  const submit = (event: FormEvent) => {
    event.preventDefault();
    setTouched(true);
    if (missing > 0 || repeat !== password) return;
    complete.mutate(
      { code, domain: cleanDomain, localPart: localPart.trim(), name: name.trim(), password },
      { onSuccess: (session) => onDone(session, cleanDomain) },
    );
  };

  return (
    <Frame step="admin" loggedIn={false} title={t("setup.admin.title")} body={t("setup.admin.body")}>
      <form className="flex flex-col gap-4" onSubmit={submit}>
        <Field label={t("setup.admin.domain")} hint={t("setup.admin.domainHint")}>
          {(id) => (
            <TextInput
              id={id}
              required
              autoFocus
              autoComplete="off"
              spellCheck={false}
              placeholder="example.com"
              value={domain}
              onChange={(event) => setDomain(event.target.value)}
            />
          )}
        </Field>
        <div className="grid gap-4 sm:grid-cols-2">
          <Field label={t("setup.admin.localPart")} hint={cleanDomain && localPart.trim() ? login : undefined}>
            {(id) => (
              <TextInput
                id={id}
                required
                autoComplete="off"
                spellCheck={false}
                value={localPart}
                onChange={(event) => setLocalPart(event.target.value.replace(/@.*/, ""))}
              />
            )}
          </Field>
          <Field label={t("setup.admin.name")} hint={t("setup.admin.nameHint")}>
            {(id) => (
              <TextInput id={id} autoComplete="name" value={name} onChange={(event) => setName(event.target.value)} />
            )}
          </Field>
        </div>
        {/* Lets password managers save the new password for the right address. */}
        <input type="email" autoComplete="username" value={login} readOnly hidden />
        <div className="grid gap-4 sm:grid-cols-2">
          <Field
            label={t("setup.admin.password")}
            hint={missing > 0 && password ? t("password.missing", { count: missing }) : t("password.hint")}
          >
            {(id) => (
              <span className="relative block">
                <TextInput
                  id={id}
                  type={visible ? "text" : "password"}
                  autoComplete="new-password"
                  required
                  className="pr-12"
                  value={password}
                  onChange={(event) => setPassword(event.target.value)}
                />
                <IconButton
                  size="sm"
                  icon={visible ? EyeOff : Eye}
                  label={visible ? t("common.hidePassword") : t("common.showPassword")}
                  className="absolute top-1/2 right-1.5 -translate-y-1/2"
                  onClick={() => setVisible((value) => !value)}
                />
              </span>
            )}
          </Field>
          <Field label={t("setup.admin.repeat")} error={mismatch ? t("password.mismatch") : undefined}>
            {(id) => (
              <TextInput
                id={id}
                type={visible ? "text" : "password"}
                autoComplete="new-password"
                required
                value={repeat}
                onChange={(event) => setRepeat(event.target.value)}
                onBlur={() => setTouched(true)}
              />
            )}
          </Field>
        </div>
        <p className="rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
          {t("setup.admin.hostnameNote", { hostname })}
        </p>
        {complete.isError && (
          <p role="alert" className="text-[13px] text-danger">
            {errorText(complete.error)}
          </p>
        )}
        <div className="flex flex-wrap items-center justify-between gap-3">
          <Button variant="ghost" icon={ArrowLeft} onClick={onBack}>
            {codeGone ? t("setup.admin.newCode") : t("setup.back")}
          </Button>
          <Button type="submit" variant="primary" size="lg" busy={complete.isPending} disabled={missing > 0}>
            {t("setup.admin.submit")}
            <ArrowRight className="size-4" aria-hidden />
          </Button>
        </div>
      </form>
    </Frame>
  );
}

function DnsStep({ domain, hostname, onNext }: { domain: string; hostname: string; onNext: () => void }) {
  const { t } = useT();
  const explain = usePrefs((s) => s.mode) === "simple";
  const query = useDomain(domain);
  const check = useCheckDomain(domain, t("domains.toasts.checked"));
  const checked = useRef(false);

  useEffect(() => {
    // A fresh domain has never been checked; do it once on the way in.
    if (query.data && !query.data.report && !checked.current) {
      checked.current = true;
      check.mutate();
    }
  }, [query.data, check]);

  const report = query.data?.report ?? null;
  const allOk = report?.records.every(
    (record) => record.optional || record.status === "ok" || record.keyState === "pending",
  );

  return (
    <Frame
      step="dns"
      loggedIn
      title={t("setup.dns.title", { domain })}
      body={t("setup.dns.body")}
      footer={<Nav onNext={onNext} />}
    >
      {query.isPending ? (
        <Loading />
      ) : query.isError ? (
        <LoadError error={query.error} onRetry={() => void query.refetch()} />
      ) : (
        <div className="flex flex-col gap-4">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <p className="text-[13px] text-muted">{t("setup.dns.hostnameNote", { hostname })}</p>
            <Button size="sm" icon={RefreshCw} busy={check.isPending} onClick={() => check.mutate()}>
              {check.isPending ? t("setup.dns.checking") : t("setup.dns.check")}
            </Button>
          </div>
          {query.data.setup.upstreamMx && (
            <p className="rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">{t("domains.detail.upstream")}</p>
          )}
          {report ? (
            <>
              {allOk && (
                <p className="rounded-control bg-success-tint px-3 py-2.5 text-[13px] font-semibold text-success">
                  {t("setup.dns.allOk")}
                </p>
              )}
              <RecordList records={report.records} domain={domain} explain={explain} />
              <CloudflarePanel domain={domain} report={report} explain={explain} />
              {!allOk && <p className="text-[13px] text-muted">{t("setup.dns.later")}</p>}
            </>
          ) : (
            <Checking />
          )}
        </div>
      )}
    </Frame>
  );
}

/** Runs the server check once when a step needs it and none is recent. */
function useServerCheck() {
  const last = useLastServerCheck();
  const run = useRunServerCheck();
  const started = useRef(false);
  useEffect(() => {
    if (last.isPending || started.current) return;
    const stale = !last.data || Date.now() / 1000 - last.data.checkedAt > 600;
    if (stale) {
      started.current = true;
      run.mutate(false);
    }
  }, [last.isPending, last.data, run]);
  return { check: last.data ?? null, run, loading: last.isPending };
}

function SendingStep({ onBack, onNext }: { onBack: () => void; onNext: () => void }) {
  const { t } = useT();
  const explain = usePrefs((s) => s.mode) === "simple";
  const { check, run } = useServerCheck();
  return (
    <Frame
      step="sending"
      loggedIn
      title={t("setup.sending.title")}
      body={t("setup.sending.body")}
      footer={<Nav onBack={onBack} onNext={onNext} />}
    >
      <div className="flex flex-col gap-4">
        {run.isPending && <Checking />}
        {check && !run.isPending && (
          <>
            <div className="flex flex-wrap items-center justify-between gap-3">
              <CheckedAt check={check} />
              <Button size="sm" icon={RefreshCw} onClick={() => run.mutate(false)}>
                {t("setup.sending.run")}
              </Button>
            </div>
            <DeliveryChecks check={check} explain={explain} onRecheck={() => run.mutate(false)} />
          </>
        )}
      </div>
    </Frame>
  );
}

function ChecksStep({ onBack, onNext }: { onBack: () => void; onNext: () => void }) {
  const { t } = useT();
  const explain = usePrefs((s) => s.mode) === "simple";
  const { check, run } = useServerCheck();
  return (
    <Frame
      step="checks"
      loggedIn
      title={t("setup.checks.title")}
      body={t("setup.checks.body")}
      footer={<Nav onBack={onBack} onNext={onNext} />}
    >
      <div className="flex flex-col gap-4">
        {run.isPending && run.variables === false && <Checking />}
        {check && !(run.isPending && run.variables === false) && (
          <AddressChecks check={check} explain={explain} busy={run.isPending} onBlocklists={() => run.mutate(true)} />
        )}
      </div>
    </Frame>
  );
}

function TestMailStep({ login, onBack, onNext }: { login: string; onBack: () => void; onNext: () => void }) {
  const { t } = useT();
  const explain = usePrefs((s) => s.mode) === "simple";
  return (
    <Frame
      step="testMail"
      loggedIn
      title={t("setup.testMail.title")}
      body={t("setup.testMail.body")}
      footer={<Nav onBack={onBack} onNext={onNext} />}
    >
      <TestMailPanel login={login} explain={explain} />
    </Frame>
  );
}

function DoneStep({ onBack }: { onBack: () => void }) {
  const { t } = useT();
  return (
    <Frame
      step="done"
      loggedIn
      title={t("setup.done.title")}
      body={t("setup.done.body")}
      footer={
        <Nav
          onBack={onBack}
          nextLabel={t("setup.done.open")}
          onNext={() => {
            saveProgress(null);
            navigate("/admin", { replace: true });
          }}
        />
      }
    >
      <div className="flex flex-col gap-2 text-sm">
        <p>{t("setup.done.summary")}</p>
        <p className="text-muted">{t("setup.done.next")}</p>
      </div>
    </Frame>
  );
}

function Closed() {
  const { t } = useT();
  const info = useInfo();
  const noCode = info.data?.setupRequired;
  return (
    <main className="flex min-h-screen items-center justify-center px-4 py-10">
      <div className="w-full max-w-[460px]">
        <div className="mb-6 flex justify-center">
          <Wordmark className="text-2xl" />
        </div>
        <EmptyState
          scene={noCode ? "loadError" : "done"}
          title={noCode ? t("setup.noCode.title") : t("setup.closed.title")}
          body={noCode ? t("setup.noCode.body") : t("setup.closed.body")}
          action={
            noCode ? undefined : (
              <Button variant="primary" onClick={() => navigate("/login", { replace: true })}>
                {t("setup.closed.login")}
              </Button>
            )
          }
        />
      </div>
    </main>
  );
}

/** The full-screen setup assistant at /setup. Before the first admin it needs the code from the log. */
export function SetupWizard({ session }: { session: Session | null }) {
  const [progress, setProgress] = useState(loadProgress);
  const [code, setCode] = useState("");
  const status = useSetupStatus();
  const start = useStartSession();
  const isAdmin = session?.account.role === "admin";
  const domains = useQuery({
    queryKey: ["admin", "domains"],
    queryFn: () => api<DomainSummary[]>("/api/admin/domains"),
    enabled: isAdmin,
  });

  useEffect(() => {
    if (session && !isAdmin) navigate("/account", { replace: true });
  }, [session, isAdmin]);

  const go = (step: Step, domain = progress.domain) => {
    const next = { step, domain };
    setProgress(next);
    saveProgress(next);
  };

  if (session && !isAdmin) return null;

  if (!session) {
    if (status.isPending) return <Loading fullPage />;
    if (status.isError) {
      return (
        <main className="flex min-h-screen items-center justify-center">
          <LoadError error={status.error} onRetry={() => void status.refetch()} />
        </main>
      );
    }
    if (!status.data.open) return <Closed />;
    const hostname = status.data.hostname;
    if (progress.step === "admin" && code) {
      return (
        <AdminStep
          code={code}
          hostname={hostname}
          domains={status.data.domains}
          onBack={() => go("welcome")}
          onDone={(created, domain) => {
            go("dns", domain);
            start(created, "/setup");
          }}
        />
      );
    }
    return (
      <WelcomeStep
        hostname={hostname}
        onCode={(value) => {
          setCode(value);
          go("admin");
        }}
      />
    );
  }

  const hostname = session.server.hostname;
  const domain = progress.domain || domains.data?.[0]?.name || "";
  const step: Step = STEPS.indexOf(progress.step) < STEPS.indexOf("dns") ? "dns" : progress.step;

  switch (step) {
    case "sending":
      return <SendingStep onBack={() => go("dns", domain)} onNext={() => go("checks", domain)} />;
    case "checks":
      return <ChecksStep onBack={() => go("sending", domain)} onNext={() => go("testMail", domain)} />;
    case "testMail":
      return (
        <TestMailStep
          login={session.account.login}
          onBack={() => go("checks", domain)}
          onNext={() => go("done", domain)}
        />
      );
    case "done":
      return <DoneStep onBack={() => go("testMail", domain)} />;
    default:
      if (!domain) return domains.isPending ? <Loading fullPage /> : <DoneStep onBack={() => go("testMail")} />;
      return <DnsStep domain={domain} hostname={hostname} onNext={() => go("sending", domain)} />;
  }
}

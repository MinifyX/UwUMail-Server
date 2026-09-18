import { useQuery } from "@tanstack/react-query";
import clsx from "clsx";
import { CircleCheck, CircleHelp, CircleX, Cloud, Info, Mail, Send, ShieldCheck, TriangleAlert } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { useEffect, useState, type FormEvent, type ReactNode } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Field, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type DomainReport, type ServerCheck, type SettingsView, type TestMailSent } from "@/lib/api";
import { formatDateTime } from "@/lib/format";
import { Link } from "@/lib/router";
import { DeliveryFields, Section } from "@/features/settings/SettingsPage";
import { blocklistLines, ptrLines, receivingLines, sendingLines, type CheckLineData, type LineLevel } from "./lines";
import { useCloudflare, useSendTestMail, useTestMailStatus } from "./queries";

const LEVELS: Record<LineLevel, { icon: LucideIcon; className: string }> = {
  ok: { icon: CircleCheck, className: "text-success" },
  unknown: { icon: CircleHelp, className: "text-muted" },
  warning: { icon: TriangleAlert, className: "text-warning" },
  problem: { icon: CircleX, className: "text-danger" },
};

/** A small heading inside a card. */
export function SubHeading({ icon: Icon, children }: { icon: LucideIcon; children: ReactNode }) {
  return (
    <h3 className="flex items-center gap-2 text-sm font-bold">
      <Icon className="size-4 text-pink-ink" aria-hidden />
      {children}
    </h3>
  );
}

export function CheckLines({
  lines,
  explain,
  onRelay,
}: {
  lines: CheckLineData[];
  explain: boolean;
  onRelay?: () => void;
}) {
  const { t, i18n } = useT();
  return (
    <ul className="flex flex-col gap-2.5">
      {lines.map((entry, index) => {
        const { icon: Icon, className } = LEVELS[entry.level];
        const hintKey = `setup.hints.${entry.code}`;
        const hint = entry.level !== "ok" && i18n.exists(hintKey) ? t(hintKey, entry.params) : null;
        return (
          <li key={`${entry.code}-${index}`} className="flex gap-2.5">
            <Icon className={clsx("mt-0.5 size-[18px] shrink-0", className)} aria-hidden />
            <div className="flex min-w-0 flex-col gap-1">
              <p className="text-sm break-words">{t(`setup.lines.${entry.code}`, entry.params)}</p>
              {hint && (explain || entry.level === "problem") && <p className="text-[13px] text-muted">{hint}</p>}
              {entry.relay && onRelay && (
                <Button size="sm" icon={Send} className="mt-1 self-start" onClick={onRelay}>
                  {t("setup.sending.relayButton")}
                </Button>
              )}
            </div>
          </li>
        );
      })}
    </ul>
  );
}

export function CheckedAt({ check }: { check: { checkedAt: number } }) {
  const { t, i18n } = useT();
  return (
    <p className="text-[12px] text-faint">
      {t("setup.page.lastRun", { time: formatDateTime(check.checkedAt, i18n.language) })}
    </p>
  );
}

/** A friendly note while the check runs; it can take up to a minute. */
export function Checking() {
  const { t } = useT();
  return (
    <p className="flex items-center gap-2.5 rounded-control bg-pink-tint/60 px-3 py-2.5 text-[13px] text-pink-ink">
      <span
        className="size-4 shrink-0 animate-spin rounded-full border-2 border-current border-t-transparent"
        aria-hidden
      />
      {t("setup.sending.running")}
    </p>
  );
}

/** Sending and receiving, with the relay form when mail cannot get out. */
export function DeliveryChecks({
  check,
  explain,
  onRecheck,
}: {
  check: ServerCheck;
  explain: boolean;
  onRecheck: () => void;
}) {
  const { t } = useT();
  const [relayOpen, setRelayOpen] = useState(false);
  const sending = sendingLines(check);
  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-col gap-3">
        <SubHeading icon={Send}>{t("setup.sending.outTitle")}</SubHeading>
        <CheckLines lines={sending} explain={explain} onRelay={relayOpen ? undefined : () => setRelayOpen(true)} />
      </div>
      {relayOpen && (
        <RelayForm
          explain={explain}
          onSaved={() => {
            setRelayOpen(false);
            onRecheck();
          }}
        />
      )}
      <div className="flex flex-col gap-3">
        <SubHeading icon={Mail}>{t("setup.sending.inTitle")}</SubHeading>
        <CheckLines lines={receivingLines(check)} explain={explain} />
        {!check.upstream && (
          <p className="flex gap-2 rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
            <Info className="mt-0.5 size-4 shrink-0" aria-hidden />
            {t("setup.sending.honest")}
          </p>
        )}
      </div>
    </div>
  );
}

/** The relay part of the sending settings. */
export function RelayForm({ explain, onSaved }: { explain: boolean; onSaved: () => void }) {
  const { t } = useT();
  const query = useQuery({ queryKey: ["admin", "settings"], queryFn: () => api<SettingsView>("/api/admin/settings") });
  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  return (
    <Section
      title={t("setup.sending.relayTitle")}
      intro={explain ? t("setup.sending.relayBody") : t("settings.delivery.relayHint")}
      view={query.data}
      keys={[
        "delivery.relay.host",
        "delivery.relay.port",
        "delivery.relay.security",
        "delivery.relay.username",
        "delivery.relay.password",
      ]}
      onSaved={onSaved}
    >
      {(form) => <DeliveryFields form={form} pro={!explain} relayOnly />}
    </Section>
  );
}

/** Reverse DNS always, blocklists only when asked for. */
export function AddressChecks({
  check,
  explain,
  busy,
  onBlocklists,
}: {
  check: ServerCheck;
  explain: boolean;
  busy: boolean;
  onBlocklists: () => void;
}) {
  const { t } = useT();
  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-col gap-3">
        <SubHeading icon={Info}>{t("setup.checks.ptrTitle")}</SubHeading>
        <CheckLines lines={ptrLines(check)} explain={explain} />
      </div>
      <div className="flex flex-col gap-3">
        <SubHeading icon={ShieldCheck}>{t("setup.checks.blocklistsTitle")}</SubHeading>
        {explain && <p className="text-[13px] text-muted">{t("setup.checks.blocklistsBody")}</p>}
        <CheckLines lines={blocklistLines(check)} explain={explain} />
        <Button size="sm" busy={busy} className="self-start" onClick={onBlocklists}>
          {check.blocklistsChecked ? t("setup.checks.blocklistsAgain") : t("setup.checks.blocklistsRun")}
        </Button>
      </div>
    </div>
  );
}

function Waiting({ children }: { children: ReactNode }) {
  return (
    <p className="flex items-center gap-2.5 text-sm text-muted">
      <span
        className="size-4 shrink-0 animate-spin rounded-full border-2 border-current border-t-transparent"
        aria-hidden
      />
      {children}
    </p>
  );
}

function Result({ ok, children }: { ok: boolean; children: ReactNode }) {
  const { icon: Icon, className } = LEVELS[ok ? "ok" : "warning"];
  return (
    <p className="flex gap-2.5 text-sm">
      <Icon className={clsx("mt-0.5 size-[18px] shrink-0", className)} aria-hidden />
      {children}
    </p>
  );
}

/** The own test mail arrives within seconds; after a minute something is off. */
function useSlow(sent: TestMailSent | null, arrived: boolean) {
  const [slowId, setSlowId] = useState<string | null>(null);
  useEffect(() => {
    if (!sent || arrived) return;
    const timer = window.setTimeout(() => setSlowId(sent.messageId), 60_000);
    return () => window.clearTimeout(timer);
  }, [sent, arrived]);
  return Boolean(sent && !arrived && slowId === sent.messageId);
}

/** A test mail to the admin's own mailbox, and optionally to an outside address to reply from. */
export function TestMailPanel({ login, explain }: { login: string; explain: boolean }) {
  const { t } = useT();
  const sendOwn = useSendTestMail();
  const sendExternal = useSendTestMail();
  const own = useTestMailStatus(sendOwn.data ?? null);
  const external = useTestMailStatus(sendExternal.data ?? null);
  const [address, setAddress] = useState("");
  const ownArrived = Boolean(own.data?.arrived);
  const slow = useSlow(sendOwn.data ?? null, ownArrived);

  const submitExternal = (event: FormEvent) => {
    event.preventDefault();
    if (address.trim()) sendExternal.mutate(address.trim());
  };

  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-col gap-3">
        <SubHeading icon={Mail}>{t("setup.testMail.ownTo", { login })}</SubHeading>
        {sendOwn.data &&
          (ownArrived ? (
            <Result ok>{t("setup.testMail.arrived")}</Result>
          ) : slow ? (
            <Result ok={false}>
              {t("setup.testMail.slow")}{" "}
              <Link to="/admin/queue" className="font-semibold text-pink-ink underline-offset-2 hover:underline">
                {t("nav.queue")}
              </Link>
            </Result>
          ) : (
            <Waiting>{t("setup.testMail.waiting")}</Waiting>
          ))}
        <Button
          variant={sendOwn.data ? "secondary" : "primary"}
          icon={Send}
          busy={sendOwn.isPending}
          className="self-start"
          onClick={() => sendOwn.mutate(null)}
        >
          {sendOwn.data ? t("setup.testMail.again") : t("setup.testMail.own")}
        </Button>
      </div>

      <form className="flex flex-col gap-3" onSubmit={submitExternal}>
        <SubHeading icon={Send}>{t("setup.testMail.externalTitle")}</SubHeading>
        {explain && <p className="text-[13px] text-muted">{t("setup.testMail.externalBody")}</p>}
        <div className="flex flex-col gap-2 sm:flex-row sm:items-end">
          <Field label={t("setup.testMail.external")} className="flex-1">
            {(id) => (
              <TextInput
                id={id}
                type="email"
                autoComplete="email"
                placeholder="name@example.net"
                value={address}
                onChange={(event) => setAddress(event.target.value)}
              />
            )}
          </Field>
          <Button type="submit" icon={Send} busy={sendExternal.isPending} disabled={!address.trim()}>
            {t("setup.testMail.externalSend")}
          </Button>
        </div>
        {sendExternal.data &&
          (external.data?.replyFrom ? (
            <Result ok>{t("setup.testMail.replied", { address: external.data.replyFrom })}</Result>
          ) : (
            <Waiting>{t("setup.testMail.externalWaiting", { address: sendExternal.data.external ?? "" })}</Waiting>
          ))}
      </form>
    </div>
  );
}

/** Kinds of records that can be replaced when they hold another value. */
const KINDS = ["mx", "spf", "dmarc", "dkim", "tlsrpt", "mtasts", "jmap", "imaps", "submissions", "submission"] as const;

/** Rewriting these can cut off other senders or another mail server, so they get a warning. */
const DELICATE = ["mx", "spf"];

/** A list of record kinds to tick off. */
function KindChoices({
  kinds,
  chosen,
  onChange,
}: {
  kinds: readonly string[];
  chosen: string[];
  onChange: (kinds: string[]) => void;
}) {
  const { t } = useT();
  return kinds.map((kind) => (
    <label key={kind} className="flex items-center gap-2 text-sm">
      <input
        type="checkbox"
        className="size-4 accent-pink"
        checked={chosen.includes(kind)}
        onChange={(event) =>
          onChange(event.target.checked ? [...chosen, kind] : chosen.filter((entry) => entry !== kind))
        }
      />
      <span className="font-semibold">{t(`domains.detail.kinds.${kind}`)}</span>
    </label>
  ));
}

/** Puts missing records into Cloudflare with a token that is used once. */
export function CloudflarePanel({
  domain,
  report,
  explain,
}: {
  domain: string;
  report: DomainReport;
  explain: boolean;
}) {
  const { t } = useT();
  const cloudflare = useCloudflare(domain);
  const [open, setOpen] = useState(false);
  const [token, setToken] = useState("");
  const [replace, setReplace] = useState<string[]>([]);
  const [tidy, setTidy] = useState<string[]>([]);
  // The MTA-STS policy is a file this server serves, not a DNS record.
  const missing = report.records.filter(
    (record) => record.status === "missing" && record.keyState !== "pending" && record.recordType !== "HTTPS",
  );
  const wrongKinds = KINDS.filter((kind) =>
    report.records.some((record) => record.kind === kind && record.status === "wrong"),
  );
  // Published, working, only written differently than UwUMail would write it.
  const differingKinds = KINDS.filter((kind) =>
    report.records.some((record) => record.kind === kind && record.differs),
  );
  const results = cloudflare.data?.results;
  const nothingToDo = missing.length === 0 && wrongKinds.length === 0 && differingKinds.length === 0;

  const submit = (event: FormEvent) => {
    event.preventDefault();
    cloudflare.mutate(
      { token, replace, tidy },
      {
        // The token is only needed for this one request.
        onSettled: () => setToken(""),
      },
    );
  };

  if (!open) {
    return (
      <Button icon={Cloud} className="self-start" onClick={() => setOpen(true)}>
        {t("setup.cloudflare.open")}
      </Button>
    );
  }

  return (
    <form className="flex flex-col gap-3 rounded-card border border-hairline bg-canvas p-4" onSubmit={submit}>
      <SubHeading icon={Cloud}>{t("setup.cloudflare.title")}</SubHeading>
      <p className="text-[13px] text-muted">
        {nothingToDo && !results ? t("setup.cloudflare.quotesOnly") : t("setup.cloudflare.body")}
      </p>
      <Field label={t("setup.cloudflare.token")} hint={explain ? t("setup.cloudflare.tokenHint") : undefined}>
        {(id) => (
          <TextInput
            id={id}
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={token}
            onChange={(event) => setToken(event.target.value)}
          />
        )}
      </Field>
      {wrongKinds.length > 0 && (
        <fieldset className="flex flex-col gap-1.5">
          <legend className="text-[13px] font-semibold text-muted">{t("setup.cloudflare.replace")}</legend>
          <KindChoices kinds={wrongKinds} chosen={replace} onChange={setReplace} />
          <p className="text-[12px] text-muted">{t("setup.cloudflare.replaceHint")}</p>
        </fieldset>
      )}
      {differingKinds.length > 0 && (
        <fieldset className="flex flex-col gap-1.5">
          <legend className="text-[13px] font-semibold text-muted">{t("setup.cloudflare.tidy")}</legend>
          <KindChoices kinds={differingKinds} chosen={tidy} onChange={setTidy} />
          <p className="text-[12px] text-muted">{t("setup.cloudflare.tidyHint")}</p>
          {differingKinds.some((kind) => DELICATE.includes(kind)) && (
            <p className="rounded-control bg-warning-tint px-3 py-2 text-[12px] text-warning">
              {t("setup.cloudflare.tidyWarning")}
            </p>
          )}
        </fieldset>
      )}
      <div className="flex flex-wrap gap-2">
        <Button type="submit" variant="primary" icon={Cloud} busy={cloudflare.isPending} disabled={!token.trim()}>
          {t("setup.cloudflare.submit")}
        </Button>
        <Button variant="ghost" onClick={() => setOpen(false)}>
          {t("common.cancel")}
        </Button>
      </div>
      {results && (
        <ul className="flex flex-col gap-1.5">
          {results.length === 0 && <li className="text-[13px] text-muted">{t("setup.cloudflare.nothing")}</li>}
          {results.map((result) => {
            const level: LineLevel =
              result.outcome === "failed" ? "problem" : result.outcome === "skipped" ? "unknown" : "ok";
            const { icon: Icon, className } = LEVELS[level];
            return (
              <li key={`${result.recordType}-${result.name}`} className="flex gap-2 text-[13px]">
                <Icon className={clsx("mt-0.5 size-4 shrink-0", className)} aria-hidden />
                <span className="min-w-0 break-words">
                  <span className="font-semibold">{result.recordType}</span> {result.name}:{" "}
                  {t(`setup.cloudflare.outcome.${result.outcome}`)}
                  {result.error && <span className="text-muted"> ({result.error})</span>}
                </span>
              </li>
            );
          })}
        </ul>
      )}
    </form>
  );
}

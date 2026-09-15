import clsx from "clsx";
import { Lightbulb, ShieldCheck } from "lucide-react";
import { useState, type ReactNode } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card, CopyButton } from "@/components/ui/Card";
import { Segmented } from "@/components/ui/Field";
import { useT } from "@/i18n";
import type { DomainDetail, MtaStsMode } from "@/lib/api";
import { formatDate, formatNumber } from "@/lib/format";
import { usePrefs } from "@/state/prefs";
import { toast } from "@/state/toasts";
import { useDomainReports, useSetMtaSts } from "./queries";

type ModeChoice = MtaStsMode | "off";

function Suggestion({ children, action }: { children: ReactNode; action?: ReactNode }) {
  return (
    <div className="flex gap-2.5 rounded-control bg-pink-tint/60 p-3 text-[13px] text-pink-ink">
      <Lightbulb className="mt-0.5 size-4 shrink-0" aria-hidden />
      <div className="flex min-w-0 flex-1 flex-col items-start gap-2">
        <div className="min-w-0 self-stretch">{children}</div>
        {action}
      </div>
    </div>
  );
}

/** Switching MTA-STS for a domain: off, testing, enforce. */
export function MtaStsCard({ domain }: { domain: DomainDetail }) {
  const { t, i18n } = useT();
  const pro = usePrefs((s) => s.mode) === "pro";
  const set = useSetMtaSts(domain.name);
  const reports = useDomainReports(domain.name, 30);
  const settings = domain.mtaSts;
  const mode: ModeChoice = settings?.mode ?? "off";
  const ready =
    mode === "testing" && reports.data?.suggestions.some((suggestion) => suggestion.code === "mtaStsEnforce");

  const change = (next: ModeChoice) => {
    if (next === mode) return;
    if (next === "enforce" && !pro && !window.confirm(t("domains.mtaSts.enforceConfirm"))) return;
    set.mutate(next, { onSuccess: () => toast(t(`domains.mtaSts.toasts.${next}`), "success") });
  };

  return (
    <Card title={t("domains.mtaSts.title")}>
      <div className="flex flex-col gap-3">
        {!pro && <p className="text-[13px] text-muted">{t("domains.mtaSts.intro")}</p>}
        <Segmented<ModeChoice>
          label={t("domains.mtaSts.title")}
          value={mode}
          onChange={change}
          options={[
            { value: "off", label: t("domains.mtaSts.modes.off") },
            { value: "testing", label: t("domains.mtaSts.modes.testing") },
            { value: "enforce", label: t("domains.mtaSts.modes.enforce") },
          ]}
        />
        <p className="text-[13px] text-muted">{t(`domains.mtaSts.explain.${mode}`)}</p>
        {settings && (
          <>
            <p className="text-[13px]">
              {t("domains.mtaSts.mx", { names: settings.mx.join(", ") })}
              <span className="text-faint">
                {" · "}
                {t("domains.mtaSts.since", { date: formatDate(settings.changedAt, i18n.language) })}
              </span>
            </p>
            {!pro && <p className="text-[13px] text-muted">{t("domains.mtaSts.records")}</p>}
            {pro && (
              <div className="flex items-start gap-1 rounded-control bg-canvas px-2.5 py-1.5">
                <code className="min-w-0 flex-1 text-[12px] whitespace-pre-wrap">{settings.policy.trimEnd()}</code>
                <CopyButton value={settings.policy} />
              </div>
            )}
          </>
        )}
        {ready && (
          <Suggestion
            action={
              <Button size="sm" variant="primary" busy={set.isPending} onClick={() => change("enforce")}>
                {t("domains.mtaSts.enforceNow")}
              </Button>
            }
          >
            {t("domains.mtaSts.ready")}
          </Suggestion>
        )}
      </div>
    </Card>
  );
}

const RESULT_TYPES = [
  "starttls-not-supported",
  "certificate-host-mismatch",
  "certificate-expired",
  "certificate-not-trusted",
  "validation-failure",
  "sts-policy-fetch-error",
  "sts-policy-invalid",
  "sts-webpki-invalid",
];

/** A number, or with `text` a short list of names. */
function Stat({ label, value, tone, text }: { label: string; value: string; tone?: "good" | "bad"; text?: boolean }) {
  return (
    <div className="rounded-control bg-canvas px-3 py-2">
      <p className="text-[12px] text-muted">{label}</p>
      <p
        className={clsx(
          text ? "text-[13px] font-semibold break-words" : "text-[17px] font-bold tabular-nums",
          tone === "good" && "text-success",
          tone === "bad" && "text-danger",
        )}
      >
        {value}
      </p>
    </div>
  );
}

function percent(part: number, whole: number, language: string) {
  if (whole === 0) return "–";
  return new Intl.NumberFormat(language, { style: "percent", maximumFractionDigits: 1 }).format(part / whole);
}

/** What other servers reported: DMARC results of mail in the domain's name, and TLS to our MX. */
export function ReportsCard({ domain }: { domain: DomainDetail }) {
  const { t, i18n } = useT();
  const language = i18n.language;
  const pro = usePrefs((s) => s.mode) === "pro";
  const [days, setDays] = useState(30);
  const query = useDomainReports(domain.name, days);

  const body = () => {
    if (query.isPending) return <Loading />;
    if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
    const { dmarc, tls, suggestions } = query.data;
    if (dmarc.reports === 0 && tls.reports === 0) {
      return (
        <div className="flex flex-col gap-2 text-[13px] text-muted">
          <p>{t("domains.reports.empty", { days })}</p>
          {!pro && <p>{t("domains.reports.emptyHint", { domain: domain.name })}</p>}
        </div>
      );
    }
    const stricter = suggestions.find((suggestion) => suggestion.code === "dmarcStricter");
    const ownFailed = dmarc.sources.filter((source) => source.ours && source.passed < source.messages);
    const record =
      stricter && `v=DMARC1; p=${stricter.params.to}; adkim=s; aspf=s; rua=mailto:dmarc-reports@${domain.name}`;
    return (
      <div className="flex flex-col gap-6">
        <section className="flex flex-col gap-3">
          <h3 className="text-sm font-bold">{t("domains.reports.dmarcTitle")}</h3>
          {!pro && <p className="-mt-2 text-[13px] text-muted">{t("domains.reports.dmarcIntro")}</p>}
          {dmarc.reports === 0 ? (
            <p className="text-[13px] text-muted">{t("domains.reports.noneOfKind")}</p>
          ) : (
            <>
              <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
                <Stat label={t("domains.reports.messages")} value={formatNumber(dmarc.messages, language)} />
                <Stat
                  label={t("domains.reports.passed")}
                  value={percent(dmarc.passed, dmarc.messages, language)}
                  tone={dmarc.passed === dmarc.messages ? "good" : undefined}
                />
                <Stat label={t("domains.reports.reports")} value={formatNumber(dmarc.reports, language)} />
                <Stat
                  text
                  label={t("domains.reports.reporters")}
                  value={
                    dmarc.reporters
                      .map((reporter) => reporter.organization)
                      .slice(0, 3)
                      .join(", ") || "–"
                  }
                />
              </div>
              {ownFailed.length > 0 && (
                <p className="rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning">
                  {t("domains.reports.ownFailing", { count: ownFailed.length })}
                </p>
              )}
              {stricter && record && (
                <Suggestion action={<CopyButton value={record} label={t("domains.detail.copyValue")} />}>
                  <p>{t("domains.reports.stricter", { from: stricter.params.from, to: stricter.params.to })}</p>
                  <code className="mt-1 block text-[12px] break-all">{record}</code>
                </Suggestion>
              )}
              <div className="overflow-x-auto">
                <table className="w-full min-w-[460px] text-left text-[13px]">
                  <thead className="text-[12px] text-muted">
                    <tr>
                      <th className="py-1.5 pr-3 font-semibold">{t("domains.reports.source")}</th>
                      <th className="py-1.5 pr-3 text-right font-semibold">{t("domains.reports.messages")}</th>
                      <th className="py-1.5 text-right font-semibold">{t("domains.reports.passed")}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {dmarc.sources.slice(0, pro ? 50 : 10).map((source) => {
                      const failed = source.messages - source.passed;
                      return (
                        <tr key={source.ip} className="border-t border-hairline">
                          <td className="py-1.5 pr-3">
                            <span className="font-mono text-[12px]">{source.ip || "?"}</span>
                            {source.ours && (
                              <span className="ml-2 inline-flex items-center gap-1 rounded-full bg-pink-tint px-2 text-[11px] font-semibold text-pink-ink">
                                <ShieldCheck className="size-3" aria-hidden />
                                {t("domains.reports.ours")}
                              </span>
                            )}
                            {pro && source.headerFrom.length > 0 && (
                              <span className="block text-[11px] text-faint">{source.headerFrom.join(", ")}</span>
                            )}
                          </td>
                          <td className="py-1.5 pr-3 text-right tabular-nums">
                            {formatNumber(source.messages, language)}
                          </td>
                          <td
                            className={clsx(
                              "py-1.5 text-right tabular-nums",
                              failed === 0 ? "text-success" : source.ours ? "text-danger" : "text-muted",
                            )}
                          >
                            {percent(source.passed, source.messages, language)}
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
              {!pro && <p className="text-[12px] text-faint">{t("domains.reports.strangers")}</p>}
            </>
          )}
        </section>

        <section className="flex flex-col gap-3">
          <h3 className="text-sm font-bold">{t("domains.reports.tlsTitle")}</h3>
          {!pro && <p className="-mt-2 text-[13px] text-muted">{t("domains.reports.tlsIntro")}</p>}
          {tls.reports === 0 ? (
            <p className="text-[13px] text-muted">{t("domains.reports.noneOfKind")}</p>
          ) : (
            <>
              <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
                <Stat
                  label={t("domains.reports.sessionsOk")}
                  value={formatNumber(tls.successful, language)}
                  tone="good"
                />
                <Stat
                  label={t("domains.reports.sessionsFailed")}
                  value={formatNumber(tls.failed, language)}
                  tone={tls.failed > 0 ? "bad" : undefined}
                />
                <Stat label={t("domains.reports.reports")} value={formatNumber(tls.reports, language)} />
                <Stat
                  text
                  label={t("domains.reports.reporters")}
                  value={
                    tls.reporters
                      .map((reporter) => reporter.organization)
                      .slice(0, 3)
                      .join(", ") || "–"
                  }
                />
              </div>
              {tls.failures.length > 0 && (
                <ul className="flex flex-col gap-1.5">
                  {tls.failures.map((failure) => (
                    <li
                      key={`${failure.resultType}-${failure.mxHost}-${failure.policyType}`}
                      className="flex flex-wrap items-baseline gap-x-2 text-[13px]"
                    >
                      <span className="font-semibold">
                        {RESULT_TYPES.includes(failure.resultType)
                          ? t(`domains.reports.results.${failure.resultType}`)
                          : failure.resultType}
                      </span>
                      {failure.mxHost && <span className="text-muted">{failure.mxHost}</span>}
                      <span className="ml-auto text-muted tabular-nums">
                        {t("domains.reports.sessions", { count: failure.sessions })}
                      </span>
                    </li>
                  ))}
                </ul>
              )}
            </>
          )}
        </section>
        {(dmarc.unauthenticated > 0 || tls.unauthenticated > 0) && (
          <p className="text-[12px] text-faint">
            {t("domains.reports.unauthenticated", { count: dmarc.unauthenticated + tls.unauthenticated })}
          </p>
        )}
      </div>
    );
  };

  return (
    <Card
      title={t("domains.reports.title")}
      action={
        <Segmented<string>
          label={t("domains.reports.period")}
          value={String(days)}
          onChange={(value) => setDays(Number(value))}
          options={[7, 30, 180].map((value) => ({
            value: String(value),
            label: t("domains.reports.days", { count: value }),
          }))}
        />
      }
    >
      {body()}
    </Card>
  );
}

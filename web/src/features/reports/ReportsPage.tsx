import clsx from "clsx";
import { useState } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Card, PageHeader } from "@/components/ui/Card";
import { EmptyState } from "@/components/ui/EmptyState";
import { Segmented } from "@/components/ui/Field";
import { useT } from "@/i18n";
import type { DomainReports } from "@/lib/api";
import { formatNumber } from "@/lib/format";
import { Link } from "@/lib/router";
import { usePrefs } from "@/state/prefs";
import { useReports } from "./queries";
import { ReportList } from "./ReportList";

const PERIODS = [7, 30, 90, 180];

function share(part: number, whole: number, language: string) {
  if (whole === 0) return "–";
  return new Intl.NumberFormat(language, { style: "percent", maximumFractionDigits: 1 }).format(part / whole);
}

function Number_({ label, value, tone }: { label: string; value: string; tone?: "good" | "bad" }) {
  return (
    <div className="rounded-control bg-canvas px-3 py-2">
      <p className="text-[12px] text-muted">{label}</p>
      <p
        className={clsx(
          "text-[17px] font-bold tabular-nums",
          tone === "good" && "text-success",
          tone === "bad" && "text-danger",
        )}
      >
        {value}
      </p>
    </div>
  );
}

function DomainCard({ domain, days }: { domain: DomainReports; days: number }) {
  const { t, i18n } = useT();
  const language = i18n.language;
  const [showing, setShowing] = useState(false);
  const { dmarc, tls } = domain;
  const nothing = dmarc.reports === 0 && tls.reports === 0;
  const dmarcFailed = dmarc.messages - dmarc.passed;
  const notRead = [!domain.reading.dmarc && "dmarc-reports", !domain.reading.tls && "tls-reports"].filter(Boolean);

  return (
    <Card
      title={
        <Link to={`/admin/domains/${encodeURIComponent(domain.name)}`} className="hover:underline">
          {domain.name}
        </Link>
      }
    >
      <div className="flex flex-col gap-3">
        {notRead.length > 0 && (
          <p className="rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning">
            {t("reports.addressTaken", { addresses: notRead.map((local) => `${local}@${domain.name}`).join(", ") })}
          </p>
        )}
        {nothing ? (
          <p className="text-[13px] text-muted">{t("reports.none", { days })}</p>
        ) : (
          <>
            <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
              <Number_ label={t("reports.messages")} value={formatNumber(dmarc.messages, language)} />
              <Number_
                label={t("reports.passed")}
                value={share(dmarc.passed, dmarc.messages, language)}
                tone={dmarc.messages > 0 && dmarc.passed === dmarc.messages ? "good" : undefined}
              />
              <Number_ label={t("reports.tlsSessions")} value={formatNumber(tls.successful + tls.failed, language)} />
              <Number_
                label={t("reports.tlsFailed")}
                value={formatNumber(tls.failed, language)}
                tone={tls.failed > 0 ? "bad" : "good"}
              />
            </div>
            {dmarcFailed > 0 && (
              <p className="text-[13px] text-muted">
                {t("reports.someFailed", { count: dmarcFailed, sources: dmarc.sources.length })}
              </p>
            )}
            {domain.ownFailing > 0 && (
              <p className="rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning">
                {t("reports.ownFailing", { count: domain.ownFailing })}
              </p>
            )}
            <p className="text-[13px] text-muted">
              {t("reports.reporters", {
                names: dmarc.reporters
                  .concat(tls.reporters)
                  .map((reporter) => reporter.organization)
                  .filter((name, index, all) => all.indexOf(name) === index)
                  .slice(0, 4)
                  .join(", "),
              })}
            </p>
          </>
        )}
        {!nothing &&
          (showing ? (
            <ReportList domain={domain.name} />
          ) : (
            <button
              type="button"
              onClick={() => setShowing(true)}
              className="self-start text-[13px] font-semibold text-pink-ink hover:underline"
            >
              {t("reports.showSingle")}
            </button>
          ))}
        <Link
          to={`/admin/domains/${encodeURIComponent(domain.name)}`}
          className="text-[13px] font-semibold text-pink-ink hover:underline"
        >
          {t("reports.toDomain")}
        </Link>
      </div>
    </Card>
  );
}

/** What other mail servers report about our domains: DMARC results and TLS to our MX. */
export function ReportsPage() {
  const { t } = useT();
  const pro = usePrefs((s) => s.mode) === "pro";
  const [days, setDays] = useState(30);
  const query = useReports(days);

  return (
    <div className="flex flex-col gap-5">
      <PageHeader title={t("reports.title")} intro={t("reports.intro")} />
      {!pro && <p className="text-[13px] text-muted">{t("reports.explain")}</p>}
      <Segmented<string>
        label={t("reports.period")}
        value={String(days)}
        onChange={(value) => setDays(Number(value))}
        options={PERIODS.map((value) => ({ value: String(value), label: t("reports.days", { count: value }) }))}
      />
      {query.isPending ? (
        <Loading />
      ) : query.isError ? (
        <LoadError error={query.error} onRetry={() => void query.refetch()} />
      ) : query.data.domains.length === 0 ? (
        <EmptyState scene="search" title={t("reports.noDomains.title")} body={t("reports.noDomains.body")} />
      ) : (
        query.data.domains.map((domain) => <DomainCard key={domain.name} domain={domain} days={days} />)
      )}
    </div>
  );
}

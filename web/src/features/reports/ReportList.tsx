import clsx from "clsx";
import { useState } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Dialog } from "@/components/ui/Dialog";
import { Segmented } from "@/components/ui/Field";
import { useT } from "@/i18n";
import type { DmarcReportRow, ReportDetail, ReportKind, TlsReportFailure } from "@/lib/api";
import { formatDate, formatDateTime, formatNumber } from "@/lib/format";
import { useReportDetail, useReportList } from "./queries";

/** A value the report did not carry, so nothing is invented for it. */
function Said({ value }: { value: string | null }) {
  const { t } = useT();
  return value ? <>{value}</> : <span className="text-faint">{t("reports.notSaid")}</span>;
}

function Line({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-wrap gap-x-2 border-b border-hairline py-1.5 last:border-b-0">
      <span className="min-w-36 text-[12px] text-muted">{label}</span>
      <span className="min-w-0 flex-1 text-[13px] break-words">{children}</span>
    </div>
  );
}

function DmarcRowCard({ row }: { row: DmarcReportRow }) {
  const { t, i18n } = useT();
  const aligned = row.dkimAligned || row.spfAligned;
  return (
    <div className="rounded-control bg-canvas px-3 py-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-mono text-[12px]">{row.sourceIp || "?"}</span>
        {row.ours && (
          <span className="rounded-full bg-pink-tint px-2 text-[11px] font-semibold text-pink-ink">
            {t("reports.ours")}
          </span>
        )}
        <span className={clsx("text-[12px] font-semibold", aligned ? "text-success" : "text-danger")}>
          {aligned ? t("reports.genuine") : t("reports.notGenuine")}
        </span>
        <span className="ml-auto text-[13px] font-bold tabular-nums">{formatNumber(row.messages, i18n.language)}</span>
      </div>
      <div className="mt-1">
        <Line label={t("reports.dkim")}>
          {row.dkimResult ? (
            <>
              {row.dkimResult}
              {row.dkimDomain && ` · ${row.dkimDomain}`}
              {row.dkimSelector && ` · ${row.dkimSelector}`}
            </>
          ) : (
            <Said value={null} />
          )}
        </Line>
        <Line label={t("reports.spf")}>
          {row.spfResult ? (
            <>
              {row.spfResult}
              {row.spfDomain && ` · ${row.spfDomain}`}
            </>
          ) : (
            <Said value={null} />
          )}
        </Line>
        <Line label={t("reports.headerFrom")}>{row.headerFrom || <Said value={null} />}</Line>
        {row.envelopeFrom && <Line label={t("reports.envelopeFrom")}>{row.envelopeFrom}</Line>}
        <Line label={t("reports.disposition")}>{t(`reports.dispositions.${row.disposition}`)}</Line>
        {row.overrideReason && <Line label={t("reports.override")}>{row.overrideReason}</Line>}
      </div>
    </div>
  );
}

function TlsFailureCard({ failure }: { failure: TlsReportFailure }) {
  const { t, i18n } = useT();
  return (
    <div className="rounded-control bg-canvas px-3 py-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-[13px] font-semibold text-danger">{failure.resultType}</span>
        <span className="ml-auto text-[13px] font-bold tabular-nums">
          {formatNumber(failure.sessions, i18n.language)}
        </span>
      </div>
      <div className="mt-1">
        <Line label={t("reports.mxHost")}>{failure.mxHost || <Said value={null} />}</Line>
        <Line label={t("reports.sendingIp")}>{failure.sendingIp || <Said value={null} />}</Line>
        {failure.receivingIp && <Line label={t("reports.receivingIp")}>{failure.receivingIp}</Line>}
        {failure.helo && <Line label={t("reports.helo")}>{failure.helo}</Line>}
        {failure.failureCode && <Line label={t("reports.failureCode")}>{failure.failureCode}</Line>}
        {failure.detail && <Line label={t("reports.detail")}>{failure.detail}</Line>}
      </div>
    </div>
  );
}

function Detail({ detail }: { detail: ReportDetail }) {
  const { t, i18n } = useT();
  const { report } = detail;
  return (
    <div className="flex flex-col gap-4 px-6 pb-6">
      <div>
        <Line label={t("reports.reporter")}>{report.organization}</Line>
        <Line label={t("reports.period")}>
          {formatDate(report.beginAt, i18n.language)} – {formatDate(report.endAt, i18n.language)}
        </Line>
        <Line label={t("reports.arrived")}>{formatDateTime(report.receivedAt, i18n.language)}</Line>
        <Line label={t("reports.reportId")}>
          <span className="font-mono text-[12px] break-all">{report.reportId}</span>
        </Line>
        {report.about && <Line label={t("reports.about")}>{report.about}</Line>}
        {report.policy && <Line label={t("reports.policy")}>{report.policy}</Line>}
        {!report.authenticated && (
          <Line label={t("reports.sender")}>
            <span className="text-warning">{t("reports.unauthenticated")}</span>
          </Line>
        )}
      </div>
      {detail.kind === "dmarc" ? (
        <div className="flex flex-col gap-2">
          {detail.rows.length === 0 && <p className="text-[13px] text-muted">{t("reports.noRows")}</p>}
          {detail.rows.map((row, index) => (
            <DmarcRowCard key={`${row.sourceIp}-${index}`} row={row} />
          ))}
        </div>
      ) : (
        <div className="flex flex-col gap-2">
          {detail.failures.length === 0 && <p className="text-[13px] text-muted">{t("reports.noFailures")}</p>}
          {detail.failures.map((failure, index) => (
            <TlsFailureCard key={`${failure.resultType}-${index}`} failure={failure} />
          ))}
          {detail.policy && (
            <div>
              <p className="mt-2 text-[12px] text-muted">{t("reports.policyApplied")}</p>
              <pre className="mt-1 overflow-x-auto rounded-control bg-canvas px-3 py-2 font-mono text-[12px] whitespace-pre-wrap">
                {detail.policy}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/** The reports of one domain, one of which can be opened and read. */
export function ReportList({ domain }: { domain: string }) {
  const { t, i18n } = useT();
  const [kind, setKind] = useState<ReportKind>("dmarc");
  const [open, setOpen] = useState<number | null>(null);
  const list = useReportList(domain, kind, true);
  const detail = useReportDetail(domain, kind, open);

  return (
    <div className="flex flex-col gap-3 border-t border-hairline pt-3">
      <Segmented<ReportKind>
        label={t("reports.kind")}
        value={kind}
        onChange={(value) => {
          setKind(value);
          setOpen(null);
        }}
        options={[
          { value: "dmarc", label: t("reports.dmarc") },
          { value: "tls", label: t("reports.tls") },
        ]}
      />
      {list.isPending ? (
        <Loading />
      ) : list.isError ? (
        <LoadError error={list.error} onRetry={() => void list.refetch()} />
      ) : list.data.reports.length === 0 ? (
        <p className="text-[13px] text-muted">{t("reports.noneOfKind")}</p>
      ) : (
        <ul className="flex flex-col">
          {list.data.reports.map((entry) => (
            <li key={entry.id}>
              <button
                type="button"
                onClick={() => setOpen(entry.id)}
                className="flex w-full flex-wrap items-center gap-x-3 gap-y-1 rounded-control border-b border-hairline px-2 py-2 text-left hover:bg-pink-tint/40"
              >
                <span className="min-w-0 flex-1 text-[13px] font-semibold">{entry.organization}</span>
                <span className="text-[12px] text-muted">{formatDate(entry.endAt, i18n.language)}</span>
                <span className="text-[12px] tabular-nums">
                  {formatNumber(entry.good, i18n.language)}
                  {entry.bad > 0 && <span className="text-danger"> · {formatNumber(entry.bad, i18n.language)}</span>}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
      <Dialog open={open !== null} onClose={() => setOpen(null)} title={t("reports.oneReport")}>
        {detail.isPending ? (
          <div className="px-6 pb-6">
            <Loading />
          </div>
        ) : detail.isError ? (
          <div className="px-6 pb-6">
            <LoadError error={detail.error} onRetry={() => void detail.refetch()} />
          </div>
        ) : (
          <Detail detail={detail.data} />
        )}
      </Dialog>
    </div>
  );
}

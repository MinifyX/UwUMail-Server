import { useQuery } from "@tanstack/react-query";
import clsx from "clsx";
import { LoadError, Loading } from "@/components/StatusViews";
import { Card } from "@/components/ui/Card";
import { Section, ToggleField } from "@/features/settings/SettingsPage";
import { useT } from "@/i18n";
import { api, type SentTlsReport, type SettingsView } from "@/lib/api";
import { formatDate, formatNumber } from "@/lib/format";
import { useSentReports } from "./queries";

const STATUS_CLASS: Record<SentTlsReport["status"], string> = {
  sent: "bg-success-tint text-success",
  failed: "bg-danger-tint text-danger",
  none: "bg-elevated text-muted",
  skipped: "bg-elevated text-muted",
};

function SentRow({ report }: { report: SentTlsReport }) {
  const { t, i18n } = useT();
  const language = i18n.language;
  return (
    <li className="flex flex-col gap-1 border-b border-hairline py-3 first:pt-0 last:border-b-0 last:pb-0">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-sm font-bold break-all">{report.domain}</span>
        <span
          className={clsx(
            "inline-flex h-6 items-center rounded-full px-2.5 text-[12px] font-semibold",
            STATUS_CLASS[report.status],
          )}
        >
          {t(`reports.sent.status.${report.status}`)}
        </span>
        {/* The day is a UTC day; its noon falls on the same date everywhere. */}
        <span className="ml-auto text-[12px] text-muted">{formatDate(report.day + 12 * 3600, language)}</span>
      </div>
      <p className="text-[13px] text-muted">
        {t("reports.sent.sessions", {
          successful: formatNumber(report.successful, language),
          failed: formatNumber(report.failed, language),
        })}
      </p>
      {report.destinations.length > 0 && (
        <p className="text-[12px] break-all text-muted">
          {t("reports.sent.to", { destinations: report.destinations.join(", ") })}
        </p>
      )}
      {report.error && <p className="text-[12px] break-words text-danger">{report.error}</p>}
    </li>
  );
}

/** The TLS reports this server sends to the domains it delivers to, and the switch for them. */
export function SentReports({ days }: { days: number }) {
  const { t } = useT();
  const settings = useQuery({
    queryKey: ["admin", "settings"],
    queryFn: () => api<SettingsView>("/api/admin/settings"),
  });
  // Reports are kept for 60 days.
  const query = useSentReports(Math.min(days, 60));

  if (settings.isPending || query.isPending) return <Loading />;
  if (settings.isError) return <LoadError error={settings.error} onRetry={() => void settings.refetch()} />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  // Domains that ask for no reports, and our own, are only counted.
  const shown = query.data.reports.filter((report) => report.status === "sent" || report.status === "failed");
  const quiet = query.data.reports.length - shown.length;

  return (
    <>
      <Section
        title={t("reports.sent.title")}
        intro={t("reports.sent.intro", { sender: query.data.sender })}
        view={settings.data}
        keys={["reports.send_tls_reports"]}
      >
        {(form) => (
          <ToggleField
            form={form}
            settingKey="reports.send_tls_reports"
            label={t("reports.sent.toggle")}
            hint={t("reports.sent.toggleHint")}
          />
        )}
      </Section>
      <Card title={t("reports.sent.listTitle", { days: query.data.days })}>
        {shown.length === 0 ? (
          <p className="text-[13px] text-muted">{t("reports.sent.empty")}</p>
        ) : (
          <ul className="flex flex-col">
            {shown.map((report) => (
              <SentRow key={`${report.day}-${report.domain}`} report={report} />
            ))}
          </ul>
        )}
        {quiet > 0 && <p className="mt-3 text-[12px] text-faint">{t("reports.sent.quiet", { count: quiet })}</p>}
      </Card>
    </>
  );
}

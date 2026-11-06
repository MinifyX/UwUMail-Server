import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { LockOpen } from "lucide-react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { EmptyState } from "@/components/ui/EmptyState";
import { useT } from "@/i18n";
import { api, type AbuseReport, type SendingSuspension, type SendingSuspensions } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDuration, formatRelative } from "@/lib/format";
import { toast } from "@/state/toasts";

const SUSPENSIONS_KEY = ["admin", "sending", "suspensions"];
const REPORTS_KEY = ["admin", "abuse-reports"];

function SuspensionRow({ suspension, now }: { suspension: SendingSuspension; now: number }) {
  const { t } = useT();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const lift = useMutation({
    mutationFn: () =>
      api<SendingSuspensions>(`/api/admin/sending/suspensions/${suspension.accountId}/lift`, { method: "POST" }),
    onSuccess: (data) => {
      queryClient.setQueryData(SUSPENSIONS_KEY, data);
      void queryClient.invalidateQueries({ queryKey: ["admin", "alerts"] });
      toast(t("logs.sending.lifted", { login: suspension.login }), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  return (
    <li className="flex flex-wrap items-start justify-between gap-3 rounded-card border border-hairline bg-surface p-4">
      <div className="min-w-0">
        <p className="text-sm font-semibold break-all">{suspension.login}</p>
        <p className="text-sm">{t(`logs.sending.reasons.${suspension.reason}`)}</p>
        <p className="text-[12px] text-muted">
          {t("logs.sending.remaining", { duration: formatDuration(Math.max(0, suspension.until - now), t) })}
        </p>
      </div>
      <Button size="sm" icon={LockOpen} busy={lift.isPending} onClick={() => lift.mutate()}>
        {t("logs.sending.lift")}
      </Button>
    </li>
  );
}

function Suspensions() {
  const { t } = useT();
  const query = useQuery({
    queryKey: SUSPENSIONS_KEY,
    queryFn: () => api<SendingSuspensions>("/api/admin/sending/suspensions"),
    refetchInterval: 15_000,
  });
  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  const { suspensions, now } = query.data;
  return (
    <section className="flex flex-col gap-3" aria-labelledby="sending-suspensions">
      <h2 id="sending-suspensions" className="text-[15px] font-bold">
        {t("logs.sending.suspensionsTitle")}
      </h2>
      <p className="text-[13px] text-muted">{t("logs.sending.suspensionsIntro")}</p>
      {suspensions.length === 0 ? (
        <EmptyState
          compact
          scene="emptyFolder"
          title={t("logs.sending.empty.title")}
          body={t("logs.sending.empty.body")}
        />
      ) : (
        <ul className="flex flex-col gap-3">
          {suspensions.map((suspension) => (
            <SuspensionRow key={suspension.accountId} suspension={suspension} now={now} />
          ))}
        </ul>
      )}
    </section>
  );
}

function ReportRow({ report }: { report: AbuseReport }) {
  const { t, i18n } = useT();
  return (
    <li className="flex flex-col gap-0.5 border-b border-hairline py-3 text-sm last:border-b-0">
      <p className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <span className="font-semibold break-all">{report.reporter}</span>
        <span className="font-mono text-[12px] text-muted">{report.feedbackType}</span>
        <span
          className={
            report.counted
              ? "inline-flex h-6 items-center rounded-full bg-danger-tint px-2.5 text-[12px] font-semibold text-danger"
              : "inline-flex h-6 items-center rounded-full bg-elevated px-2.5 text-[12px] font-semibold text-muted"
          }
        >
          {t(report.counted ? "logs.complaints.counted" : "logs.complaints.notCounted")}
        </span>
      </p>
      <p className="text-[13px] break-words">
        {report.originalSubject || t("logs.complaints.noSubject")}
        <span className="text-muted"> · {report.login || t("logs.complaints.unknownAccount")}</span>
      </p>
      <p className="text-[12px] text-muted">{formatRelative(report.receivedAt, i18n.language)}</p>
    </li>
  );
}

function Complaints() {
  const { t } = useT();
  const query = useQuery({
    queryKey: REPORTS_KEY,
    queryFn: () => api<{ reports: AbuseReport[] }>("/api/admin/abuse-reports"),
    refetchInterval: 60_000,
  });
  return (
    <Card title={t("logs.complaints.title")}>
      <div className="flex flex-col gap-3">
        <p className="text-[13px] text-muted">{t("logs.complaints.intro")}</p>
        {query.isPending ? (
          <Loading />
        ) : query.isError ? (
          <LoadError error={query.error} onRetry={() => void query.refetch()} />
        ) : query.data.reports.length === 0 ? (
          <p className="text-[13px] text-muted">{t("logs.complaints.empty")}</p>
        ) : (
          <ul>
            {query.data.reports.map((report) => (
              <ReportRow key={report.id} report={report} />
            ))}
          </ul>
        )}
      </div>
    </Card>
  );
}

/** Server → Logs → Sending: accounts held back from sending, and complaints about the mail they sent. */
export function SendingPage() {
  return (
    <div className="flex flex-col gap-5">
      <Suspensions />
      <Complaints />
    </div>
  );
}

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import clsx from "clsx";
import { BellRing, Check, ChevronRight, Info } from "lucide-react";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Segmented } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type AdminAlert, type AlertLevel, type AlertsView, type HealthLevel } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatRelative } from "@/lib/format";
import { Link } from "@/lib/router";
import { toast } from "@/state/toasts";
import { useAdminPrefs, useSaveAdminPref, type AlertMails } from "./adminPrefs";
import { LEVELS, useFindingText } from "./HealthCard";

/** Codes the health overview does not know; they have their own texts. */
const OWN_CODES = ["certRenewalFailing", "backupFailed", "backupOld", "updateAvailable"];

const LEVEL_OF: Record<AlertLevel, HealthLevel> = { info: "ok", warning: "warning", problem: "problem" };

export function useAlerts() {
  return useQuery({
    queryKey: ["admin", "alerts"],
    queryFn: () => api<AlertsView>("/api/admin/alerts"),
    refetchInterval: 60_000,
  });
}

/** What an alert is about, and what to do about it, in the viewer's language. */
export function useAlertText() {
  const { t, i18n } = useT();
  const finding = useFindingText();
  return (alert: AdminAlert) => {
    const params = alert.params ?? {};
    const num = (key: string) => (typeof params[key] === "number" ? (params[key] as number) : 0);
    if (OWN_CODES.includes(alert.code)) {
      const text = t(`alerts.codes.${alert.code}`, {
        ...params,
        since: formatRelative(num("since"), i18n.language),
        time: formatRelative(num("lastSuccessAt") || num("at"), i18n.language),
      });
      return { text, hint: t(`alerts.hints.${alert.code}`, { defaultValue: "" }) };
    }
    const text = finding({ code: alert.code, level: LEVEL_OF[alert.level], params, link: alert.link ?? undefined });
    return { text, hint: t(`health.hints.${alert.code}`, { defaultValue: "" }) };
  };
}

function AlertRow({ alert }: { alert: AdminAlert }) {
  const { t, i18n } = useT();
  const text = useAlertText();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const acknowledge = useMutation({
    mutationFn: () => api<AdminAlert>(`/api/admin/alerts/${alert.id}/acknowledge`, { method: "POST" }),
    onSuccess: () => {
      toast(t("alerts.acknowledgedToast"), "success");
      void queryClient.invalidateQueries({ queryKey: ["admin", "alerts"] });
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const { text: sentence, hint } = text(alert);
  const resolved = alert.resolvedAt !== null;
  const dot = resolved ? LEVELS.ok.dot : alert.level === "info" ? "bg-pink" : LEVELS[LEVEL_OF[alert.level]].dot;
  return (
    <li className="flex flex-wrap items-start gap-3 border-b border-hairline py-3 last:border-b-0">
      <span className={clsx("mt-1.5 size-2.5 shrink-0 rounded-full", dot)} aria-hidden />
      <div className="min-w-0 flex-1 basis-60">
        <p className={clsx("text-sm", resolved ? "text-muted" : "font-semibold")}>
          <span className="sr-only">{t(resolved ? "alerts.level.resolved" : `alerts.level.${alert.level}`)}: </span>
          {sentence}
        </p>
        {!resolved && hint && <p className="mt-0.5 text-[13px] text-muted">{hint}</p>}
        <p className="mt-0.5 text-[12px] text-faint">
          {resolved
            ? t("alerts.resolvedAt", { time: formatRelative(alert.resolvedAt ?? 0, i18n.language) })
            : t("alerts.since", { time: formatRelative(alert.firstSeen, i18n.language) })}
          {alert.acknowledgedBy && !resolved && ` · ${t("alerts.acknowledgedBy", { login: alert.acknowledgedBy })}`}
        </p>
        {!resolved && alert.link && (
          <Link
            to={alert.link}
            className="mt-1 inline-flex items-center gap-0.5 text-[13px] font-semibold text-pink-ink hover:underline"
          >
            {t("health.open")}
            <ChevronRight className="size-3.5" aria-hidden />
          </Link>
        )}
      </div>
      {!resolved && alert.level !== "info" && !alert.acknowledgedAt && (
        <Button
          size="sm"
          icon={Check}
          busy={acknowledge.isPending}
          title={t("alerts.acknowledgeHint")}
          onClick={() => acknowledge.mutate()}
        >
          {t("alerts.acknowledge")}
        </Button>
      )}
    </li>
  );
}

/** Which alert mails this admin gets; each admin chooses for themselves. */
export function AlertMailChoice() {
  const { t } = useT();
  const { alertMails } = useAdminPrefs();
  const save = useSaveAdminPref();
  return (
    <div className="flex flex-col gap-1.5">
      <span className="text-[13px] font-semibold">{t("alerts.mail.label")}</span>
      <Segmented<AlertMails>
        label={t("alerts.mail.label")}
        value={alertMails}
        onChange={(value) => save.mutate({ adminAlerts: value })}
        options={(["all", "problems", "none"] as const).map((value) => ({ value, label: t(`alerts.mail.${value}`) }))}
      />
      <span className="text-[12px] text-muted">{t("alerts.mail.hint")}</span>
    </div>
  );
}

/** The alerts on the server overview: open ones first, what was fine again folded away. */
export function AlertsCard() {
  const { t } = useT();
  const alerts = useAlerts();
  const data = alerts.data;
  return (
    <Card
      title={
        <span className="flex items-center gap-2">
          <BellRing className="size-4 text-pink-ink" aria-hidden />
          {t("alerts.title")}
        </span>
      }
    >
      <div className="flex flex-col gap-3">
        <p className="-mt-1 text-[13px] text-muted">{t("alerts.intro")}</p>
        {!data ? (
          <p className="text-sm text-muted">{alerts.isError ? t("errors.loadFailed") : t("alerts.loading")}</p>
        ) : data.open.length === 0 ? (
          <p className="flex items-center gap-2 text-sm text-muted">
            <Info className="size-4" aria-hidden />
            {t("alerts.none")}
          </p>
        ) : (
          <ul aria-label={t("alerts.openLabel")}>
            {data.open.map((alert) => (
              <AlertRow key={alert.id} alert={alert} />
            ))}
          </ul>
        )}
        {data && data.resolved.length > 0 && (
          <details>
            <summary className="cursor-pointer text-[13px] font-semibold text-muted hover:text-ink">
              {t("alerts.history", { count: data.resolved.length })}
            </summary>
            <ul className="mt-1">
              {data.resolved.map((alert) => (
                <AlertRow key={alert.id} alert={alert} />
              ))}
            </ul>
          </details>
        )}
        <div className="border-t border-hairline pt-4">
          <AlertMailChoice />
        </div>
      </div>
    </Card>
  );
}

import { Button, ICONS } from "@uwusuite/design";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Section, ToggleField } from "@/features/settings/SettingsPage";
import { useT } from "@/i18n";
import { api, type SettingsView, type SpamNetStatus } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatNumber, formatRelative } from "@/lib/format";
import { toast } from "@/state/toasts";

const spamnetKey = ["admin", "spamnet"] as const;

function Line({ text, failed }: { text: string; failed: boolean }) {
  return <p className={failed ? "text-danger-ink" : undefined}>{text}</p>;
}

function Status({ status }: { status: SpamNetStatus }) {
  const { t, i18n } = useT();
  const language = i18n.language;
  const fetch = !status.fetch
    ? null
    : status.fetchError
      ? t("spam.spamnet.fetchFailed", { error: status.fetchError })
      : status.fetchedAt === null
        ? t("spam.spamnet.notYet")
        : t("spam.spamnet.fetched", {
            count: status.entries,
            entries: formatNumber(status.entries, language),
            when: formatRelative(status.fetchedAt, language),
          });
  const report = !status.report
    ? null
    : status.reportError
      ? t("spam.spamnet.reportFailed", { error: status.reportError })
      : status.reportedAt === null
        ? t("spam.spamnet.notYet")
        : t("spam.spamnet.reported", {
            count: status.sent,
            sent: formatNumber(status.sent, language),
            queued: formatNumber(status.queued, language),
            when: formatRelative(status.reportedAt, language),
          });
  if (fetch === null && report === null) return null;
  return (
    <div className="flex flex-col gap-1 text-[12px] text-muted" aria-live="polite">
      {fetch !== null && <Line text={fetch} failed={!!status.fetchError} />}
      {report !== null && <Line text={report} failed={!!status.reportError} />}
    </div>
  );
}

/** The shared Spam-Netz: fetching its list and reporting to it, each on its own, and what is shared. */
export function SpamNetCard({ view }: { view: SettingsView }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const query = useQuery({ queryKey: spamnetKey, queryFn: () => api<SpamNetStatus>("/api/admin/spamnet") });
  const sync = useMutation({
    mutationFn: () => api<SpamNetStatus>("/api/admin/spamnet/sync", { method: "POST" }),
    onSuccess: (next) => {
      queryClient.setQueryData(spamnetKey, next);
      const error = next.fetchError ?? next.reportError;
      if (error) toast(t("spam.spamnet.syncFailed", { error }), "error");
      else toast(t("spam.spamnet.synced"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const status = query.data;

  return (
    <Section
      title={t("spam.spamnet.title")}
      intro={t("spam.spamnet.intro")}
      view={view}
      keys={["spamnet.fetch", "spamnet.report"]}
      onSaved={() => void queryClient.invalidateQueries({ queryKey: spamnetKey })}
    >
      {(form) => (
        <>
          <ToggleField
            form={form}
            settingKey="spamnet.fetch"
            label={t("spam.spamnet.fetchLabel")}
            hint={t("spam.spamnet.fetchHint")}
          />
          <ToggleField
            form={form}
            settingKey="spamnet.report"
            label={t("spam.spamnet.reportLabel")}
            hint={t("spam.spamnet.reportHint")}
          />
          <div className="grid gap-3 text-[13px] sm:grid-cols-2">
            <div>
              <h4 className="font-medium text-ink">{t("spam.spamnet.sharedTitle")}</h4>
              <p className="text-muted">{t("spam.spamnet.shared")}</p>
            </div>
            <div>
              <h4 className="font-medium text-ink">{t("spam.spamnet.notSharedTitle")}</h4>
              <p className="text-muted">{t("spam.spamnet.notShared")}</p>
            </div>
          </div>
          {status && (status.fetch || status.report) && (
            <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
              <Status status={status} />
              <Button size="sm" variant="ghost" icon={ICONS.refresh} busy={sync.isPending} onClick={() => sync.mutate()}>
                {t("spam.spamnet.sync")}
              </Button>
            </div>
          )}
        </>
      )}
    </Section>
  );
}

/** Our half of the one-time card "New: Spam-Netz and parcel lookup"; the parcel lookup brings the other half. */
export function SpamNetNews() {
  const { t } = useT();
  return (
    <div className="flex flex-col gap-1">
      <h3 className="text-[14px] font-medium text-ink">{t("spam.spamnet.newsTitle")}</h3>
      <p className="text-[13px] text-muted">{t("spam.spamnet.newsText")}</p>
    </div>
  );
}

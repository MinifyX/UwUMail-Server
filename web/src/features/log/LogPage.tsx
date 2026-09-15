import { useInfiniteQuery } from "@tanstack/react-query";
import { useState } from "react";
import type { TFunction } from "i18next";
import { History } from "lucide-react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { PageHeader } from "@/components/ui/Card";
import { EmptyState } from "@/components/ui/EmptyState";
import { useT } from "@/i18n";
import { api, type AuditRecord } from "@/lib/api";
import { dayKey, formatBytes, formatDate, formatTime } from "@/lib/format";
import { usePrefs } from "@/state/prefs";

const PAGE = 50;

const KNOWN_ACTIONS = new Set([
  "accountCreate",
  "accountUpdate",
  "accountTrash",
  "accountRestore",
  "accountPurge",
  "accountPasswordLink",
  "accountPasswordSet",
  "accountPasswordChosen",
  "aliasAdd",
  "aliasRemove",
  "domainCreate",
  "domainRemove",
  "domainCatchAll",
  "domainDkimPrepare",
  "domainDkimActivate",
  "domainDkimRemove",
  "queueRetry",
  "queueDrop",
]);

function actorName(actor: string, t: TFunction) {
  return actor === "cli" || actor === "system" ? t(`log.actors.${actor}`) : actor;
}

export function describe(record: AuditRecord, t: TFunction): string {
  const key = record.action.replace(/\.(\w)/g, (_, char: string) => char.toUpperCase());
  const values = { actor: actorName(record.actor, t), target: record.target, action: record.action };
  return KNOWN_ACTIONS.has(key) ? t(`log.actions.${key}`, values) : t("log.actions.other", values);
}

/** The interesting parts of a change in words: "is now an admin", "storage limit 5 GB". */
export function detailText(record: AuditRecord, t: TFunction, language: string): string {
  const details = record.details ?? {};
  const parts: string[] = [];
  if (typeof details.admin === "boolean") parts.push(t(details.admin ? "log.details.adminOn" : "log.details.adminOff"));
  if (typeof details.disabled === "boolean") {
    parts.push(t(details.disabled ? "log.details.disabledOn" : "log.details.disabledOff"));
  }
  if (typeof details.quotaBytes === "number" && record.action === "account.update") {
    const value = details.quotaBytes === 0 ? t("people.create.unlimited") : formatBytes(details.quotaBytes, language);
    parts.push(t("log.details.quota", { value }));
  }
  if (typeof details.name === "string") parts.push(t("log.details.name", { value: details.name }));
  if (details.invited === true) parts.push(t("log.details.invited"));
  if (details.reason === "trash") parts.push(t("log.details.reasonTrash"));
  return parts.join(" · ");
}

export function LogPage() {
  const { t, i18n } = useT();
  const pro = usePrefs((s) => s.mode) === "pro";
  const [today] = useState(() => dayKey(Date.now() / 1000));
  const log = useInfiniteQuery({
    queryKey: ["admin", "audit"],
    queryFn: ({ pageParam }) =>
      api<AuditRecord[]>(`/api/admin/audit?limit=${PAGE}${pageParam ? `&before=${pageParam}` : ""}`),
    initialPageParam: 0,
    getNextPageParam: (last) => (last.length === PAGE ? last[last.length - 1]?.id : undefined),
  });

  if (log.isPending) return <Loading />;
  if (log.isError) return <LoadError error={log.error} onRetry={() => void log.refetch()} />;
  const records = log.data.pages.flat();

  const dayLabel = (key: number) =>
    key === today
      ? t("log.today")
      : key === today - 86_400_000
        ? t("log.yesterday")
        : formatDate(key / 1000, i18n.language);

  const days: { key: number; records: AuditRecord[] }[] = [];
  for (const record of records) {
    const key = dayKey(record.at);
    const last = days[days.length - 1];
    if (last?.key === key) last.records.push(record);
    else days.push({ key, records: [record] });
  }

  return (
    <div className="flex flex-col gap-5">
      <PageHeader title={t("log.title")} intro={t("log.intro")} />
      {records.length === 0 ? (
        <EmptyState scene="emptyFolder" title={t("log.empty.title")} body={t("log.empty.body")} compact={pro} />
      ) : (
        <>
          {days.map((day) => (
            <section key={day.key} className="flex flex-col gap-2">
              <h2 className="px-1 text-[12px] font-semibold tracking-wide text-faint uppercase">{dayLabel(day.key)}</h2>
              <ul className="rounded-card border border-hairline bg-surface">
                {day.records.map((record) => {
                  const details = detailText(record, t, i18n.language);
                  return (
                    <li key={record.id} className="flex gap-3 border-b border-hairline px-4 py-3 last:border-b-0">
                      <History className="mt-0.5 size-4 shrink-0 text-faint" aria-hidden />
                      <div className="min-w-0 flex-1">
                        <p className="text-sm break-words">{describe(record, t)}</p>
                        {details && <p className="text-[13px] text-muted">{details}</p>}
                        {pro && record.ip && <p className="text-[12px] text-faint">{record.ip}</p>}
                      </div>
                      <time
                        className="shrink-0 text-[12px] text-muted"
                        dateTime={new Date(record.at * 1000).toISOString()}
                      >
                        {formatTime(record.at, i18n.language)}
                      </time>
                    </li>
                  );
                })}
              </ul>
            </section>
          ))}
          {log.hasNextPage && (
            <Button className="self-center" busy={log.isFetchingNextPage} onClick={() => void log.fetchNextPage()}>
              {t("common.loadMore")}
            </Button>
          )}
        </>
      )}
    </div>
  );
}

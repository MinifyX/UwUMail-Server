import { useInfiniteQuery } from "@tanstack/react-query";
import { useState } from "react";
import type { TFunction } from "i18next";
import { ChevronDown, History } from "lucide-react";
import clsx from "clsx";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { EmptyState } from "@/components/ui/EmptyState";
import { useT } from "@/i18n";
import { api, type AuditRecord } from "@/lib/api";
import { dayKey, formatBytes, formatDate, formatDateTime, formatTime } from "@/lib/format";

const PAGE = 50;

const KNOWN_ACTIONS = new Set([
  "accountCreate",
  "accountUpdate",
  "accountProtocols",
  "accountAppPasswordCreated",
  "accountAppPasswordRevoked",
  "accountSendAsDomains",
  "backupRun",
  "backupSettings",
  "backupShowRecoveryKey",
  "backupForgetHostKey",
  "domainCloudflare",
  "domainMtaSts",
  "domainForwardAddress",
  "domainForwardAddressRemove",
  "gatewayPair",
  "gatewayForget",
  "gatewayJob",
  "hostJob",
  "spamLogCleared",
  "spamVirusTest",
  "accountTrash",
  "accountRestore",
  "accountPurge",
  "accountPasswordLink",
  "accountPasswordSet",
  "accountPasswordChosen",
  "accountSecondFactorsReset",
  "accountExternalForwarding",
  "accountAliasLimit",
  "domainSelfServiceAliases",
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
  "settingsUpdate",
  "updatesSettings",
  "spamLearnFromFolders",
  "spamSenderAdd",
  "spamSenderRemove",
  "spamWordsAdd",
  "spamWordRemove",
  "spamWordSourceAdd",
  "spamWordSourceRemove",
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
  if (typeof details.service === "boolean") {
    parts.push(t(details.service ? "log.details.serviceOn" : "log.details.serviceOff"));
  }
  if (details.protocols && typeof details.protocols === "object") {
    const protocols = details.protocols as Record<string, unknown>;
    const on = Object.entries(protocols)
      .filter(([, value]) => value === true)
      .map(([key]) => key.toUpperCase());
    parts.push(on.length > 0 ? t("log.details.protocols", { value: on.join(", ") }) : t("log.details.protocolsNone"));
  }
  if (typeof details.redirectTo === "string") {
    parts.push(
      details.redirectTo ? t("log.details.redirectTo", { address: details.redirectTo }) : t("log.details.redirectNone"),
    );
  }
  if (typeof details.name === "string" && record.action.startsWith("account.appPassword")) {
    parts.push(t("log.details.appPassword", { value: details.name }));
  } else if (typeof details.name === "string") {
    parts.push(t("log.details.name", { value: details.name }));
  }
  if (details.invited === true) parts.push(t("log.details.invited"));
  if (details.reason === "trash") parts.push(t("log.details.reasonTrash"));
  if (record.action.startsWith("spam.word")) {
    if (typeof details.added === "number") parts.push(t("log.details.wordsAdded", { count: details.added }));
    parts.push(
      typeof details.domain === "string"
        ? t("log.details.senderDomain", { domain: details.domain })
        : t("log.details.senderServer"),
    );
  }
  if (record.action.startsWith("spam.sender")) {
    if (details.list === "allow" || details.list === "block") {
      parts.push(t(details.list === "allow" ? "log.details.senderAllow" : "log.details.senderBlock"));
    }
    parts.push(
      typeof details.domain === "string"
        ? t("log.details.senderDomain", { domain: details.domain })
        : t("log.details.senderServer"),
    );
  }
  return parts.join(" · ");
}

function DetailRow({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <>
      <dt className="text-muted">{label}</dt>
      <dd className={clsx("min-w-0 break-words", mono && "font-mono text-[12px]")}>{value}</dd>
    </>
  );
}

function LogEntry({ record }: { record: AuditRecord }) {
  const { t, i18n } = useT();
  const [open, setOpen] = useState(false);
  const details = detailText(record, t, i18n.language);
  const raw = Object.keys(record.details ?? {}).length > 0 ? JSON.stringify(record.details, null, 2) : null;

  return (
    <li className="border-b border-hairline last:border-b-0">
      <button
        type="button"
        aria-expanded={open}
        className="flex w-full gap-3 px-4 py-3 text-left hover:bg-canvas/60"
        onClick={() => setOpen((was) => !was)}
      >
        <History className="mt-0.5 size-4 shrink-0 text-faint" aria-hidden />
        <span className="min-w-0 flex-1">
          <span className="block text-sm break-words">{describe(record, t)}</span>
          {details && <span className="block text-[13px] text-muted">{details}</span>}
        </span>
        <time className="shrink-0 text-[12px] text-muted" dateTime={new Date(record.at * 1000).toISOString()}>
          {formatTime(record.at, i18n.language)}
        </time>
        <ChevronDown
          className={clsx("mt-0.5 size-4 shrink-0 text-faint transition-transform", open && "rotate-180")}
          aria-hidden
        />
      </button>
      {open && (
        <div className="flex flex-col gap-3 px-4 pb-4 pl-11">
          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-[13px]">
            <DetailRow label={t("log.entry.actor")} value={actorName(record.actor, t)} />
            {record.target && <DetailRow label={t("log.entry.target")} value={record.target} />}
            <DetailRow label={t("log.entry.action")} value={record.action} mono />
            <DetailRow label={t("log.entry.time")} value={formatDateTime(record.at, i18n.language)} />
            {record.ip && <DetailRow label={t("log.entry.ip")} value={record.ip} mono />}
          </dl>
          {raw ? (
            <div className="flex flex-col gap-1">
              <span className="text-[12px] font-semibold text-muted">{t("log.entry.raw")}</span>
              <pre className="overflow-x-auto rounded-control bg-canvas px-3 py-2 font-mono text-[12px] whitespace-pre-wrap">
                {raw}
              </pre>
            </div>
          ) : (
            <p className="text-[13px] text-muted">{t("log.entry.noDetails")}</p>
          )}
        </div>
      )}
    </li>
  );
}

export function LogPage() {
  const { t, i18n } = useT();
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
      {records.length === 0 ? (
        <EmptyState scene="emptyFolder" title={t("log.empty.title")} body={t("log.empty.body")} compact />
      ) : (
        <>
          {days.map((day) => (
            <section key={day.key} className="flex flex-col gap-2">
              <h2 className="px-1 text-[12px] font-semibold tracking-wide text-faint uppercase">{dayLabel(day.key)}</h2>
              <ul className="rounded-card border border-hairline bg-surface">
                {day.records.map((record) => (
                  <LogEntry key={record.id} record={record} />
                ))}
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

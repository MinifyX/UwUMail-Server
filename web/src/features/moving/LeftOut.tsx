import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useT } from "@/i18n";
import { api, type LeftOutCounts, type LeftOutMessage, type LeftOutView } from "@/lib/api";
import { formatBytes, formatDateTime } from "@/lib/format";

type Translate = ReturnType<typeof useT>["t"];

/**
 * Why a move left messages out, one part per reason that happened: "3 here already · 2 too large".
 * Moves from before the counts were kept apart only have the total; that rest is "left out earlier".
 */
export function leftOutParts(t: Translate, counts: LeftOutCounts): string[] {
  const parts: string[] = [];
  if (counts.messagesKnown > 0) parts.push(t("leftOut.known", { count: counts.messagesKnown }));
  if (counts.messagesTooLarge > 0) parts.push(t("leftOut.tooLarge", { count: counts.messagesTooLarge }));
  if (counts.messagesUnreadable > 0) parts.push(t("leftOut.unreadable", { count: counts.messagesUnreadable }));
  const earlier = counts.messagesSkipped - counts.messagesKnown - counts.messagesTooLarge - counts.messagesUnreadable;
  if (earlier > 0) parts.push(t("leftOut.earlier", { count: earlier }));
  return parts;
}

/** Why one message was left out, in words; for "too large" with the limit. */
export function leftOutReason(t: Translate, message: LeftOutMessage, maxSize: number, language: string): string {
  if (message.reason === "tooLarge") {
    return maxSize > 0
      ? t("leftOut.reasons.tooLarge", { limit: formatBytes(maxSize, language) })
      : t("leftOut.reasons.tooLargeAny");
  }
  return t(`leftOut.reasons.${message.reason}`);
}

function LeftOutTable({ path, total }: { path: string; total: number }) {
  const { t, i18n } = useT();
  const list = useQuery({
    // Asked again when the counts grow.
    queryKey: ["leftOut", path, total],
    queryFn: () => api<LeftOutView>(path),
  });
  if (list.isPending) return <p className="mt-1">{t("leftOut.loading")}</p>;
  if (list.isError) return <p className="mt-1 text-danger">{t("leftOut.failed")}</p>;
  const { messages, max, maxSize } = list.data;
  if (messages.length === 0) return <p className="mt-1">{t("leftOut.empty")}</p>;
  return (
    <div className="mt-1 flex flex-col gap-1">
      <div className="max-h-72 overflow-auto rounded-control border border-hairline">
        <table className="w-full min-w-[560px] text-left text-[12px]">
          <thead className="sticky top-0 bg-surface text-muted">
            <tr>
              <th className="px-2 py-1 font-semibold">{t("leftOut.columns.folder")}</th>
              <th className="px-2 py-1 font-semibold">{t("leftOut.columns.from")}</th>
              <th className="px-2 py-1 font-semibold">{t("leftOut.columns.subject")}</th>
              <th className="px-2 py-1 font-semibold">{t("leftOut.columns.date")}</th>
              <th className="px-2 py-1 text-right font-semibold">{t("leftOut.columns.size")}</th>
              <th className="px-2 py-1 font-semibold">{t("leftOut.columns.reason")}</th>
            </tr>
          </thead>
          <tbody>
            {messages.map((message) => (
              <tr
                key={`${message.folder}-${message.uid}-${message.reason}`}
                className="border-t border-hairline text-ink"
              >
                <td className="max-w-[140px] truncate px-2 py-1" title={message.folder}>
                  {message.folder}
                </td>
                <td className="max-w-[180px] truncate px-2 py-1" title={message.from}>
                  {message.from || "–"}
                </td>
                <td className="max-w-[220px] truncate px-2 py-1" title={message.subject}>
                  {message.subject || <span className="text-muted">{t("leftOut.noSubject")}</span>}
                </td>
                <td className="px-2 py-1 whitespace-nowrap">
                  {message.date ? formatDateTime(message.date, i18n.language) : "–"}
                </td>
                <td className="px-2 py-1 text-right whitespace-nowrap">{formatBytes(message.size, i18n.language)}</td>
                <td className="px-2 py-1 text-muted">{leftOutReason(t, message, maxSize, i18n.language)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {messages.length >= max && <p>{t("leftOut.capped", { max })}</p>}
    </div>
  );
}

/** What a move left out: the counts by reason, and the list from `path`, loaded when it is opened. */
export function LeftOut({ counts, path }: { counts: LeftOutCounts; path: string }) {
  const { t } = useT();
  const [open, setOpen] = useState(false);
  const parts = leftOutParts(t, counts);
  if (parts.length === 0) return null;
  return (
    <div className="flex flex-col gap-0.5 text-[12px] text-muted">
      <p>{parts.join(" · ")}</p>
      <details onToggle={(event) => setOpen(event.currentTarget.open)}>
        <summary className="cursor-pointer font-semibold text-pink-ink hover:underline">{t("leftOut.show")}</summary>
        {open && <LeftOutTable path={path} total={counts.messagesSkipped} />}
      </details>
    </div>
  );
}

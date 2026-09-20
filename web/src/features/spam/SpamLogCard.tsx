import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import clsx from "clsx";
import { Trash2 } from "lucide-react";
import { useState } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Dialog } from "@/components/ui/Dialog";
import { EmptyState } from "@/components/ui/EmptyState";
import { Field, Segmented, Select, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type SpamLogAction, type SpamLogEntry, type SpamLogView } from "@/lib/api";
import { formatDate, formatDateTime, formatNumber } from "@/lib/format";
import { usePhone } from "@/lib/media";
import { toast } from "@/state/toasts";

const ACTIONS: (SpamLogAction | "all")[] = [
  "all",
  "junk",
  "reject",
  "greylist",
  "dmarc",
  "blocked",
  "virus",
  "delivered",
];

/** Held-back mail is what the history is for, so it is the one that stands out. */
const TONE: Record<SpamLogAction, string> = {
  delivered: "text-muted",
  junk: "text-warning",
  greylist: "text-warning",
  reject: "text-danger",
  dmarc: "text-danger",
  blocked: "text-danger",
  virus: "text-danger",
  settled: "text-muted",
};

function Line({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-wrap gap-x-2 border-b border-hairline py-1.5 last:border-b-0">
      <span className="min-w-36 text-[12px] text-muted">{label}</span>
      <span className="min-w-0 flex-1 text-[13px] break-words">{children}</span>
    </div>
  );
}

function EntryDialog({ entry, onClose }: { entry: SpamLogEntry | null; onClose: () => void }) {
  const { t, i18n } = useT();
  // A virus has no score, so its name rides along as a rule of its own.
  const virus = entry?.hits.find((hit) => hit.rule === "VIRUS")?.detail;
  return (
    <Dialog open={entry !== null} onClose={onClose} title={t("spam.log.oneMessage")}>
      {entry && (
        <div className="flex flex-col gap-4 px-6 pb-6">
          <div>
            <Line label={t("spam.log.when")}>{formatDateTime(entry.at, i18n.language)}</Line>
            <Line label={t("spam.log.action")}>
              <span className={TONE[entry.action]}>{t(`spam.log.actions.${entry.action}`)}</span>
              {entry.correctedToJunk !== null && (
                <span className="ml-2 text-muted">
                  {entry.correctedToJunk ? t("spam.log.laterSpam") : t("spam.log.laterNotSpam")}
                </span>
              )}
            </Line>
            <Line label={t("spam.log.from")}>{entry.headerFrom || entry.envelopeFrom || "–"}</Line>
            {entry.envelopeFrom && entry.envelopeFrom !== entry.headerFrom && (
              <Line label={t("spam.log.envelope")}>{entry.envelopeFrom}</Line>
            )}
            {entry.subject && <Line label={t("spam.log.subject")}>{entry.subject}</Line>}
            {virus && <Line label={t("spam.log.virus")}>{virus}</Line>}
            <Line label={t("spam.log.server")}>
              <span className="font-mono text-[12px]">{entry.clientIp || "–"}</span>
              {entry.reverseName && <span className="ml-2">{entry.reverseName}</span>}
              {entry.helo && <span className="ml-2 text-muted">HELO {entry.helo}</span>}
            </Line>
            <Line label={t("spam.log.recipients")}>
              {entry.recipients.length === 0
                ? "–"
                : entry.recipients
                    .map((to) => `${to.address} (${t(`spam.log.actions.${to.action as SpamLogAction}`)})`)
                    .join(", ")}
            </Line>
            <Line label={t("spam.log.size")}>{formatNumber(Math.round(entry.size / 1024), i18n.language)} KB</Line>
            <Line label={t("spam.log.smtpId")}>
              <span className="font-mono text-[12px] break-all">{entry.smtpId}</span>
            </Line>
            {entry.messageId && (
              <Line label={t("spam.log.messageId")}>
                <span className="font-mono text-[12px] break-all">{entry.messageId}</span>
              </Line>
            )}
          </div>

          {entry.score !== null && (
            <div>
              <p className="mb-1 text-[12px] text-muted">
                {t("spam.log.scoreOf", { score: entry.score.toFixed(1), count: entry.hits.length })}
              </p>
              {entry.hits.length === 0 ? (
                <p className="text-[13px] text-muted">{t("spam.log.noRules")}</p>
              ) : (
                <ul className="flex flex-col">
                  {entry.hits.map((hit, index) => (
                    <li
                      key={`${hit.rule}-${index}`}
                      className="flex flex-wrap items-baseline gap-x-2 border-b border-hairline py-1 last:border-b-0"
                    >
                      <span className="font-mono text-[12px]">{hit.rule}</span>
                      {hit.detail && <span className="min-w-0 flex-1 text-[12px] text-muted">{hit.detail}</span>}
                      <span
                        className={clsx(
                          "ml-auto text-[13px] font-semibold tabular-nums",
                          hit.points > 0 ? "text-danger" : "text-success",
                        )}
                      >
                        {hit.points > 0 ? "+" : ""}
                        {hit.points.toFixed(1)}
                      </span>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          )}

          {entry.auth && (
            <div>
              <p className="mb-1 text-[12px] text-muted">{t("spam.log.authResults")}</p>
              <pre className="overflow-x-auto rounded-control bg-canvas px-3 py-2 font-mono text-[12px] whitespace-pre-wrap">
                {entry.auth}
              </pre>
            </div>
          )}
        </div>
      )}
    </Dialog>
  );
}

/** What the filter decided, message by message. The one place that says who writes to whom. */
export function SpamLogCard() {
  const { t, i18n } = useT();
  const queryClient = useQueryClient();
  const [action, setAction] = useState<SpamLogAction | "all">("all");
  const [search, setSearch] = useState("");
  const [open, setOpen] = useState<SpamLogEntry | null>(null);
  const [clearing, setClearing] = useState(false);
  const phone = usePhone();

  const query = useQuery({
    queryKey: ["admin", "spam", "log", action, search.trim()],
    queryFn: () => {
      const params = new URLSearchParams({ limit: "50" });
      if (action !== "all") params.set("action", action);
      if (search.trim().length > 2) params.set("search", search.trim());
      return api<SpamLogView>(`/api/admin/spam/log?${params}`);
    },
  });

  const clear = useMutation({
    mutationFn: () => api<{ removed: number }>("/api/admin/spam/log", { method: "DELETE" }),
    onSuccess: (result) => {
      setClearing(false);
      toast(t("spam.log.cleared", { count: result.removed }), "success");
      void queryClient.invalidateQueries({ queryKey: ["admin", "spam", "log"] });
    },
  });

  const body = () => {
    if (query.isPending) return <Loading />;
    if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
    const { entries, total, oldest, settings } = query.data;
    return (
      <div className="flex flex-col gap-3">
        <p className="text-[13px] text-muted">{t("spam.log.explain")}</p>
        {!settings.enabled && <p className="rounded-control bg-canvas px-3 py-2 text-[13px]">{t("spam.log.off")}</p>}
        <p className="rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
          {t("spam.log.privacy", { days: settings.retentionDays })}{" "}
          {settings.cleanSubjects ? t("spam.log.subjectsOn") : t("spam.log.subjectsOff")}
        </p>

        {/* Seven of them side by side are wider than a phone, so there they become a dropdown. */}
        {phone ? (
          <Field label={t("spam.log.filter")}>
            {(id) => (
              <Select
                id={id}
                value={action}
                onChange={(event) => setAction(event.target.value as SpamLogAction | "all")}
              >
                {ACTIONS.map((value) => (
                  <option key={value} value={value}>
                    {t(`spam.log.filters.${value}`)}
                  </option>
                ))}
              </Select>
            )}
          </Field>
        ) : (
          <Segmented<string>
            label={t("spam.log.filter")}
            value={action}
            onChange={(value) => setAction(value as SpamLogAction | "all")}
            options={ACTIONS.map((value) => ({ value, label: t(`spam.log.filters.${value}`) }))}
          />
        )}
        <TextInput
          value={search}
          onChange={(event) => setSearch(event.target.value)}
          placeholder={t("spam.log.searchPlaceholder")}
          aria-label={t("spam.log.search")}
        />

        {entries.length === 0 ? (
          <EmptyState compact scene="search" title={t("spam.log.empty.title")} body={t("spam.log.empty.body")} />
        ) : (
          <ul className="flex flex-col">
            {entries.map((entry) => (
              <li key={entry.id}>
                <button
                  type="button"
                  onClick={() => setOpen(entry)}
                  className="flex w-full flex-wrap items-baseline gap-x-3 gap-y-0.5 rounded-control border-b border-hairline px-2 py-2 text-left hover:bg-pink-tint/40"
                >
                  <span className="text-[12px] text-muted tabular-nums">{formatDate(entry.at, i18n.language)}</span>
                  <span className={clsx("text-[12px] font-semibold", TONE[entry.action])}>
                    {t(`spam.log.actions.${entry.action}`)}
                  </span>
                  <span className="min-w-0 flex-1 basis-40 text-[13px] break-words">
                    {entry.headerFrom || entry.envelopeFrom || "–"}
                  </span>
                  {entry.subject && (
                    <span className="line-clamp-1 min-w-0 basis-full text-[12px] text-muted">{entry.subject}</span>
                  )}
                  {entry.score !== null && (
                    <span className="ml-auto text-[13px] font-semibold tabular-nums">{entry.score.toFixed(1)}</span>
                  )}
                </button>
              </li>
            ))}
          </ul>
        )}

        <div className="flex flex-wrap items-center justify-between gap-3 border-t border-hairline pt-3">
          <p className="text-[12px] text-muted">
            {t("spam.log.extent", {
              total: formatNumber(total, i18n.language),
              since: oldest ? formatDate(oldest, i18n.language) : "–",
            })}
          </p>
          <Button size="sm" variant="danger" icon={Trash2} onClick={() => setClearing(true)}>
            {t("spam.log.clear")}
          </Button>
        </div>
      </div>
    );
  };

  return (
    <Card title={t("spam.log.title")}>
      {body()}
      <EntryDialog entry={open} onClose={() => setOpen(null)} />
      <Dialog open={clearing} onClose={() => setClearing(false)} width="sm" title={t("spam.log.clearTitle")}>
        <div className="flex flex-col gap-4 px-6 pb-6">
          <p className="text-[13px] text-muted">{t("spam.log.clearBody")}</p>
          <div className="flex justify-end gap-2">
            <Button onClick={() => setClearing(false)}>{t("common.cancel")}</Button>
            <Button variant="danger" busy={clear.isPending} onClick={() => clear.mutate()}>
              {t("spam.log.clear")}
            </Button>
          </div>
        </div>
      </Dialog>
    </Card>
  );
}

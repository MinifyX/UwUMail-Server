import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link2, Plus, RefreshCw, Trash2 } from "lucide-react";
import { useState, type FormEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button, IconButton } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Field, Select, TextInput, Toggle } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type WordEntry, type WordImport, type WordSource, type WordsView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatNumber, formatRelative } from "@/lib/format";
import { usePrefs } from "@/state/prefs";
import { toast } from "@/state/toasts";

/** Entries shown at once; the filter finds the rest. */
const SHOWN = 100;

const textareaClass =
  "w-full rounded-control border border-line bg-surface px-3.5 py-2.5 font-mono text-[13px] text-ink placeholder:text-faint focus:border-pink focus:shadow-focus focus:outline-none disabled:opacity-60";

function ImportReport({ report }: { report: WordImport }) {
  const { t } = useT();
  return (
    <div className="flex flex-col gap-1 rounded-control bg-canvas px-3 py-2.5 text-[13px]">
      <p>
        {t("spam.words.imported", {
          added: report.added,
          duplicates: report.duplicates,
          refused: report.refusedCount,
        })}
      </p>
      {report.refused.length > 0 && (
        <ul className="flex flex-col gap-0.5 text-[12px] text-muted">
          {report.refused.map((refused) => (
            <li key={refused.line} className="break-all">
              <code className="text-ink">{refused.line}</code> · {refused.reason}
            </li>
          ))}
          {report.refusedCount > report.refused.length && (
            <li>{t("spam.words.moreRefused", { count: report.refusedCount - report.refused.length })}</li>
          )}
        </ul>
      )}
    </div>
  );
}

/** Words, phrases and /regex/flags: one's own in My account, the server's and the domains' for admins. */
export function WordListCard({ admin }: { admin: boolean }) {
  const { t, i18n } = useT();
  const language = i18n.language;
  const errorText = useErrorText();
  const pro = usePrefs((s) => s.mode) === "pro";
  const queryClient = useQueryClient();
  const key = admin ? ["admin", "spam", "words"] : ["account", "spam", "words"];
  const base = admin ? "/api/admin/spam" : "/api/account/spam";
  const query = useQuery({ queryKey: key, queryFn: () => api<WordsView>(`${base}/words`) });
  const [text, setText] = useState("");
  const [points, setPoints] = useState("");
  const [domain, setDomain] = useState("");
  const [url, setUrl] = useState("");
  const [subjectOnly, setSubjectOnly] = useState(false);
  const [filter, setFilter] = useState("");
  const [report, setReport] = useState<WordImport | null>(null);
  const scope = admin && domain ? { domain } : {};
  const saved = (lists: WordsView) => queryClient.setQueryData(key, lists);

  const add = useMutation({
    mutationFn: () =>
      api<{ import: WordImport; lists: WordsView }>(`${base}/words`, {
        method: "POST",
        body: { text, ...(points.trim() ? { points: Number(points.replace(",", ".")) } : {}), ...scope },
      }),
    onSuccess: (answer) => {
      saved(answer.lists);
      setReport(answer.import);
      if (answer.import.added > 0) setText("");
    },
  });
  const subscribe = useMutation({
    mutationFn: () =>
      api<{ error: string | null; lists: WordsView }>(`${base}/word-sources`, {
        method: "POST",
        body: { url: url.trim(), subjectOnly, ...scope },
      }),
    onSuccess: (answer) => {
      saved(answer.lists);
      setUrl("");
      if (answer.error) toast(t("spam.words.subscribedFailed", { error: answer.error }), "error");
      else toast(t("spam.words.subscribed"), "success");
    },
  });
  const refresh = useMutation({
    mutationFn: (source: WordSource) =>
      api<{ error: string | null; lists: WordsView }>(`${base}/word-sources/${source.id}/refresh`, { method: "POST" }),
    onSuccess: (answer) => {
      saved(answer.lists);
      if (answer.error) toast(t("spam.words.fetchFailed", { error: answer.error }), "error");
      else toast(t("spam.words.fetched"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const removeEntry = useMutation({
    mutationFn: (entry: WordEntry) => api<WordsView>(`${base}/words/${entry.id}`, { method: "DELETE" }),
    onSuccess: saved,
    onError: (error) => toast(errorText(error), "error"),
  });
  const unsubscribe = useMutation({
    mutationFn: (source: WordSource) => api<WordsView>(`${base}/word-sources/${source.id}`, { method: "DELETE" }),
    onSuccess: saved,
    onError: (error) => toast(errorText(error), "error"),
  });

  const title = admin ? t("spam.words.titleAdmin") : t("spam.words.title");
  if (query.isPending) {
    return (
      <Card title={title}>
        <Loading />
      </Card>
    );
  }
  if (query.isError) {
    return (
      <Card title={title}>
        <LoadError error={query.error} onRetry={() => void query.refetch()} />
      </Card>
    );
  }
  const view = query.data;
  const scopeName = (item: { domain: string | null }) => item.domain ?? t("spam.senders.wholeServer");
  const needle = filter.trim().toLowerCase();
  const matching = view.entries.filter((entry) => !needle || entry.pattern.toLowerCase().includes(needle));
  const submitWords = (event: FormEvent) => {
    event.preventDefault();
    add.mutate();
  };
  const submitSource = (event: FormEvent) => {
    event.preventDefault();
    subscribe.mutate();
  };

  return (
    <Card title={title}>
      <div className="flex flex-col gap-4">
        <p className="-mt-1 text-[13px] text-muted">
          {t(admin ? "spam.words.explainAdmin" : "spam.words.explain", {
            points: formatNumber(view.defaultPoints, language),
            max: formatNumber(view.maxPoints, language),
          })}
        </p>

        {admin && (
          <Field label={t("spam.senders.scope")} className="sm:max-w-xs">
            {(id) => (
              <Select id={id} value={domain} onChange={(event) => setDomain(event.target.value)}>
                <option value="">{t("spam.senders.wholeServer")}</option>
                {(view.domains ?? []).map((name) => (
                  <option key={name} value={name}>
                    {name}
                  </option>
                ))}
              </Select>
            )}
          </Field>
        )}

        <form className="flex flex-col gap-3" onSubmit={submitWords}>
          <Field
            label={t("spam.words.entries")}
            hint={t("spam.words.entriesHint")}
            error={add.isError ? errorText(add.error) : undefined}
          >
            {(id) => (
              <textarea
                id={id}
                rows={4}
                spellCheck={false}
                className={textareaClass}
                placeholder={t("spam.words.placeholder")}
                value={text}
                onChange={(event) => setText(event.target.value)}
              />
            )}
          </Field>
          <div className="flex flex-wrap items-end gap-3">
            {pro && (
              <Field label={t("spam.words.points")} className="w-36">
                {(id) => (
                  <TextInput
                    id={id}
                    inputMode="decimal"
                    placeholder={formatNumber(view.defaultPoints, language)}
                    value={points}
                    onChange={(event) => setPoints(event.target.value)}
                  />
                )}
              </Field>
            )}
            <Button type="submit" icon={Plus} busy={add.isPending} disabled={!text.trim()}>
              {t("spam.words.add")}
            </Button>
          </div>
          {report && <ImportReport report={report} />}
        </form>

        <section className="flex flex-col gap-3 border-t border-hairline pt-3">
          <h3 className="text-[13px] font-bold">{t("spam.words.subscribeTitle")}</h3>
          <form className="flex flex-col gap-3" onSubmit={submitSource}>
            <Field
              label={t("spam.words.url")}
              hint={t("spam.words.urlHint")}
              error={subscribe.isError ? errorText(subscribe.error) : undefined}
            >
              {(id) => (
                <TextInput
                  id={id}
                  type="url"
                  autoComplete="off"
                  spellCheck={false}
                  placeholder="https://…"
                  value={url}
                  onChange={(event) => setUrl(event.target.value)}
                />
              )}
            </Field>
            {pro && (
              <Toggle
                checked={subjectOnly}
                onChange={setSubjectOnly}
                label={t("spam.words.subjectOnly")}
                description={t("spam.words.subjectOnlyHint")}
              />
            )}
            <div>
              <Button type="submit" icon={Link2} busy={subscribe.isPending} disabled={!url.trim()}>
                {t("spam.words.subscribe")}
              </Button>
            </div>
          </form>
          {view.sources.length > 0 && (
            <ul className="flex flex-col">
              {view.sources.map((source) => (
                <li
                  key={source.id}
                  className="flex min-h-11 items-center gap-2 border-b border-hairline py-1.5 last:border-b-0"
                >
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-sm font-semibold">{source.url}</span>
                    <span className={`block truncate text-[12px] ${source.error ? "text-danger" : "text-muted"}`}>
                      {[
                        admin ? scopeName(source) : null,
                        source.subjectOnly ? t("spam.words.subjectOnlyShort") : null,
                        source.error
                          ? t("spam.words.fetchFailed", { error: source.error })
                          : source.fetchedAt === null
                            ? t("spam.words.notFetched")
                            : t("spam.words.sourceState", {
                                entries: formatNumber(source.entries, language),
                                when: formatRelative(source.fetchedAt, language),
                              }),
                      ]
                        .filter(Boolean)
                        .join(" · ")}
                    </span>
                  </span>
                  <IconButton
                    icon={RefreshCw}
                    size="sm"
                    label={t("spam.words.refresh")}
                    onClick={() => refresh.mutate(source)}
                  />
                  <IconButton
                    icon={Trash2}
                    size="sm"
                    label={t("spam.words.unsubscribe")}
                    onClick={() => unsubscribe.mutate(source)}
                  />
                </li>
              ))}
            </ul>
          )}
        </section>

        <section className="flex flex-col gap-2 border-t border-hairline pt-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <h3 className="text-[13px] font-bold">{t("spam.words.listTitle", { count: view.entries.length })}</h3>
            {view.entries.length > 10 && (
              <TextInput
                aria-label={t("spam.words.filter")}
                placeholder={t("spam.words.filter")}
                className="h-9 max-w-56"
                value={filter}
                onChange={(event) => setFilter(event.target.value)}
              />
            )}
          </div>
          {view.entries.length === 0 ? (
            <p className="rounded-control bg-canvas px-3 py-2.5 text-[13px] text-muted">{t("spam.words.empty")}</p>
          ) : (
            <ul className="flex flex-col">
              {matching.slice(0, SHOWN).map((entry) => (
                <li
                  key={entry.id}
                  className="flex min-h-10 items-center gap-2 border-b border-hairline py-1 last:border-b-0"
                >
                  <span className="min-w-0 flex-1">
                    <code className="block truncate text-[13px]">{entry.pattern}</code>
                    <span className="block truncate text-[12px] text-muted">
                      {[
                        admin ? scopeName(entry) : null,
                        t("spam.words.pointsShort", {
                          points: formatNumber(entry.points ?? view.defaultPoints, language),
                        }),
                      ]
                        .filter(Boolean)
                        .join(" · ")}
                    </span>
                  </span>
                  <IconButton
                    icon={Trash2}
                    size="sm"
                    label={t("spam.words.remove", { entry: entry.pattern })}
                    onClick={() => removeEntry.mutate(entry)}
                  />
                </li>
              ))}
              {matching.length > SHOWN && (
                <li className="py-2 text-[12px] text-muted">
                  {t("spam.words.more", { count: matching.length - SHOWN })}
                </li>
              )}
            </ul>
          )}
        </section>
      </div>
    </Card>
  );
}

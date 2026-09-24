import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link2, RefreshCw, Trash2 } from "lucide-react";
import { useState, type FormEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button, IconButton } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Field, Select, TextInput, Toggle } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type WordSource, type WordsView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatNumber, formatRelative } from "@/lib/format";
import { toast } from "@/state/toasts";

/**
 * Word lists subscribed to by link and fetched again every day: one's own in My account, the server's and
 * the domains' for admins. Their entries belong to the list, so they are not among the rules.
 */
export function SubscribedListsCard({ admin }: { admin: boolean }) {
  const { t, i18n } = useT();
  const language = i18n.language;
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const key = admin ? ["admin", "spam", "words"] : ["account", "spam", "words"];
  const base = admin ? "/api/admin/spam" : "/api/account/spam";
  const query = useQuery({ queryKey: key, queryFn: () => api<WordsView>(`${base}/words`) });
  const [domain, setDomain] = useState("");
  const [url, setUrl] = useState("");
  const [subjectOnly, setSubjectOnly] = useState(false);
  const scope = admin && domain ? { domain } : {};
  const saved = (lists: WordsView) => queryClient.setQueryData(key, lists);

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
  const unsubscribe = useMutation({
    mutationFn: (source: WordSource) => api<WordsView>(`${base}/word-sources/${source.id}`, { method: "DELETE" }),
    onSuccess: saved,
    onError: (error) => toast(errorText(error), "error"),
  });

  const title = t("spam.words.subscribeTitle");
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
  const submitSource = (event: FormEvent) => {
    event.preventDefault();
    subscribe.mutate();
  };

  return (
    <Card title={title}>
      <div className="flex flex-col gap-4">
        <p className="-mt-1 text-[13px] text-muted">{t("spam.rules.subscribedIntro")}</p>
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
          {
            <Toggle
              checked={subjectOnly}
              onChange={setSubjectOnly}
              label={t("spam.words.subjectOnly")}
              description={t("spam.words.subjectOnlyHint")}
            />
          }
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
                  {/* A fetch error is the one line here worth reading in full, so on a phone it
                        gets a second line instead of ending in an ellipsis. */}
                  <span
                    className={`block truncate text-[12px] max-sm:line-clamp-2 max-sm:whitespace-normal ${source.error ? "text-danger" : "text-muted"}`}
                  >
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
        {view.sources.length === 0 && (
          <p className="rounded-control bg-canvas px-3 py-2.5 text-[13px] text-muted">{t("spam.rules.noSubscribed")}</p>
        )}
      </div>
    </Card>
  );
}

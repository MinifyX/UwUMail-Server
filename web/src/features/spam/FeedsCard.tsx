import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ExternalLink, RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/Button";
import { Field, TextInput } from "@/components/ui/Field";
import { Section, ToggleField, type Form } from "@/features/settings/SettingsPage";
import { useT } from "@/i18n";
import { api, type FeedStatus, type FeedsView, type SettingsView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatNumber, formatRelative } from "@/lib/format";
import { toast } from "@/state/toasts";

const feedsKey = ["admin", "spam", "feeds"] as const;
const KEY_SETTING = "spam.feeds.abuse_ch_key";
const ORDER = ["urlhaus", "malware_bazaar", "bad_subjects", "disposable", "freemail", "redirectors"];

function Status({ feed, onRefresh, busy }: { feed: FeedStatus; onRefresh: () => void; busy: boolean }) {
  const { t, i18n } = useT();
  const language = i18n.language;
  let text: string;
  if (!feed.active) {
    text = feed.needsKey ? t("spam.feeds.waitsForKey") : t("spam.feeds.off");
  } else if (feed.error) {
    text = t("spam.feeds.failed", { error: feed.error });
  } else if (feed.fetchedAt === null) {
    text = t("spam.feeds.notYet");
  } else {
    text = t("spam.feeds.fetched", {
      count: feed.entries,
      entries: formatNumber(feed.entries, language),
      when: formatRelative(feed.fetchedAt, language),
    });
  }
  return (
    <div className="-mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 pl-0.5 text-[12px] text-muted">
      <span className={feed.active && feed.error ? "text-danger" : undefined}>{text}</span>
      <a href={feed.page} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1 hover:text-ink">
        <ExternalLink className="size-3" aria-hidden />
        {feed.source}
      </a>
      {feed.active && (
        <Button size="sm" variant="ghost" icon={RefreshCw} busy={busy} onClick={onRefresh}>
          {t("spam.feeds.refresh")}
        </Button>
      )}
    </div>
  );
}

function KeyField({ form }: { form: Form }) {
  const { t } = useT();
  const stored = form.setting(KEY_SETTING);
  const draft = form.value(KEY_SETTING);
  const locked = form.locked(KEY_SETTING);
  return (
    <Field
      label={t("spam.feeds.key")}
      hint={stored?.set && draft !== null ? t("spam.feeds.keySet") : t("spam.feeds.keyHint")}
    >
      {(id) => (
        <div className="flex gap-2">
          <TextInput
            id={id}
            type="password"
            autoComplete="off"
            disabled={locked}
            placeholder={stored?.set ? "••••••••" : ""}
            value={typeof draft === "string" ? draft : ""}
            onChange={(event) => form.set(KEY_SETTING, event.target.value === "" ? undefined : event.target.value)}
          />
          {stored?.set && !locked && (
            <Button onClick={() => form.set(KEY_SETTING, null)}>{t("spam.feeds.keyRemove")}</Button>
          )}
        </div>
      )}
    </Field>
  );
}

/** The built-in lists: a switch for each, how fetching it went, and the abuse.ch Auth-Key. */
export function FeedsCard({ view }: { view: SettingsView }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const query = useQuery({ queryKey: feedsKey, queryFn: () => api<FeedsView>("/api/admin/spam/feeds") });
  const refresh = useMutation({
    mutationFn: (key: string) => api<FeedsView>(`/api/admin/spam/feeds/${key}/refresh`, { method: "POST" }),
    onSuccess: (next) => {
      queryClient.setQueryData(feedsKey, next);
      if (next.error) toast(t("spam.feeds.failed", { error: next.error }), "error");
      else toast(t("spam.feeds.refreshed"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const feeds = query.data?.feeds ?? [];
  const byKey = (key: string) => feeds.find((feed) => feed.key === key);

  return (
    <Section
      title={t("spam.feeds.title")}
      intro={t("spam.feeds.intro")}
      view={view}
      keys={[...ORDER.map((key) => `spam.feeds.${key}`), KEY_SETTING]}
      onSaved={() => void queryClient.invalidateQueries({ queryKey: feedsKey })}
    >
      {(form) => (
        <>
          {ORDER.map((key) => {
            const feed = byKey(key);
            return (
              <div key={key} className="flex flex-col gap-2">
                <ToggleField
                  form={form}
                  settingKey={`spam.feeds.${key}`}
                  label={t(`spam.feeds.lists.${key}.label`)}
                  hint={t(`spam.feeds.lists.${key}.hint`)}
                />
                {feed && (
                  <Status
                    feed={feed}
                    busy={refresh.isPending && refresh.variables === key}
                    onRefresh={() => refresh.mutate(key)}
                  />
                )}
              </div>
            );
          })}
          <KeyField form={form} />
        </>
      )}
    </Section>
  );
}

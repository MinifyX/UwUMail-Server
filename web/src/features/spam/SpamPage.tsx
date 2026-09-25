import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import clsx from "clsx";
import {
  Ban,
  BookOpen,
  Check,
  ChevronRight,
  Clock,
  GraduationCap,
  Hash,
  History,
  LayoutDashboard,
  ListFilter,
  Rss,
  Settings2,
  ShieldAlert,
  Sparkles,
  Timer,
} from "lucide-react";
import { useState, type FormEvent, type ReactNode } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card, PageHeader } from "@/components/ui/Card";
import { Field, TextInput } from "@/components/ui/Field";
import { Tabs } from "@/components/ui/Tabs";
import {
  ANTIVIRUS_SETTING_KEYS,
  AntivirusFields,
  SPAM_LOG_SETTING_KEYS,
  SPAM_SETTING_KEYS,
  Section,
  SpamFields,
  SpamLogFields,
} from "@/features/settings/SettingsPage";
import { useT } from "@/i18n";
import {
  api,
  type AccountSpamView,
  type AdminSpamView,
  type BayesTotals,
  type GreylistView,
  type LearnedFromFolders,
  type Rule,
  type RulesView,
  type SettingsView,
} from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatNumber, formatRelative } from "@/lib/format";
import { Link } from "@/lib/router";
import { toast } from "@/state/toasts";
import { AntivirusCard } from "./AntivirusCard";
import { FeedsCard } from "./FeedsCard";
import { FetchedCard } from "./FetchedCard";
import { GreylistCard, greylistKey } from "./GreylistCard";
import { RulesPanel, rulesLink } from "./rules/RulesPanel";
import { ScopePicker } from "./rules/ScopePicker";
import { guessSenderKind } from "./senders";
import { SpamLimitsCard } from "./SpamLimitsCard";
import { SpamLogCard } from "./SpamLogCard";
import { SubscribedListsCard } from "./SubscribedListsCard";

const accountKey = ["account", "spam"] as const;
const adminKey = ["admin", "spam"] as const;

/** The admin's spam filter, one tab per thing to look after; every tab has its own address. */
export type SpamTab = "overview" | "rules" | "settings" | "lists" | "learning" | "antivirus" | "history";
const PATHS: Record<SpamTab, string> = {
  overview: "/admin/spam",
  rules: "/admin/spam/rules",
  settings: "/admin/spam/settings",
  lists: "/admin/spam/lists",
  learning: "/admin/spam/learning",
  antivirus: "/admin/spam/antivirus",
  history: "/admin/spam/history",
};
export const SPAM_TABS = Object.keys(PATHS) as SpamTab[];
const ICONS = {
  overview: LayoutDashboard,
  rules: ListFilter,
  settings: Settings2,
  lists: Rss,
  learning: GraduationCap,
  antivirus: ShieldAlert,
  history: History,
  waiting: Timer,
};

/** In My account: one's own rules, subscribed lists, what the filter learned, and what waits. */
export type AccountSpamTab = "rules" | "lists" | "learning" | "waiting";
const ACCOUNT_PATHS: Record<AccountSpamTab, string> = {
  rules: "/account/spam",
  lists: "/account/spam/lists",
  learning: "/account/spam/learning",
  waiting: "/account/spam/waiting",
};

const counts = (totals: BayesTotals, minimum: number) => totals.spam >= minimum && totals.ham >= minimum;

/** How far one scope of the Bayes filter got towards counting: a bar each for spam and wanted mail. */
function Progress({ totals, minimum }: { totals: BayesTotals; minimum: number }) {
  const { t } = useT();
  const bar = (label: string, value: number) => (
    <div className="flex flex-col gap-1">
      <div className="flex justify-between gap-2 text-[13px]">
        <span>{label}</span>
        <span className="text-muted tabular-nums">
          {value < minimum ? t("spam.bayes.countLearning", { value, minimum }) : t("spam.bayes.countDone", { value })}
        </span>
      </div>
      <div className="h-2.5 overflow-hidden rounded-full bg-canvas" aria-hidden>
        <div
          className="h-full rounded-full bg-pink"
          style={{ width: `${Math.min(100, (value / Math.max(minimum, 1)) * 100)}%` }}
        />
      </div>
    </div>
  );
  return (
    <div className="grid gap-3 sm:grid-cols-2">
      {bar(t("spam.bayes.spam"), totals.spam)}
      {bar(t("spam.bayes.ham"), totals.ham)}
    </div>
  );
}

function useLearn(path: string, key: readonly string[]) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => api<LearnedFromFolders>(path, { method: "POST" }),
    onSuccess: (learned) => {
      toast(
        learned.spam + learned.ham > 0
          ? t("spam.bayes.learnQueued", { spam: learned.spam, ham: learned.ham })
          : t("spam.bayes.learnNothing"),
        learned.spam + learned.ham > 0 ? "success" : "info",
      );
      void queryClient.invalidateQueries({ queryKey: key });
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}

/** A number that leads somewhere: the overview's tiles open the rules tab already filtered. */
function Tile({
  to,
  icon: Icon,
  label,
  value,
  tone,
}: {
  to: string;
  icon: typeof Ban;
  label: string;
  value: number | undefined;
  tone: "danger" | "success" | "warning" | "muted";
}) {
  const { i18n } = useT();
  const tones = {
    danger: "bg-danger-tint text-danger",
    success: "bg-success-tint text-success",
    warning: "bg-warning-tint text-warning",
    muted: "bg-elevated text-muted",
  };
  return (
    <Link
      to={to}
      className="group flex items-center gap-3 rounded-[14px] border border-hairline bg-surface p-3.5 transition-colors hover:border-pink/40"
    >
      <span className={clsx("flex size-9 shrink-0 items-center justify-center rounded-full", tones[tone])}>
        <Icon className="size-4" aria-hidden />
      </span>
      <span className="min-w-0 flex-1">
        <span className="block text-xl font-bold tabular-nums">
          {value === undefined ? "–" : formatNumber(value, i18n.language)}
        </span>
        <span className="block text-[12px] leading-tight text-muted">{label}</span>
      </span>
      <ChevronRight className="size-4 text-faint group-hover:text-pink-ink" aria-hidden />
    </Link>
  );
}

function rulesQuery(params: string) {
  return {
    queryKey: ["admin", "spam", "rules", params],
    queryFn: () => api<RulesView>(`/api/admin/spam/rules?${params}`),
  };
}

/** Block or allow a sender, or count a word, in one line: the overview's shortcut into the rules. */
function QuickRule() {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [value, setValue] = useState("");
  const [scope, setScope] = useState("server");
  const add = useMutation({
    mutationFn: (list: "allow" | "block" | "points") =>
      api<Rule>("/api/admin/spam/rules", {
        method: "POST",
        body: list === "points" ? { type: "word", value, scope } : { type: "sender", list, value, scope },
      }),
    onSuccess: (rule) => {
      toast(t("spam.rules.added", { value: rule.value }), "success");
      setValue("");
      void queryClient.invalidateQueries({ queryKey: ["admin", "spam"] });
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    add.mutate("block");
  };
  const guessed = value.trim() ? guessSenderKind(value) : null;
  return (
    <Card title={t("spam.overview.quickTitle")}>
      <form className="flex flex-col gap-3" onSubmit={submit}>
        <p className="-mt-1 text-[13px] text-muted">{t("spam.overview.quickIntro")}</p>
        <div className="grid gap-3 md:grid-cols-[minmax(0,1fr)_minmax(0,16rem)]">
          <Field
            label={t("spam.senders.value")}
            hint={
              guessed
                ? t("spam.senders.guessed", { kind: t(`spam.senders.kinds.${guessed}`) })
                : t("spam.overview.quickHint")
            }
          >
            {(id) => (
              <TextInput
                id={id}
                className="font-mono"
                autoCapitalize="none"
                spellCheck={false}
                placeholder={t("spam.senders.valuePlaceholder")}
                value={value}
                onChange={(event) => setValue(event.target.value)}
              />
            )}
          </Field>
          <Field label={t("spam.senders.scope")}>
            {() => <ScopePicker value={scope} onChange={setScope} label={t("spam.senders.scope")} />}
          </Field>
        </div>
        <div className="flex flex-wrap gap-2">
          <Button type="submit" variant="primary" icon={Ban} busy={add.isPending} disabled={!value.trim()}>
            {t("spam.rules.lists.block")}
          </Button>
          <Button icon={Check} disabled={!value.trim() || add.isPending} onClick={() => add.mutate("allow")}>
            {t("spam.rules.lists.allow")}
          </Button>
          <Button
            variant="ghost"
            icon={Hash}
            disabled={!value.trim() || add.isPending}
            onClick={() => add.mutate("points")}
          >
            {t("spam.overview.quickWord")}
          </Button>
        </div>
      </form>
    </Card>
  );
}

function Overview({ view, settings }: { view: AdminSpamView; settings: SettingsView }) {
  const { t, i18n } = useT();
  const all = useQuery(rulesQuery("perPage=25"));
  const temporary = useQuery(rulesQuery("perPage=25&state=temporary"));
  const unused = useQuery(rulesQuery("perPage=25&state=stale"));
  const top = useQuery(rulesQuery("perPage=25&sort=hits&desc=true"));
  const value = (key: string) => settings.settings.find((setting) => setting.key === key)?.value;
  const enabled = Boolean(value("spam.enabled"));
  const rulesPath = PATHS.rules;
  const busiest = (top.data?.rules ?? []).filter((rule) => rule.hits > 0).slice(0, 5);
  const state = (on: boolean, label: string, detail?: string) => (
    <li className="flex items-center gap-2 text-[13px]">
      <span className={clsx("size-2 shrink-0 rounded-full", on ? "bg-success" : "bg-faint")} aria-hidden />
      <span className="font-semibold">{label}</span>
      <span className="text-muted">{detail ?? (on ? t("spam.overview.on") : t("spam.overview.off"))}</span>
    </li>
  );

  return (
    <>
      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-5">
        <Tile
          to={rulesPath}
          icon={ListFilter}
          label={t("spam.overview.tiles.all")}
          value={all.data?.total}
          tone="muted"
        />
        <Tile
          to={rulesLink(rulesPath, { list: "block" })}
          icon={Ban}
          label={t("spam.overview.tiles.block")}
          value={all.data?.lists.block ?? 0}
          tone="danger"
        />
        <Tile
          to={rulesLink(rulesPath, { list: "allow" })}
          icon={Check}
          label={t("spam.overview.tiles.allow")}
          value={all.data?.lists.allow ?? 0}
          tone="success"
        />
        <Tile
          to={rulesLink(rulesPath, { state: "temporary", sort: "expires" })}
          icon={Clock}
          label={t("spam.overview.tiles.temporary")}
          value={temporary.data?.total}
          tone="warning"
        />
        <Tile
          to={rulesLink(rulesPath, { state: "stale" })}
          icon={Sparkles}
          label={t("spam.overview.tiles.stale")}
          value={unused.data?.total}
          tone="muted"
        />
      </div>

      <QuickRule />

      <div className="grid items-start gap-5 lg:grid-cols-2">
        <Card
          title={t("spam.overview.busiestTitle")}
          action={
            <Link
              to={rulesLink(rulesPath, { sort: "hits", desc: true })}
              className="text-[13px] font-semibold text-pink-ink hover:underline"
            >
              {t("spam.overview.more")}
            </Link>
          }
        >
          {top.isPending ? (
            <Loading />
          ) : busiest.length === 0 ? (
            <p className="rounded-control bg-canvas px-3 py-2.5 text-[13px] text-muted">
              {t("spam.overview.busiestEmpty")}
            </p>
          ) : (
            <ul className="flex flex-col">
              {busiest.map((rule) => (
                <li
                  key={`${rule.type}:${rule.id}`}
                  className="flex items-center gap-3 border-b border-hairline py-2 last:border-b-0"
                >
                  <span className="min-w-0 flex-1">
                    <code className="block truncate text-[13px] font-semibold">{rule.value}</code>
                    <span className="block truncate text-[12px] text-muted">
                      {t(`spam.rules.effect.${rule.list}`)}
                      {rule.lastHitAt ? ` · ${formatRelative(rule.lastHitAt, i18n.language)}` : ""}
                    </span>
                  </span>
                  <span className="text-sm font-bold tabular-nums">{formatNumber(rule.hits, i18n.language)}×</span>
                </li>
              ))}
            </ul>
          )}
        </Card>
        <div className="flex flex-col gap-5">
          <Card
            title={t("spam.overview.stateTitle")}
            action={
              <Link to={PATHS.settings} className="text-[13px] font-semibold text-pink-ink hover:underline">
                {t("spam.overview.change")}
              </Link>
            }
          >
            <ul className="flex flex-col gap-2">
              {state(enabled, t("settings.spam.enabled"))}
              {enabled && state(Boolean(value("spam.blocklists")), t("settings.spam.blocklists"))}
              {enabled && state(Boolean(value("spam.uri_blocklists")), t("settings.spam.uriBlocklists"))}
              {enabled && state(Boolean(value("spam.bayes")), t("settings.spam.bayes"))}
              {enabled &&
                state(
                  true,
                  t("spam.overview.thresholds"),
                  t("spam.overview.thresholdsDetail", {
                    junk: formatNumber(Number(value("spam.junk_score") ?? 0), i18n.language),
                    greylist: formatNumber(Number(value("spam.greylist_score") ?? 0), i18n.language),
                  }),
                )}
              {state(Boolean(value("spam.antivirus.enabled")), t("spam.admin.tabs.antivirus"))}
              {state(Boolean(value("spam.log.enabled")), t("spam.admin.tabs.history"))}
            </ul>
          </Card>
          <Card
            title={t("spam.bayes.title")}
            action={
              <Link to={PATHS.learning} className="text-[13px] font-semibold text-pink-ink hover:underline">
                {t("spam.overview.more")}
              </Link>
            }
          >
            <div className="flex flex-col gap-3">
              {!view.bayes.enabled && (
                <p className="rounded-control bg-canvas px-3 py-2 text-[13px]">{t("spam.bayes.off")}</p>
              )}
              <Progress totals={view.bayes.server} minimum={view.bayes.minimum} />
              <p className="text-[13px] text-muted">
                {counts(view.bayes.server, view.bayes.minimum)
                  ? t("spam.bayes.serverCounts")
                  : t("spam.bayes.serverLearning", { minimum: view.bayes.minimum })}
              </p>
            </div>
          </Card>
        </div>
      </div>

      <p className="flex items-start gap-2 rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
        <BookOpen className="mt-0.5 size-4 shrink-0" aria-hidden />
        {t("spam.overview.howItWorks")}
      </p>
    </>
  );
}

function Frame({ header, tabs, children }: { header: ReactNode; tabs: ReactNode; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-5">
      {header}
      {tabs}
      {children}
    </div>
  );
}

/** The spam filter in My account: one's own rules and lists, what it learned, and what waits. */
export function AccountSpamPage({ tab = "rules" }: { tab?: AccountSpamTab }) {
  const { t } = useT();
  const query = useQuery({
    queryKey: accountKey,
    queryFn: () => api<AccountSpamView>("/api/account/spam"),
    enabled: tab === "learning",
  });
  // On every tab, so the number of waiting messages shows without opening that one.
  const greylist = useQuery({ queryKey: greylistKey, queryFn: () => api<GreylistView>("/api/account/greylist") });
  const learn = useLearn("/api/account/spam/learn-folders", accountKey);
  const waiting = greylist.data?.count ?? 0;

  const tabs = (
    <Tabs<AccountSpamTab>
      label={t("spam.account.tab")}
      value={tab}
      tabs={(Object.keys(ACCOUNT_PATHS) as AccountSpamTab[]).map((value) => ({
        value,
        to: ACCOUNT_PATHS[value],
        icon: ICONS[value],
        label: t(`spam.account.tabs.${value}`),
        count: value === "waiting" ? waiting : undefined,
      }))}
    />
  );
  const header = <PageHeader title={t("spam.account.title")} intro={t("spam.account.intro")} />;

  if (tab === "waiting") {
    return (
      <Frame header={header} tabs={tabs}>
        <GreylistCard />
      </Frame>
    );
  }
  if (tab === "rules") {
    return (
      <Frame header={header} tabs={tabs}>
        <RulesPanel admin={false} />
      </Frame>
    );
  }
  if (tab === "lists") {
    return (
      <Frame header={header} tabs={tabs}>
        <SubscribedListsCard admin={false} />
      </Frame>
    );
  }

  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  const { bayes, limits } = query.data;
  return (
    <Frame header={header} tabs={tabs}>
      <SpamLimitsCard limits={limits} queryKey={accountKey} />
      <Card title={t("spam.bayes.title")}>
        <div className="flex flex-col gap-4">
          <p className="-mt-1 text-[13px] text-muted">{t("spam.bayes.explain")}</p>
          {!bayes.enabled && <p className="rounded-control bg-canvas px-3 py-2 text-[13px]">{t("spam.bayes.off")}</p>}
          <section className="flex flex-col gap-2">
            <h3 className="text-sm font-semibold">{t("spam.bayes.own")}</h3>
            <Progress totals={bayes.own} minimum={bayes.minimum} />
            <p className="text-[13px] text-muted">
              {counts(bayes.own, bayes.minimum)
                ? t("spam.bayes.ownCounts")
                : t("spam.bayes.ownLearning", { minimum: bayes.minimum })}
            </p>
          </section>
          <section className="flex flex-col gap-2">
            <h3 className="text-sm font-semibold">{t("spam.bayes.server")}</h3>
            <Progress totals={bayes.server} minimum={bayes.minimum} />
          </section>
          <div className="flex flex-wrap items-center justify-between gap-3">
            <p className="max-w-prose text-[13px] text-muted">{t("spam.bayes.learnHint")}</p>
            <Button icon={GraduationCap} busy={learn.isPending} onClick={() => learn.mutate()}>
              {t("spam.bayes.learnOwn")}
            </Button>
          </div>
        </div>
      </Card>
    </Frame>
  );
}

/** The spam filter for admins: an overview, the rules, the settings and everything behind them. */
export function AdminSpamPage({ tab = "overview" }: { tab?: SpamTab }) {
  const { t } = useT();
  const queryClient = useQueryClient();
  const query = useQuery({ queryKey: adminKey, queryFn: () => api<AdminSpamView>("/api/admin/spam") });
  const settings = useQuery({
    queryKey: ["admin", "settings"],
    queryFn: () => api<SettingsView>("/api/admin/settings"),
  });
  const learn = useLearn("/api/admin/spam/learn-folders", adminKey);

  const tabs = (
    <Tabs<SpamTab>
      label={t("spam.admin.tab")}
      value={tab}
      tabs={(Object.keys(PATHS) as SpamTab[]).map((value) => ({
        value,
        to: PATHS[value],
        icon: ICONS[value],
        label: t(`spam.admin.tabs.${value}`),
      }))}
    />
  );
  const header = <PageHeader title={t("spam.admin.title")} intro={t("spam.admin.intro")} />;

  // The rules load on their own, so the table is there before anything else is.
  if (tab === "rules") {
    return (
      <Frame header={header} tabs={tabs}>
        <RulesPanel admin />
      </Frame>
    );
  }
  if (query.isPending || settings.isPending) {
    return (
      <Frame header={header} tabs={tabs}>
        <Loading />
      </Frame>
    );
  }
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  if (settings.isError) return <LoadError error={settings.error} onRetry={() => void settings.refetch()} />;
  const view = query.data;

  return (
    <Frame header={header} tabs={tabs}>
      {tab === "overview" && <Overview view={view} settings={settings.data} />}
      {tab === "settings" && (
        <Section
          title={t("spam.admin.settingsTitle")}
          intro={t("settings.spam.intro")}
          view={settings.data}
          keys={SPAM_SETTING_KEYS}
          onSaved={() => void queryClient.invalidateQueries({ queryKey: adminKey })}
        >
          {(form) => <SpamFields form={form} />}
        </Section>
      )}
      {tab === "lists" && (
        <>
          <FeedsCard view={settings.data} />
          <SubscribedListsCard admin />
        </>
      )}
      {tab === "learning" && (
        <>
          <Card title={t("spam.bayes.title")}>
            <div className="flex flex-col gap-4">
              <p className="-mt-1 text-[13px] text-muted">{t("spam.bayes.explainAdmin")}</p>
              {!view.bayes.enabled && (
                <p className="rounded-control bg-canvas px-3 py-2 text-[13px]">{t("spam.bayes.off")}</p>
              )}
              <Progress totals={view.bayes.server} minimum={view.bayes.minimum} />
              <p className="text-[13px] text-muted">
                {counts(view.bayes.server, view.bayes.minimum)
                  ? t("spam.bayes.serverCounts")
                  : t("spam.bayes.serverLearning", { minimum: view.bayes.minimum })}
                {view.bayes.queued > 0 && ` ${t("spam.bayes.queued", { count: view.bayes.queued })}`}
              </p>
              <div className="flex flex-wrap items-center justify-between gap-3">
                <p className="max-w-prose text-[13px] text-muted">{t("spam.bayes.learnHintAdmin")}</p>
                <Button icon={GraduationCap} busy={learn.isPending} onClick={() => learn.mutate()}>
                  {t("spam.bayes.learnAll")}
                </Button>
              </div>
            </div>
          </Card>
          {view.fetched && <FetchedCard verdicts={view.fetched} days={view.fetchedDays} />}
        </>
      )}
      {tab === "antivirus" && (
        <>
          <Section
            title={t("spam.antivirus.settingsTitle")}
            intro={t("spam.antivirus.settingsIntro")}
            view={settings.data}
            keys={ANTIVIRUS_SETTING_KEYS}
            onSaved={() => void queryClient.invalidateQueries({ queryKey: ["admin", "spam", "antivirus"] })}
          >
            {(form) => <AntivirusFields form={form} />}
          </Section>
          <AntivirusCard />
        </>
      )}
      {tab === "history" && (
        <>
          <Section
            title={t("spam.log.settingsTitle")}
            intro={t("spam.log.settingsIntro")}
            view={settings.data}
            keys={SPAM_LOG_SETTING_KEYS}
            onSaved={() => void queryClient.invalidateQueries({ queryKey: ["admin", "spam", "log"] })}
          >
            {(form) => <SpamLogFields form={form} />}
          </Section>
          <SpamLogCard />
        </>
      )}
    </Frame>
  );
}

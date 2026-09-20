import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { GraduationCap } from "lucide-react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card, PageHeader } from "@/components/ui/Card";
import { Segmented } from "@/components/ui/Field";
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
  type LearnedFromFolders,
  type GreylistView,
  type SettingsView,
} from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { navigate } from "@/lib/router";
import { toast } from "@/state/toasts";
import { AntivirusCard } from "./AntivirusCard";
import { FetchedCard } from "./FetchedCard";
import { FeedsCard } from "./FeedsCard";
import { SenderListCard } from "./SenderListCard";
import { SpamLimitsCard } from "./SpamLimitsCard";
import { GreylistCard, greylistKey } from "./GreylistCard";
import { SpamLogCard } from "./SpamLogCard";
import { WordListCard } from "./WordListCard";

const accountKey = ["account", "spam"] as const;
const adminKey = ["admin", "spam"] as const;

/** The settings of the filter, the virus scanner, and what was decided message by message. */
export type SpamTab = "filter" | "antivirus" | "history";
const TABS: SpamTab[] = ["filter", "antivirus", "history"];

/** In My account: one's own filter, and what greylisting is currently holding back. */
export type AccountSpamTab = "filter" | "waiting";
const ACCOUNT_TABS: AccountSpamTab[] = ["filter", "waiting"];
const ACCOUNT_PATHS: Record<AccountSpamTab, string> = {
  filter: "/account/spam",
  waiting: "/account/spam/waiting",
};

/** Every tab has its own address, so the health overview can link straight to the one it means. */
const PATHS: Record<SpamTab, string> = {
  filter: "/admin/spam",
  antivirus: "/admin/spam/antivirus",
  history: "/admin/spam/history",
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

/** The spam filter in My account: one's own sender list and what the filter learned from one's marks. */
export function AccountSpamPage({ tab = "filter" }: { tab?: AccountSpamTab }) {
  const { t } = useT();
  const query = useQuery({ queryKey: accountKey, queryFn: () => api<AccountSpamView>("/api/account/spam") });
  // Also on the filter tab, so the number of waiting messages shows without opening the other one.
  const greylist = useQuery({ queryKey: greylistKey, queryFn: () => api<GreylistView>("/api/account/greylist") });
  const learn = useLearn("/api/account/spam/learn-folders", accountKey);
  const setTab = (value: string) => navigate(ACCOUNT_PATHS[value as AccountSpamTab] ?? ACCOUNT_PATHS.filter);

  const waiting = greylist.data?.count ?? 0;
  const tabs = (
    <Segmented<string>
      label={t("spam.account.tab")}
      value={tab}
      onChange={setTab}
      options={ACCOUNT_TABS.map((value) => ({
        value,
        label:
          value === "waiting" && waiting > 0
            ? t("spam.account.tabs.waitingCount", { count: waiting })
            : t(`spam.account.tabs.${value}`),
      }))}
    />
  );

  if (tab === "waiting") {
    return (
      <div className="flex flex-col gap-5">
        <PageHeader title={t("spam.account.title")} intro={t("spam.account.intro")} />
        {tabs}
        <GreylistCard />
      </div>
    );
  }

  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  const { bayes, limits } = query.data;

  return (
    <div className="flex flex-col gap-5">
      <PageHeader title={t("spam.account.title")} intro={t("spam.account.intro")} />
      {tabs}
      <SenderListCard admin={false} />
      <WordListCard admin={false} />
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
    </div>
  );
}

/** The spam filter for admins: its settings, the server and domain sender lists, and what it learned. */
export function AdminSpamPage({ tab = "filter" }: { tab?: SpamTab }) {
  const { t } = useT();
  const queryClient = useQueryClient();
  const setTab = (value: string) => navigate(PATHS[value as SpamTab] ?? PATHS.filter);
  const query = useQuery({ queryKey: adminKey, queryFn: () => api<AdminSpamView>("/api/admin/spam") });
  const settings = useQuery({
    queryKey: ["admin", "settings"],
    queryFn: () => api<SettingsView>("/api/admin/settings"),
  });
  const learn = useLearn("/api/admin/spam/learn-folders", adminKey);
  if (query.isPending || settings.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  if (settings.isError) return <LoadError error={settings.error} onRetry={() => void settings.refetch()} />;
  const { bayes, fetched, fetchedDays } = query.data;

  const tabs = (
    <Segmented<string>
      label={t("spam.admin.tab")}
      value={tab}
      onChange={setTab}
      options={TABS.map((value) => ({ value, label: t(`spam.admin.tabs.${value}`) }))}
    />
  );

  if (tab === "antivirus") {
    return (
      <div className="flex flex-col gap-5">
        <PageHeader title={t("spam.admin.title")} intro={t("spam.admin.intro")} />
        {tabs}
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
      </div>
    );
  }

  if (tab === "history") {
    return (
      <div className="flex flex-col gap-5">
        <PageHeader title={t("spam.admin.title")} intro={t("spam.admin.intro")} />
        {tabs}
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
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-5">
      <PageHeader title={t("spam.admin.title")} intro={t("spam.admin.intro")} />
      {tabs}
      <Section
        title={t("spam.admin.settingsTitle")}
        intro={t("settings.spam.intro")}
        view={settings.data}
        keys={SPAM_SETTING_KEYS}
        onSaved={() => void queryClient.invalidateQueries({ queryKey: adminKey })}
      >
        {(form) => <SpamFields form={form} />}
      </Section>
      {fetched && <FetchedCard verdicts={fetched} days={fetchedDays} />}
      <FeedsCard view={settings.data} />
      <SenderListCard admin />
      <WordListCard admin />
      <Card title={t("spam.bayes.title")}>
        <div className="flex flex-col gap-4">
          <p className="-mt-1 text-[13px] text-muted">{t("spam.bayes.explainAdmin")}</p>
          {!bayes.enabled && <p className="rounded-control bg-canvas px-3 py-2 text-[13px]">{t("spam.bayes.off")}</p>}
          <Progress totals={bayes.server} minimum={bayes.minimum} />
          <p className="text-[13px] text-muted">
            {counts(bayes.server, bayes.minimum)
              ? t("spam.bayes.serverCounts")
              : t("spam.bayes.serverLearning", { minimum: bayes.minimum })}
            {bayes.queued > 0 && ` ${t("spam.bayes.queued", { count: bayes.queued })}`}
          </p>
          <div className="flex flex-wrap items-center justify-between gap-3">
            <p className="max-w-prose text-[13px] text-muted">{t("spam.bayes.learnHintAdmin")}</p>
            <Button icon={GraduationCap} busy={learn.isPending} onClick={() => learn.mutate()}>
              {t("spam.bayes.learnAll")}
            </Button>
          </div>
        </div>
      </Card>
    </div>
  );
}

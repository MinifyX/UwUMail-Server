import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { GraduationCap } from "lucide-react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card, PageHeader } from "@/components/ui/Card";
import { SPAM_SETTING_KEYS, Section, SpamFields } from "@/features/settings/SettingsPage";
import { useT } from "@/i18n";
import {
  api,
  type AccountSpamView,
  type AdminSpamView,
  type BayesTotals,
  type LearnedFromFolders,
  type SettingsView,
} from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { usePrefs } from "@/state/prefs";
import { toast } from "@/state/toasts";
import { FeedsCard } from "./FeedsCard";
import { SenderListCard } from "./SenderListCard";
import { WordListCard } from "./WordListCard";

const accountKey = ["account", "spam"] as const;
const adminKey = ["admin", "spam"] as const;

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
export function AccountSpamPage() {
  const { t } = useT();
  const query = useQuery({ queryKey: accountKey, queryFn: () => api<AccountSpamView>("/api/account/spam") });
  const learn = useLearn("/api/account/spam/learn-folders", accountKey);
  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  const { bayes } = query.data;

  return (
    <div className="flex flex-col gap-5">
      <PageHeader title={t("spam.account.title")} intro={t("spam.account.intro")} />
      <SenderListCard admin={false} />
      <WordListCard admin={false} />
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
export function AdminSpamPage() {
  const { t } = useT();
  const pro = usePrefs((s) => s.mode) === "pro";
  const queryClient = useQueryClient();
  const query = useQuery({ queryKey: adminKey, queryFn: () => api<AdminSpamView>("/api/admin/spam") });
  const settings = useQuery({
    queryKey: ["admin", "settings"],
    queryFn: () => api<SettingsView>("/api/admin/settings"),
  });
  const learn = useLearn("/api/admin/spam/learn-folders", adminKey);
  if (query.isPending || settings.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  if (settings.isError) return <LoadError error={settings.error} onRetry={() => void settings.refetch()} />;
  const { bayes } = query.data;

  return (
    <div className="flex flex-col gap-5">
      <PageHeader title={t("spam.admin.title")} intro={t("spam.admin.intro")} />
      <Section
        title={t("spam.admin.settingsTitle")}
        intro={t("settings.spam.intro")}
        view={settings.data}
        keys={SPAM_SETTING_KEYS}
        onSaved={() => void queryClient.invalidateQueries({ queryKey: adminKey })}
      >
        {(form) => <SpamFields form={form} pro={pro} />}
      </Section>
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

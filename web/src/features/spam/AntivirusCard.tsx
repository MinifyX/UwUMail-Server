import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import clsx from "clsx";
import { CircleCheck, CircleHelp, CircleX, ShieldCheck, TriangleAlert } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { useT } from "@/i18n";
import { api, type AntivirusTest, type AntivirusView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDateTime } from "@/lib/format";
import { toast } from "@/state/toasts";

const key = ["admin", "spam", "antivirus"] as const;

type Tone = "ok" | "warning" | "problem" | "off";

const TONES: Record<Tone, { icon: LucideIcon; className: string }> = {
  ok: { icon: CircleCheck, className: "bg-success-tint text-success" },
  warning: { icon: TriangleAlert, className: "bg-warning-tint text-warning" },
  problem: { icon: CircleX, className: "bg-danger-tint text-danger" },
  off: { icon: CircleHelp, className: "bg-elevated text-muted" },
};

function Pill({ tone, children }: { tone: Tone; children: string }) {
  const { icon: Icon, className } = TONES[tone];
  return (
    <span
      className={clsx("inline-flex h-6 items-center gap-1 rounded-full px-2.5 text-[12px] font-semibold", className)}
    >
      <Icon className="size-3.5" aria-hidden />
      {children}
    </span>
  );
}

/** Whether the scanner is there, what it is, and what it turned away lately. */
export function AntivirusCard({ explain }: { explain: boolean }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const query = useQuery({ queryKey: key, queryFn: () => api<AntivirusView>("/api/admin/spam/antivirus") });
  const test = useMutation({
    mutationFn: () => api<AntivirusTest>("/api/admin/spam/antivirus/test", { method: "POST" }),
    onSuccess: (result) => {
      toast(
        result.found ? t("spam.antivirus.testFound", { name: result.found }) : t("spam.antivirus.testFailed"),
        result.found ? "success" : "error",
      );
      void queryClient.invalidateQueries({ queryKey: key });
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  const view = query.data;
  const old = view.signaturesOld;
  const tone: Tone = !view.enabled ? "off" : view.error ? "problem" : old ? "warning" : "ok";

  return (
    <Card title={t("spam.antivirus.title")}>
      <div className="flex flex-col gap-4">
        {explain && <p className="-mt-1 text-[13px] text-muted">{t("spam.antivirus.explain")}</p>}
        <div className="flex flex-wrap items-center gap-2">
          <Pill tone={tone}>{t(`spam.antivirus.state.${tone}`)}</Pill>
          {view.enabled && <span className="text-[13px] text-muted">{view.address}</span>}
        </div>
        {!view.enabled && <p className="text-[13px] text-muted">{t("spam.antivirus.off")}</p>}
        {view.error && (
          <p className="rounded-control bg-danger-tint px-3 py-2 text-[13px] text-danger">
            {t("spam.antivirus.away", { error: view.error })}
          </p>
        )}
        {view.status && (
          <dl className="grid gap-2 text-[13px] sm:grid-cols-[160px_1fr]">
            <dt className="font-semibold text-muted">{t("spam.antivirus.version")}</dt>
            <dd className="break-all">{view.status.version}</dd>
            {view.status.signatures != null && (
              <>
                <dt className="font-semibold text-muted">{t("spam.antivirus.signatures")}</dt>
                <dd className={clsx(old && "text-warning")}>
                  {view.status.signatures}
                  {view.status.signaturesAt != null && ` · ${formatDateTime(view.status.signaturesAt, i18n.language)}`}
                </dd>
              </>
            )}
          </dl>
        )}
        <p className="text-[13px]">{t("spam.antivirus.found", { count: view.found, days: view.days })}</p>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <p className="max-w-prose text-[13px] text-muted">{t("spam.antivirus.testHint")}</p>
          <Button icon={ShieldCheck} busy={test.isPending} disabled={!view.enabled} onClick={() => test.mutate()}>
            {t("spam.antivirus.test")}
          </Button>
        </div>
        {test.data?.error && (
          <p className="rounded-control bg-danger-tint px-3 py-2 text-[13px] text-danger">{test.data.error}</p>
        )}
      </div>
    </Card>
  );
}

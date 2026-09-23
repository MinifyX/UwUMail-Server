import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import clsx from "clsx";
import { CircleCheck, CircleHelp, Globe, TriangleAlert } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { useT } from "@/i18n";
import { api, type EgressTest, type EgressView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatRelative } from "@/lib/format";

const key = ["admin", "egress"] as const;

/** A proxy failure this recent still counts as trouble. */
const RECENT_SECONDS = 15 * 60;

type Tone = "proxy" | "trouble" | "direct";

const TONES: Record<Tone, { icon: LucideIcon; className: string }> = {
  proxy: { icon: CircleCheck, className: "bg-success-tint text-success" },
  trouble: { icon: TriangleAlert, className: "bg-warning-tint text-warning" },
  direct: { icon: CircleHelp, className: "bg-elevated text-muted" },
};

function State({ tone, children }: { tone: Tone; children: string }) {
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

/**
 * How a mail's remote pictures leave the server: straight, or through a VPN's proxy. Only shown;
 * the proxy is set in the configuration, because its address often holds a password.
 */
export function EgressCard() {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const query = useQuery({ queryKey: key, queryFn: () => api<EgressView>("/api/admin/egress") });
  const test = useMutation({
    mutationFn: () => api<EgressTest>("/api/admin/egress/test", { method: "POST" }),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: key }),
  });

  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  const view = query.data;
  const failure = view.lastProxyFailure;
  // Measured from when the status came, which is as fresh as its numbers.
  const recent = failure !== null && failure.at > query.dataUpdatedAt / 1000 - RECENT_SECONDS;
  const tone: Tone = view.proxy === null ? "direct" : recent ? "trouble" : "proxy";

  return (
    <Card title={t("settings.egress.title")}>
      <div className="flex flex-col gap-4">
        <p className="max-w-prose text-[13px] text-muted">{t("settings.egress.intro")}</p>
        <div className="flex flex-wrap items-center gap-2">
          <State tone={tone}>{t(`settings.egress.state.${tone}`)}</State>
        </div>
        {view.proxy === null ? (
          <p className="text-[13px] text-muted">{t("settings.egress.directHint")}</p>
        ) : (
          <dl className="grid gap-2 text-[13px] sm:grid-cols-[180px_1fr]">
            <dt className="font-semibold text-muted">{t("settings.egress.proxy")}</dt>
            <dd className="font-mono break-all">{view.proxy}</dd>
            <dt className="font-semibold text-muted">{t("settings.egress.fallback")}</dt>
            <dd>{t(`settings.egress.fallbackOptions.${view.fallback}`)}</dd>
          </dl>
        )}
        <p className="text-[13px]">{t("settings.egress.counts", { fetched: view.fetched, failed: view.failed })}</p>
        {failure && (
          <p
            className={clsx(
              "rounded-control px-3 py-2 text-[13px]",
              recent ? "bg-warning-tint text-warning" : "bg-canvas text-muted",
            )}
          >
            {t("settings.egress.proxyFailed", {
              times: view.proxyFailures,
              when: formatRelative(failure.at, i18n.language),
              error: failure.error,
            })}
            {view.fallbacks > 0 && ` ${t("settings.egress.fallbacks", { times: view.fallbacks })}`}
          </p>
        )}
        <p className="text-[13px] text-muted">{t("settings.egress.configHint")}</p>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <p className="max-w-prose text-[13px] text-muted">{t("settings.egress.testHint")}</p>
          <Button icon={Globe} busy={test.isPending} onClick={() => test.mutate()}>
            {t("settings.egress.test")}
          </Button>
        </div>
        {test.isError && (
          <p className="rounded-control bg-danger-tint px-3 py-2 text-[13px] text-danger">{errorText(test.error)}</p>
        )}
        {test.data?.address && (
          <p className="rounded-control bg-success-tint px-3 py-2 text-[13px] text-success">
            {t(test.data.proxied ? "settings.egress.testProxied" : "settings.egress.testDirect", {
              address: test.data.address,
            })}
          </p>
        )}
        {test.data?.error && (
          <p className="rounded-control bg-danger-tint px-3 py-2 text-[13px] text-danger">
            {t("settings.egress.testFailed", { error: test.data.error })}
          </p>
        )}
      </div>
    </Card>
  );
}

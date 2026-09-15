import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import clsx from "clsx";
import { ChevronRight, CircleCheck, CircleHelp, CircleX, RotateCw, TriangleAlert } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { NyuMood } from "@/components/nyu/Nyu";
import { Button } from "@/components/ui/Button";
import { LogoSymbol } from "@/components/ui/Logo";
import { useT } from "@/i18n";
import { api, type Health, type HealthArea, type HealthFinding, type HealthLevel } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatBytes, formatDate, formatDuration, formatRelative } from "@/lib/format";
import { Link } from "@/lib/router";
import { usePrefs } from "@/state/prefs";
import { toast } from "@/state/toasts";

const LEVELS: Record<HealthLevel, { icon: LucideIcon; tint: string; dot: string; mood: NyuMood }> = {
  ok: { icon: CircleCheck, tint: "bg-success-tint text-success", dot: "bg-success", mood: "happy" },
  unknown: { icon: CircleHelp, tint: "bg-elevated text-muted", dot: "bg-faint", mood: "puzzled" },
  warning: { icon: TriangleAlert, tint: "bg-warning-tint text-warning", dot: "bg-warning", mood: "puzzled" },
  problem: { icon: CircleX, tint: "bg-danger-tint text-danger", dot: "bg-danger", mood: "sad" },
};

/** Codes with an extra explanation in Simple mode. */
const HINTS = new Set([
  "noDomains",
  "dnsDomain",
  "certWaiting",
  "certSelfSigned",
  "certWrongName",
  "certExpired",
  "certExpiresSoon",
  "relayLogin",
  "relayTls",
  "relayUnreachable",
  "outboundBlocked",
  "port25Blocked",
  "gatewayDown",
  "gatewayRefused",
  "gatewayPort25Blocked",
  "gatewayOutboundBlocked",
  "queueStuck",
  "manyBounces",
  "diskLow",
  "mailboxesNearlyFull",
  "adminsWithoutSecondFactor",
  "youWithoutSecondFactor",
  "tlsFailures",
  "dmarcOwnFailures",
]);

function useFindingText() {
  const { t, i18n } = useT();
  const language = i18n.language;
  return (finding: HealthFinding) => {
    const p = finding.params ?? {};
    const num = (key: string) => (typeof p[key] === "number" ? (p[key] as number) : 0);
    const values: Record<string, unknown> = { ...p };
    switch (finding.code) {
      case "dnsDomain":
        values.statusText = t(`domains.dnsStatus.${String(p.status)}`);
        break;
      case "certOk":
      case "certOkAutomatic":
        values.date = formatDate(num("notAfter"), language);
        break;
      case "certExpiresSoon":
      case "certExpired":
        values.count = Math.max(num("days"), 0);
        break;
      case "queueStuck":
        values.since = formatDuration(num("ageSecs"), t);
        break;
      case "gatewayConnected":
        values.addresses = Array.isArray(p.addresses) ? (p.addresses as string[]).join(", ") : "";
        break;
      case "gatewayDown":
        values.since = formatDuration(Math.max(Math.floor(Date.now() / 1000) - num("downSince"), 0), t);
        break;
      case "diskOk":
      case "diskLow":
        values.free = formatBytes(num("freeBytes"), language);
        values.total = formatBytes(num("totalBytes"), language);
        break;
    }
    return t(`health.findings.${finding.code}`, values);
  };
}

function Finding({ finding, simple }: { finding: HealthFinding; simple: boolean }) {
  const { t, i18n } = useT();
  const text = useFindingText();
  const params = finding.params ?? {};
  const detail =
    finding.code === "relayOk" || finding.code === "directOk"
      ? typeof params.lastDeliveredAt === "number"
        ? t("health.lastDelivered", { time: formatRelative(params.lastDeliveredAt, i18n.language) })
        : null
      : null;
  const showHint = simple && finding.level !== "ok" && HINTS.has(finding.code);
  const error = finding.level !== "ok" && typeof params.error === "string" ? params.error : null;
  return (
    <li className="text-sm">
      <p className={clsx(finding.level === "ok" ? "text-muted" : "text-ink")}>
        {text(finding)}
        {detail && <span className="text-muted"> {detail}</span>}
      </p>
      {showHint && <p className="mt-0.5 text-[13px] text-muted">{t(`health.hints.${finding.code}`)}</p>}
      {error && !simple && <p className="mt-0.5 font-mono text-[12px] break-all text-faint">{error}</p>}
      {finding.link && finding.level !== "ok" && (
        <Link
          to={finding.link}
          className="mt-1 inline-flex items-center gap-0.5 text-[13px] font-semibold text-pink-ink hover:underline"
        >
          {t("health.open")}
          <ChevronRight className="size-3.5" aria-hidden />
        </Link>
      )}
    </li>
  );
}

function AreaTile({ area, simple }: { area: HealthArea; simple: boolean }) {
  const { t } = useT();
  const level = LEVELS[area.level];
  return (
    <div className={clsx("rounded-control border border-hairline bg-canvas/60", simple ? "p-4" : "p-3")}>
      <h3 className="flex items-center gap-2 text-[13px] font-bold">
        <span className={clsx("size-2.5 shrink-0 rounded-full", level.dot)} aria-hidden />
        {t(`health.areas.${area.area}`)}
        <span className="sr-only">: {t(`health.level.${area.level}`)}</span>
      </h3>
      <ul className="mt-1.5 flex flex-col gap-2 pl-[18px]">
        {area.findings.map((finding, index) => (
          <Finding key={`${finding.code}-${index}`} finding={finding} simple={simple} />
        ))}
      </ul>
    </div>
  );
}

export function useHealth() {
  return useQuery({
    queryKey: ["admin", "health"],
    queryFn: () => api<Health>("/api/admin/health"),
    refetchInterval: 60_000,
  });
}

export function HealthCard() {
  const { t, i18n } = useT();
  const mode = usePrefs((s) => s.mode);
  const simple = mode === "simple";
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const health = useHealth();
  const check = useMutation({
    mutationFn: () => api<Health>("/api/admin/health/check", { method: "POST" }),
    onSuccess: (data) => queryClient.setQueryData(["admin", "health"], data),
    onError: (error) => toast(errorText(error), "error"),
  });

  if (!health.data) {
    return (
      <section className="rounded-card border border-hairline bg-surface p-5" aria-busy={health.isPending}>
        <p className="text-sm text-muted">{health.isError ? errorText(health.error) : t("health.loading")}</p>
      </section>
    );
  }

  const data = health.data;
  const level = LEVELS[data.level];
  const Icon = level.icon;
  // The headline counts what makes the light this colour, e.g. only the problems when there are any.
  const open = data.areas.flatMap((area) => area.findings).filter((f) => f.level === data.level);

  return (
    <section className="rounded-card border border-hairline bg-surface p-5" aria-labelledby="health-title">
      <header className="flex flex-wrap items-center gap-4">
        {simple ? (
          <span className={clsx("flex size-14 shrink-0 items-center justify-center rounded-full", level.tint)}>
            <LogoSymbol mood={level.mood} className="h-10 w-auto" />
          </span>
        ) : (
          <span className={clsx("flex size-9 shrink-0 items-center justify-center rounded-full", level.tint)}>
            <Icon className="size-[18px]" aria-hidden />
          </span>
        )}
        <div className="min-w-0 flex-1 basis-52">
          <h2 id="health-title" className={clsx("font-bold", simple ? "text-lg" : "text-[15px]")}>
            {data.level === "warning" || data.level === "problem"
              ? t(`health.summary.${data.level}`, { count: open.length })
              : t(`health.summary.${data.level}`)}
          </h2>
          <p className="text-[13px] text-muted">
            {data.checkedAt
              ? t("health.checkedAt", { time: formatRelative(data.checkedAt, i18n.language) })
              : t("health.notChecked")}
          </p>
        </div>
        <Button size="sm" icon={RotateCw} busy={check.isPending} onClick={() => check.mutate()}>
          {check.isPending ? t("health.checking") : t("health.checkNow")}
        </Button>
      </header>
      <div className={clsx("mt-4 grid gap-3", simple ? "md:grid-cols-2" : "sm:grid-cols-2 xl:grid-cols-4")}>
        {data.areas.map((area) => (
          <AreaTile key={area.area} area={area} simple={simple} />
        ))}
      </div>
    </section>
  );
}

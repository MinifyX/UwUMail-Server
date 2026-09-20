import { Card } from "@/components/ui/Card";
import { useT } from "@/i18n";
import type { FetchedVerdicts } from "@/lib/api";

/** One row of the comparison: a number, what it means, and how much of the whole it is. */
function Row({
  count,
  total,
  label,
  hint,
  tone,
}: {
  count: number;
  total: number;
  label: string;
  hint: string;
  tone: string;
}) {
  const share = total > 0 ? Math.round((count / total) * 100) : 0;
  return (
    <div className="flex flex-col gap-1 border-b border-hairline py-2.5 last:border-b-0">
      <div className="flex items-baseline justify-between gap-3">
        <span className="text-sm font-semibold">{label}</span>
        <span className="shrink-0 text-sm tabular-nums">
          {count} <span className="text-[12px] text-muted">({share} %)</span>
        </span>
      </div>
      <div className="h-1.5 w-full overflow-hidden rounded-full bg-canvas">
        <div className={`h-full rounded-full ${tone}`} style={{ width: `${share}%` }} />
      </div>
      <span className="text-[12px] text-muted">{hint}</span>
    </div>
  );
}

/**
 * What this server made of fetched mail, next to what the provider had thought of it.
 *
 * The two interesting rows are the ones where the verdicts differ: what the provider sorted out and
 * we let through, and what it let through and we sorted out.
 */
export function FetchedCard({ verdicts, days }: { verdicts: FetchedVerdicts; days: number }) {
  const { t } = useT();
  if (verdicts.total === 0) {
    return (
      <Card title={t("spam.fetched.title")}>
        <p className="text-[13px] text-muted">{t("spam.fetched.empty", { days })}</p>
      </Card>
    );
  }
  return (
    <Card title={t("spam.fetched.title")}>
      <div className="flex flex-col gap-3">
        <p className="-mt-1 text-[13px] text-muted">{t("spam.fetched.explain", { days, count: verdicts.total })}</p>
        <div className="flex flex-col">
          <Row
            count={verdicts.agreedJunk}
            total={verdicts.total}
            label={t("spam.fetched.agreedJunk")}
            hint={t("spam.fetched.agreedJunkHint")}
            tone="bg-success"
          />
          <Row
            count={verdicts.weCaught}
            total={verdicts.total}
            label={t("spam.fetched.weCaught")}
            hint={t("spam.fetched.weCaughtHint")}
            tone="bg-pink"
          />
          <Row
            count={verdicts.weLetThrough}
            total={verdicts.total}
            label={t("spam.fetched.weLetThrough")}
            hint={t("spam.fetched.weLetThroughHint")}
            tone="bg-warning"
          />
          <Row
            count={verdicts.agreedClean}
            total={verdicts.total}
            label={t("spam.fetched.agreedClean")}
            hint={t("spam.fetched.agreedCleanHint")}
            tone="bg-line"
          />
        </div>
        <p className="text-[12px] text-muted">{t("spam.fetched.caveat")}</p>
      </div>
    </Card>
  );
}

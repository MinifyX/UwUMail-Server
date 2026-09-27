import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Card } from "@/components/ui/Card";
import { Segmented } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type StatsRange, type StatsView } from "@/lib/api";
import { formatBytes, formatNumber } from "@/lib/format";
import { BarChart } from "./BarChart";
import { MetricsCard } from "./MetricsCard";
import { LOGIN_KEYS, REFUSED_KEYS, formatPeriod, niceBytesMax, niceMax, series, total } from "./series";

/** One chart: a title, the range's total, the bars, and what the total is made of. */
interface ChartSpec {
  id: string;
  keys: readonly string[];
  /** Readings (`gauge.*`) show the latest value instead of a sum. */
  gauge?: boolean;
  /** Parts of the total, listed under the chart. */
  parts?: readonly string[];
}

const CHARTS: ChartSpec[] = [
  { id: "received", keys: ["mail.received"], parts: ["mail.junk"] },
  { id: "refused", keys: REFUSED_KEYS, parts: [...REFUSED_KEYS, "refused.greylisted"] },
  { id: "submitted", keys: ["mail.submitted"] },
  { id: "delivered", keys: ["mail.delivered"], parts: ["mail.deferred", "mail.bounced"] },
  { id: "logins", keys: LOGIN_KEYS, parts: LOGIN_KEYS },
  { id: "storage", keys: ["gauge.storageBytes"], gauge: true },
];

/** The columns of the table view: everything the charts show, one column per stored key. */
const TABLE_KEYS = [
  "mail.received",
  "mail.junk",
  ...REFUSED_KEYS,
  "refused.greylisted",
  "mail.submitted",
  "mail.delivered",
  "mail.deferred",
  "mail.bounced",
  ...LOGIN_KEYS,
  "gauge.accounts",
  "gauge.storageBytes",
];

function Chart({ spec, view }: { spec: ChartSpec; view: StatsView }) {
  const { t, i18n } = useT();
  const format = (value: number) =>
    spec.gauge ? formatBytes(value, i18n.language) : formatNumber(value, i18n.language);
  const points = series(view, spec.keys);
  const latest = [...points].reverse().find((point) => point.value !== null)?.value ?? null;
  const headline = spec.gauge ? latest : total(view, spec.keys);
  const title = t(`stats.charts.${spec.id}.title`);
  return (
    <Card
      title={title}
      action={
        <span className="text-right text-[13px] text-muted">
          <span className="block text-lg font-bold text-ink">{headline === null ? "–" : format(headline)}</span>
          {t(spec.gauge ? "stats.latest" : `stats.total.${view.range}`)}
        </span>
      }
    >
      <p className="-mt-2 mb-3 text-[13px] text-muted">{t(`stats.charts.${spec.id}.hint`)}</p>
      <BarChart
        title={title}
        points={points}
        range={view.range}
        format={format}
        nice={spec.gauge ? niceBytesMax : niceMax}
      />
      {spec.parts && (
        <dl className="mt-3 grid grid-cols-[1fr_auto] gap-x-4 gap-y-1 border-t border-hairline pt-3 text-[13px]">
          {spec.parts.map((key) => (
            <div key={key} className="contents">
              <dt className="text-muted">{t(`stats.keys.${key}`)}</dt>
              <dd className="text-right font-semibold tabular-nums">
                {formatNumber(view.totals[key] ?? 0, i18n.language)}
              </dd>
            </div>
          ))}
        </dl>
      )}
    </Card>
  );
}

/** Every number of every period, for anyone who would rather read than look at bars. */
function StatsTable({ view }: { view: StatsView }) {
  const { t, i18n } = useT();
  return (
    <details className="rounded-card border border-hairline bg-surface p-5">
      <summary className="cursor-pointer text-[15px] font-bold">{t("stats.table.title")}</summary>
      <div className="mt-3 overflow-x-auto">
        <table className="w-full text-[13px]">
          <caption className="sr-only">{t("stats.table.caption")}</caption>
          <thead>
            <tr className="border-b border-hairline text-left text-muted">
              <th scope="col" className="py-1.5 pr-3 font-semibold">
                {t(`stats.table.period.${view.range}`)}
              </th>
              {TABLE_KEYS.map((key) => (
                <th key={key} scope="col" className="px-2 py-1.5 text-right font-semibold whitespace-nowrap">
                  {t(`stats.keys.${key}`)}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {[...view.periods].reverse().map(({ period, values }) => (
              <tr key={period} className="border-b border-hairline last:border-b-0">
                <th scope="row" className="py-1.5 pr-3 text-left font-semibold whitespace-nowrap">
                  {formatPeriod(period, view.range, i18n.language, true)}
                </th>
                {TABLE_KEYS.map((key) => {
                  const value = values[key];
                  return (
                    <td key={key} className="px-2 py-1.5 text-right tabular-nums">
                      {value === undefined
                        ? key.startsWith("gauge.")
                          ? "–"
                          : "0"
                        : key === "gauge.storageBytes"
                          ? formatBytes(value, i18n.language)
                          : formatNumber(value, i18n.language)}
                    </td>
                  );
                })}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </details>
  );
}

/** Server → Statistics: what happened on the server, day by day or month by month, and the Prometheus switch. */
export function StatsPage() {
  const { t } = useT();
  const [range, setRange] = useState<StatsRange>("days");
  const query = useQuery({
    queryKey: ["admin", "stats", range],
    queryFn: () => api<StatsView>(`/api/admin/stats?range=${range}`),
    refetchInterval: 60_000,
  });

  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
        <Segmented<StatsRange>
          label={t("stats.range.label")}
          value={range}
          onChange={setRange}
          options={(["days", "months"] as const).map((value) => ({ value, label: t(`stats.range.${value}`) }))}
        />
        <span className="min-w-0 flex-1 basis-60 text-[12px] text-muted">{t("stats.utc")}</span>
      </div>
      {query.isPending ? (
        <Loading />
      ) : query.isError ? (
        <LoadError error={query.error} onRetry={() => void query.refetch()} />
      ) : (
        <>
          <div className="grid gap-4 lg:grid-cols-2">
            {CHARTS.map((spec) => (
              <Chart key={spec.id} spec={spec} view={query.data} />
            ))}
          </div>
          <StatsTable view={query.data} />
        </>
      )}
      <MetricsCard />
    </div>
  );
}

import clsx from "clsx";
import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { useT } from "@/i18n";
import type { StatsRange } from "@/lib/api";
import { formatPeriod, niceMax, type Point } from "./series";

const HEIGHT = 168;
const TOP = 18;
const BOTTOM = 24;
const LEFT = 44;
const RIGHT = 8;
/** Bars never get thicker than this, however wide the chart is. */
const MAX_BAR = 24;
/** The surface gap between neighbouring bars. */
const GAP = 2;
const RADIUS = 4;

/** The chart's width, following its box. */
function useWidth() {
  const ref = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    setWidth(element.clientWidth);
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(([entry]) => setWidth(entry?.contentRect.width ?? 0));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return [ref, width] as const;
}

/** A column with a rounded top and a square foot on the baseline. */
function column(x: number, y: number, width: number, height: number): string {
  const r = Math.min(RADIUS, width / 2, height);
  const bottom = y + height;
  return `M${x},${bottom} V${y + r} Q${x},${y} ${x + r},${y} H${x + width - r} Q${x + width},${y} ${x + width},${y + r} V${bottom} Z`;
}

/**
 * One series over time as columns: hairline grid, three round ticks, the peak labelled, and the
 * rest on hover or with the arrow keys once the chart has focus. The table view has every number.
 */
export function BarChart({
  title,
  points,
  range,
  format,
  nice = niceMax,
}: {
  title: string;
  points: Point[];
  range: StatsRange;
  format: (value: number) => string;
  /** Rounds the largest value up to where the axis ends. */
  nice?: (max: number) => number;
}) {
  const { t, i18n } = useT();
  const [ref, width] = useWidth();
  const [active, setActive] = useState<number | null>(null);
  const values = points.map((point) => point.value ?? 0);
  const max = nice(Math.max(...values, 0));
  const peak = values.indexOf(Math.max(...values));
  const plotWidth = Math.max(width - LEFT - RIGHT, 0);
  const slot = points.length > 0 ? plotWidth / points.length : 0;
  const bar = Math.max(Math.min(slot - GAP, MAX_BAR), 1);
  const plotHeight = HEIGHT - TOP - BOTTOM;
  const y = (value: number) => TOP + plotHeight - (value / max) * plotHeight;
  const ticks = [0, max / 2, max];
  // A handful of dates under the axis: first, last and a few between, never crowded.
  const every = Math.max(1, Math.ceil(points.length / Math.max(1, Math.floor(plotWidth / 64))));
  const label = (index: number) => formatPeriod(points[index]!.period, range, i18n.language);
  const peakValue = values[peak] ?? 0;
  const summary =
    peakValue > 0
      ? t("stats.chartSummary", {
          series: title,
          max: format(peakValue),
          peak: formatPeriod(points[peak]!.period, range, i18n.language, true),
        })
      : t("stats.chartEmpty", { series: title });

  const onKeyDown = (event: KeyboardEvent) => {
    if (points.length === 0) return;
    const current = active ?? points.length - 1;
    if (event.key === "ArrowLeft") setActive(Math.max(0, current - 1));
    else if (event.key === "ArrowRight") setActive(Math.min(points.length - 1, current + 1));
    else if (event.key === "Home") setActive(0);
    else if (event.key === "End") setActive(points.length - 1);
    else return;
    event.preventDefault();
  };

  // The peak's label sits on its bar, but never runs off either side of the chart.
  const peakCenter = LEFT + slot * peak + slot / 2;
  const [peakX, peakAnchor] =
    peakCenter > width - RIGHT - 32
      ? [width - RIGHT, "end" as const]
      : peakCenter < LEFT + 32
        ? [LEFT, "start" as const]
        : [peakCenter, "middle" as const];

  const shown = active !== null ? points[active] : undefined;
  const tipX = active !== null ? LEFT + slot * active + slot / 2 : 0;

  return (
    <div ref={ref} className="relative">
      {width > 0 && (
        <svg
          width={width}
          height={HEIGHT}
          role="img"
          aria-label={summary}
          tabIndex={0}
          onKeyDown={onKeyDown}
          onFocus={() => setActive((current) => current ?? points.length - 1)}
          onBlur={() => setActive(null)}
          onPointerLeave={() => setActive(null)}
          className="block rounded-control focus:shadow-focus focus:outline-none"
        >
          {ticks.map((tick) => (
            <g key={tick}>
              <line
                x1={LEFT}
                x2={width - RIGHT}
                y1={y(tick)}
                y2={y(tick)}
                className="stroke-hairline"
                strokeWidth={1}
              />
              <text
                x={LEFT - 6}
                y={y(tick)}
                textAnchor="end"
                dominantBaseline="middle"
                className="fill-muted text-[11px] tabular-nums"
              >
                {tick === 0 ? "0" : format(tick)}
              </text>
            </g>
          ))}
          {points.map((point, index) => {
            const x = LEFT + slot * index + (slot - bar) / 2;
            const value = point.value ?? 0;
            const top = y(value);
            return (
              <g key={point.period} onPointerEnter={() => setActive(index)} onPointerMove={() => setActive(index)}>
                {/* The hover target is the whole slot, taller than the bar. */}
                <rect x={LEFT + slot * index} y={TOP} width={slot} height={plotHeight} fill="transparent" />
                {value > 0 && (
                  <path
                    d={column(x, top, bar, TOP + plotHeight - top)}
                    className={clsx(
                      "fill-chart transition-opacity",
                      active !== null && active !== index && "opacity-60",
                    )}
                  />
                )}
                {index % every === 0 || index === points.length - 1 ? (
                  <text
                    x={LEFT + slot * index + slot / 2}
                    y={HEIGHT - 6}
                    textAnchor={index === points.length - 1 ? "end" : index === 0 ? "start" : "middle"}
                    className="fill-muted text-[11px]"
                  >
                    {label(index)}
                  </text>
                ) : null}
              </g>
            );
          })}
          {peakValue > 0 && (
            <text x={peakX} y={y(peakValue) - 5} textAnchor={peakAnchor} className="fill-ink text-[11px] font-semibold">
              {format(peakValue)}
            </text>
          )}
        </svg>
      )}
      {shown && (
        <div
          role="status"
          className="pointer-events-none absolute top-0 z-10 -translate-x-1/2 rounded-control border border-hairline bg-surface px-2.5 py-1.5 text-center shadow-float"
          style={{ left: Math.min(Math.max(tipX, 60), width - 60) }}
        >
          <p className="text-sm font-bold">{shown.value === null ? "–" : format(shown.value)}</p>
          <p className="text-[11px] whitespace-nowrap text-muted">
            {formatPeriod(shown.period, range, i18n.language, true)}
          </p>
        </div>
      )}
    </div>
  );
}

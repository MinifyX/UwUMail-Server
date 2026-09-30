import clsx from "clsx";
import { AlertTriangle, FlaskConical, X } from "lucide-react";
import { useId, useState, type KeyboardEvent, type ReactNode } from "react";
import { Select, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { formatNumber } from "@/lib/format";
import {
  formatCost,
  formatDay,
  normalizeChip,
  rowSum,
  share,
  type Price,
  type Quota,
  type UsageRow,
  type UsageSum,
} from "./model";

/** Marks something that may stop working any day. */
export function ExperimentalBadge() {
  const { t } = useT();
  return (
    <span className="inline-flex h-5 shrink-0 items-center gap-1 rounded-full bg-warning-tint px-2 text-[11px] font-bold tracking-wide text-warning uppercase">
      <FlaskConical className="size-3" aria-hidden />
      {t("assist.experimental")}
    </span>
  );
}

/** A small word in a rounded box: where a provider comes from, whether it is switched on. */
export function Tag({ children, tone = "plain" }: { children: ReactNode; tone?: "plain" | "pink" | "muted" }) {
  return (
    <span
      className={clsx(
        "inline-flex h-5 shrink-0 items-center rounded-full px-2 text-[11px] font-semibold",
        tone === "pink" && "bg-pink-tint text-pink-ink",
        tone === "plain" && "border border-line text-muted",
        tone === "muted" && "bg-canvas text-faint",
      )}
    >
      {children}
    </span>
  );
}

/** A box that stands out: a warning, or a note that saves someone a wrong turn. */
export function Notice({
  tone = "warning",
  title,
  children,
}: {
  tone?: "warning" | "danger" | "info";
  title?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div
      role={tone === "info" ? "note" : "alert"}
      className={clsx(
        "flex items-start gap-2 rounded-control px-3 py-2.5 text-[13px]",
        tone === "warning" && "bg-warning-tint text-warning",
        tone === "danger" && "bg-danger-tint text-danger",
        tone === "info" && "bg-canvas text-muted",
      )}
    >
      {tone !== "info" && <AlertTriangle className="mt-0.5 size-4 shrink-0" aria-hidden />}
      <div className="flex min-w-0 flex-col gap-1">
        {title && <p className="font-bold">{title}</p>}
        <div className="flex flex-col gap-1">{children}</div>
      </div>
    </div>
  );
}

const OTHER = "\u0000other";

/**
 * A model: picked from what the provider named once it was asked, typed by hand before that or
 * when the one wanted is not in the list. Empty means the default, which `emptyLabel` names.
 */
export function ModelPicker({
  id,
  value,
  onChange,
  models,
  emptyLabel,
  placeholder,
}: {
  id: string;
  value: string;
  onChange: (value: string) => void;
  models: { id: string; name: string }[] | null;
  emptyLabel: string;
  placeholder?: string;
}) {
  const { t } = useT();
  const listed = models !== null && models.length > 0;
  const known = listed && (value === "" || models.some((model) => model.id === value));
  const [typing, setTyping] = useState(false);
  if (!listed) {
    return (
      <TextInput
        id={id}
        autoComplete="off"
        spellCheck={false}
        placeholder={placeholder}
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
    );
  }
  const custom = typing || !known;
  return (
    <div className="flex flex-col gap-2">
      <Select
        id={id}
        value={custom ? OTHER : value}
        onChange={(event) => {
          const next = event.target.value;
          if (next === OTHER) {
            setTyping(true);
          } else {
            setTyping(false);
            onChange(next);
          }
        }}
      >
        <option value="">{emptyLabel}</option>
        {models.map((model) => (
          <option key={model.id} value={model.id}>
            {model.name === model.id ? model.id : `${model.name} (${model.id})`}
          </option>
        ))}
        <option value={OTHER}>{t("assist.form.modelOther")}</option>
      </Select>
      {custom && (
        <TextInput
          aria-label={t("assist.form.modelCustom")}
          autoComplete="off"
          spellCheck={false}
          placeholder={placeholder}
          value={value}
          onChange={(event) => onChange(event.target.value)}
        />
      )}
    </div>
  );
}

/**
 * A list of words, each one a chip: domains or logins. Enter, a comma or leaving the field adds
 * what was typed; the suggestions come from what the server knows.
 */
export function ChipInput({
  id,
  values,
  onChange,
  suggestions,
  placeholder,
}: {
  id: string;
  values: string[];
  onChange: (values: string[]) => void;
  suggestions: string[];
  placeholder?: string;
}) {
  const { t } = useT();
  const listId = useId();
  const [text, setText] = useState("");
  const add = (raw: string) => {
    const added = raw
      .split(/[\s,;]+/)
      .reduce<string[]>((list, part) => {
        const value = normalizeChip(part, list);
        return value ? [...list, value] : list;
      }, values)
      .slice(values.length);
    if (added.length > 0) onChange([...values, ...added]);
    setText("");
  };
  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Enter" || event.key === ",") {
      event.preventDefault();
      add(text);
    } else if (event.key === "Backspace" && text === "" && values.length > 0) {
      onChange(values.slice(0, -1));
    }
  };
  const open = suggestions.filter((suggestion) => !values.includes(suggestion));
  return (
    <div className="flex flex-col gap-2">
      {values.length > 0 && (
        <ul className="flex flex-wrap gap-1.5">
          {values.map((value) => (
            <li
              key={value}
              className="inline-flex h-7 items-center gap-1 rounded-full bg-pink-tint pr-1 pl-3 text-[13px] font-medium text-pink-ink"
            >
              {value}
              <button
                type="button"
                className="inline-flex size-5 items-center justify-center rounded-full hover:bg-pink-tint-strong"
                aria-label={t("assist.chips.remove", { value })}
                title={t("assist.chips.remove", { value })}
                onClick={() => onChange(values.filter((other) => other !== value))}
              >
                <X className="size-3.5" aria-hidden />
              </button>
            </li>
          ))}
        </ul>
      )}
      <TextInput
        id={id}
        list={listId}
        autoComplete="off"
        spellCheck={false}
        placeholder={placeholder}
        value={text}
        onChange={(event) => {
          const next = event.target.value;
          // Picking a suggestion from the list fills the whole field at once: take it right away.
          if (open.includes(next.trim().toLowerCase())) add(next);
          else setText(next);
        }}
        onKeyDown={onKeyDown}
        onBlur={() => add(text)}
      />
      <datalist id={listId}>
        {open.map((suggestion) => (
          <option key={suggestion} value={suggestion} />
        ))}
      </datalist>
    </div>
  );
}

/** A provider's daily limit per person in words. */
export function QuotaText({ quota }: { quota: Quota | null }) {
  const { t, i18n } = useT();
  const parts: string[] = [];
  if (quota?.requestsPerDay != null) {
    parts.push(
      t("assist.quota.requests", {
        count: quota.requestsPerDay,
        value: formatNumber(quota.requestsPerDay, i18n.language),
      }),
    );
  }
  if (quota?.tokensPerDay != null) {
    parts.push(
      t("assist.quota.tokens", { count: quota.tokensPerDay, value: formatNumber(quota.tokensPerDay, i18n.language) }),
    );
  }
  return <>{parts.length > 0 ? parts.join(" · ") : t("assist.quota.none")}</>;
}

/** A price in US dollars per million tokens: "$0.25 / $2.00 per million tokens (in / out)", or free. */
export function PriceText({ price }: { price: Price }) {
  const { t, i18n } = useT();
  if (price.source === "free") return <>{t("assist.price.free")}</>;
  const dollars = (value: number) =>
    new Intl.NumberFormat(i18n.language, { style: "currency", currency: "USD", maximumSignificantDigits: 4 }).format(
      value,
    );
  return (
    <>
      {t("assist.price.perMillion", { input: dollars(price.inputPerMillion), output: dollars(price.outputPerMillion) })}
      {price.source === "manual" && ` (${t("assist.price.manual")})`}
    </>
  );
}

/** How much of today's limit is used: a thin bar that warns as it fills. */
export function UsageMeter({ used, limit, label }: { used: number; limit: number | null; label: string }) {
  const { t, i18n } = useT();
  const part = share(used, limit);
  const usedText = formatNumber(used, i18n.language);
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-baseline justify-between gap-3 text-[13px]">
        <span className="text-muted">{label}</span>
        <span className="font-semibold tabular-nums">
          {limit === null
            ? t("assist.usage.noLimit", { used: usedText })
            : t("assist.usage.ofLimit", { used: usedText, limit: formatNumber(limit, i18n.language) })}
        </span>
      </div>
      {part !== null && (
        <div className="h-2 overflow-hidden rounded-full bg-canvas" aria-hidden>
          <div
            className={clsx("h-full rounded-full", part >= 1 ? "bg-danger" : part > 0.8 ? "bg-warning" : "bg-pink")}
            style={{ width: `${Math.max(part * 100, used > 0 ? 2 : 0)}%` }}
          />
        </div>
      )}
    </div>
  );
}

/** A cost cell's text: the amount, or a dash where it is not known. */
function costText(amount: number | null, currency: string, language: string): string {
  return amount === null ? "–" : formatCost(amount, currency, language);
}

/**
 * Requests, tokens in and tokens out as table cells, with a bar for the requests when `max` is
 * given, and the cost when a `currency` is.
 */
export function SumCells({ sum, max, currency }: { sum: UsageSum; max?: number; currency?: string }) {
  const { i18n } = useT();
  const number = (value: number) => formatNumber(value, i18n.language);
  return (
    <>
      <td className="px-2 py-1.5 text-right tabular-nums">
        <span className="flex items-center justify-end gap-2">
          {max !== undefined && max > 0 && (
            <span className="hidden h-1.5 w-16 overflow-hidden rounded-full bg-canvas sm:block" aria-hidden>
              <span
                className="block h-full rounded-full bg-pink"
                style={{ width: `${Math.max(2, (sum.requests / max) * 100)}%` }}
              />
            </span>
          )}
          {number(sum.requests)}
        </span>
      </td>
      <td className="px-2 py-1.5 text-right tabular-nums">{number(sum.inputTokens)}</td>
      <td className="py-1.5 pl-2 text-right tabular-nums">{number(sum.outputTokens)}</td>
      {currency && (
        <td className="py-1.5 pl-2 text-right whitespace-nowrap tabular-nums">
          {costText(sum.amount, currency, i18n.language)}
        </td>
      )}
    </>
  );
}

export function SumHeads({ cost = false }: { cost?: boolean }) {
  const { t } = useT();
  return (
    <>
      <th scope="col" className="px-2 py-1.5 text-right font-semibold">
        {t("assist.usage.requests")}
      </th>
      <th scope="col" className="px-2 py-1.5 text-right font-semibold whitespace-nowrap">
        {t("assist.usage.inputTokens")}
      </th>
      <th scope="col" className="py-1.5 pl-2 text-right font-semibold whitespace-nowrap">
        {t("assist.usage.outputTokens")}
      </th>
      {cost && (
        <th scope="col" className="py-1.5 pl-2 text-right font-semibold whitespace-nowrap">
          {t("assist.usage.cost")}
        </th>
      )}
    </>
  );
}

/** The sums as tiles; the cost as a fourth when a `currency` is given. */
export function UsageTotals({ sum, currency }: { sum: UsageSum; currency?: string }) {
  const { t, i18n } = useT();
  const tiles: [string, string][] = [
    ["requests", formatNumber(sum.requests, i18n.language)],
    ["inputTokens", formatNumber(sum.inputTokens, i18n.language)],
    ["outputTokens", formatNumber(sum.outputTokens, i18n.language)],
  ];
  if (currency) tiles.push(["cost", costText(sum.amount, currency, i18n.language)]);
  return (
    <dl className={clsx("grid gap-2 sm:gap-3", currency ? "grid-cols-2 sm:grid-cols-4" : "grid-cols-3")}>
      {tiles.map(([key, value]) => (
        <div key={key} className="min-w-0 rounded-control bg-canvas px-3 py-2">
          <dt className="truncate text-[12px] text-muted">{t(`assist.usage.${key}`)}</dt>
          <dd className="text-sm font-bold tabular-nums sm:text-lg">{value}</dd>
        </div>
      ))}
    </dl>
  );
}

/** A table of sums with a label column: per day, per feature or per person. */
export function SumTable<Row extends UsageSum>({
  caption,
  head,
  rows,
  rowKey,
  label,
  total,
  currency,
}: {
  caption: string;
  head: string;
  rows: Row[];
  rowKey: (row: Row) => string;
  label: (row: Row) => ReactNode;
  /** A last line with the sum of everything. */
  total?: UsageSum;
  /** Shows the costs, in this currency. */
  currency?: string;
}) {
  const { t } = useT();
  const max = Math.max(0, ...rows.map((row) => row.requests));
  return (
    <div className="overflow-x-auto">
      <table className="w-full min-w-[440px] text-[13px]">
        <caption className="sr-only">{caption}</caption>
        <thead>
          <tr className="border-b border-hairline text-left text-muted">
            <th scope="col" className="py-1.5 pr-3 font-semibold">
              {head}
            </th>
            <SumHeads cost={currency !== undefined} />
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={rowKey(row)} className="border-b border-hairline last:border-b-0">
              <th scope="row" className="py-1.5 pr-3 text-left font-semibold break-all">
                {label(row)}
              </th>
              <SumCells sum={row} max={max} currency={currency} />
            </tr>
          ))}
        </tbody>
        {total && (
          <tfoot>
            <tr className="border-t border-line font-bold">
              <th scope="row" className="py-1.5 pr-3 text-left">
                {t("assist.usage.total")}
              </th>
              <SumCells sum={total} currency={currency} />
            </tr>
          </tfoot>
        )}
      </table>
    </div>
  );
}

/** Every row as it came, folded away: day, (person,) provider, feature and the sums. */
export function EntriesTable({
  rows,
  showPerson,
  currency,
}: {
  rows: UsageRow[];
  showPerson: boolean;
  currency?: string;
}) {
  const { t, i18n } = useT();
  return (
    <details className="rounded-control border border-hairline p-3">
      <summary className="cursor-pointer text-[13px] font-semibold">
        {t("assist.usage.allEntries", { count: rows.length })}
      </summary>
      <div className="mt-3 overflow-x-auto">
        <table className="w-full min-w-[640px] text-[13px]">
          <caption className="sr-only">{t("assist.usage.caption")}</caption>
          <thead>
            <tr className="border-b border-hairline text-left text-muted">
              <th scope="col" className="py-1.5 pr-3 font-semibold">
                {t("assist.usage.day")}
              </th>
              {showPerson && (
                <th scope="col" className="px-2 py-1.5 font-semibold">
                  {t("assist.usage.person")}
                </th>
              )}
              <th scope="col" className="px-2 py-1.5 font-semibold">
                {t("assist.usage.provider")}
              </th>
              <th scope="col" className="px-2 py-1.5 font-semibold">
                {t("assist.usage.feature")}
              </th>
              <SumHeads cost={currency !== undefined} />
            </tr>
          </thead>
          <tbody>
            {rows.map((row, index) => (
              <tr key={index} className="border-b border-hairline last:border-b-0">
                <th scope="row" className="py-1.5 pr-3 text-left font-semibold whitespace-nowrap">
                  {formatDay(row.day, i18n.language)}
                </th>
                {showPerson && <td className="px-2 py-1.5 break-all">{row.login}</td>}
                <td className="px-2 py-1.5">{row.providerName}</td>
                <td className="px-2 py-1.5">{t(`assist.features.${row.feature}`)}</td>
                <SumCells sum={rowSum(row)} currency={currency} />
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </details>
  );
}

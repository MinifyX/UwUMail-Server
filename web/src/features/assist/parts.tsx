import clsx from "clsx";
import { AlertTriangle, FlaskConical, X } from "lucide-react";
import { useId, useState, type KeyboardEvent, type ReactNode } from "react";
import { Select, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { formatNumber } from "@/lib/format";
import { normalizeChip, share, type Quota } from "./model";

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

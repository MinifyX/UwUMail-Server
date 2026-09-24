import { useState } from "react";
import { Field, Select, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { formatDate } from "@/lib/format";
import { EXPIRY_PRESETS, dateInputValue, expiryFromDate, expiryFromDays } from "./scope";

/** For good, for a few days, or until a date: when a rule runs out by itself. */
export function ExpiryField({
  value,
  onChange,
  label,
}: {
  value: number | null;
  onChange: (expiresAt: number | null) => void;
  label?: string;
}) {
  const { t, i18n } = useT();
  const [mode, setMode] = useState<string>(value === null ? "0" : "date");
  const [today] = useState(() => dateInputValue(Math.floor(Date.now() / 1000) + 86_400));
  return (
    <Field
      label={label ?? t("spam.rules.expiry.label")}
      hint={
        value === null
          ? t("spam.rules.expiry.never")
          : t("spam.rules.expiry.until", { date: formatDate(value, i18n.language) })
      }
    >
      {(id) => (
        <div className="flex flex-wrap gap-2">
          <Select
            id={id}
            className="min-w-44 flex-1"
            value={mode}
            onChange={(event) => {
              const next = event.target.value;
              setMode(next);
              if (next !== "date") onChange(expiryFromDays(Number(next)));
              else if (value === null) onChange(expiryFromDate(today));
            }}
          >
            {EXPIRY_PRESETS.map((days) => (
              <option key={days} value={String(days)}>
                {days === 0 ? t("spam.rules.expiry.forever") : t("spam.rules.expiry.days", { count: days })}
              </option>
            ))}
            <option value="date">{t("spam.rules.expiry.date")}</option>
          </Select>
          {mode === "date" && (
            <TextInput
              type="date"
              aria-label={t("spam.rules.expiry.date")}
              className="w-44"
              min={today}
              value={dateInputValue(value)}
              onChange={(event) => onChange(expiryFromDate(event.target.value))}
            />
          )}
        </div>
      )}
    </Field>
  );
}

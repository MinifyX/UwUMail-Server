import { useMutation } from "@tanstack/react-query";
import { Ban, Check, Hash, UserRound } from "lucide-react";
import { useState, type FormEvent } from "react";
import { Button } from "@/components/ui/Button";
import { Dialog } from "@/components/ui/Dialog";
import { Field, Segmented, Select, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type NewRule, type Rule, type RuleChange, type SenderKind } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatNumber } from "@/lib/format";
import { guessSenderKind } from "../senders";
import { ExpiryField } from "./ExpiryField";
import { ScopePicker } from "./ScopePicker";
import { scopeKey } from "./scope";

const KINDS: SenderKind[] = ["address", "domain", "pattern", "ip", "host"];

export type RuleDraft = {
  type: "sender" | "word";
  list: "allow" | "block";
  scope: string;
};

/**
 * One rule, new or to change: what it is (a sender to allow or block, or a word that adds points), for whom,
 * and until when. Everything a rule can hold is here, the rarer parts below the common ones.
 */
export function RuleDialog({
  open,
  onClose,
  onSaved,
  admin,
  base,
  rule,
  initial,
  defaultPoints,
  maxPoints,
}: {
  open: boolean;
  onClose: () => void;
  onSaved: (rule: Rule) => void;
  admin: boolean;
  /** /api/admin/spam or /api/account/spam */
  base: string;
  /** The rule to change; a new one without. */
  rule?: Rule | null;
  initial?: Partial<RuleDraft>;
  defaultPoints: number;
  maxPoints: number;
}) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const editing = Boolean(rule);
  const [type, setType] = useState<"sender" | "word">(rule?.type ?? initial?.type ?? "sender");
  const [list, setList] = useState<"allow" | "block">(
    rule?.list === "allow" ? "allow" : rule ? "block" : (initial?.list ?? "block"),
  );
  const [value, setValue] = useState(rule?.value ?? "");
  // Changing a rule keeps its kind unless the value or the kind itself changes.
  const [kind, setKind] = useState<SenderKind | "auto">("auto");
  const [note, setNote] = useState(rule?.note ?? "");
  const [points, setPoints] = useState(rule?.points != null ? String(rule.points) : "");
  const [scope, setScope] = useState(rule ? scopeKey(rule.scope) : (initial?.scope ?? "server"));
  const [expiresAt, setExpiresAt] = useState<number | null>(rule?.expiresAt ?? null);

  const parsedPoints = points.trim() === "" ? null : Number(points.replace(",", "."));
  const pointsInvalid =
    parsedPoints !== null && (!Number.isFinite(parsedPoints) || parsedPoints < 0.1 || parsedPoints > maxPoints);

  const save = useMutation({
    mutationFn: () => {
      if (rule) {
        const change: RuleChange = { note, expiresAt };
        if (value.trim() !== rule.value) change.value = value;
        if (rule.type === "sender") {
          change.list = list;
          if (kind !== "auto") change.kind = kind;
        } else {
          change.points = parsedPoints;
        }
        if (admin && scope !== scopeKey(rule.scope)) change.scope = scope;
        return api<Rule>(`${base}/rules/${rule.type}/${rule.id}`, { method: "PATCH", body: change });
      }
      const body: NewRule = { type, value, note, expiresAt, ...(admin ? { scope } : {}) };
      if (type === "sender") {
        body.list = list;
        if (kind !== "auto") body.kind = kind;
      } else {
        body.points = parsedPoints;
      }
      return api<Rule>(`${base}/rules`, { method: "POST", body });
    },
    onSuccess: (saved) => onSaved(saved),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    save.mutate();
  };
  const guessed = value.trim() ? guessSenderKind(value) : null;

  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={editing ? t("spam.rules.dialog.editTitle") : t("spam.rules.dialog.newTitle")}
      closeOnOutsideClick={!value}
    >
      <form className="flex flex-col gap-4 px-6 pt-2 pb-6" onSubmit={submit}>
        {!editing && (
          <Segmented<"sender" | "word">
            label={t("spam.rules.dialog.type")}
            value={type}
            onChange={setType}
            options={[
              { value: "sender", label: t("spam.rules.types.sender") },
              { value: "word", label: t("spam.rules.types.word") },
            ]}
          />
        )}

        {type === "sender" && (
          <div className="grid gap-2 sm:grid-cols-2" role="radiogroup" aria-label={t("spam.rules.dialog.what")}>
            {(["block", "allow"] as const).map((option) => (
              <button
                key={option}
                type="button"
                role="radio"
                aria-checked={list === option}
                onClick={() => setList(option)}
                className={`flex items-start gap-3 rounded-control border px-3.5 py-3 text-left transition-colors ${
                  list === option ? "border-pink bg-pink-tint/50" : "border-line hover:border-faint/60"
                }`}
              >
                {option === "block" ? (
                  <Ban className="mt-0.5 size-4 shrink-0 text-danger" aria-hidden />
                ) : (
                  <Check className="mt-0.5 size-4 shrink-0 text-success" aria-hidden />
                )}
                <span className="flex flex-col gap-0.5">
                  <span className="text-sm font-semibold">{t(`spam.rules.lists.${option}`)}</span>
                  <span className="text-[12px] text-muted">{t(`spam.rules.dialog.${option}Hint`)}</span>
                </span>
              </button>
            ))}
          </div>
        )}

        <Field
          label={type === "sender" ? t("spam.senders.value") : t("spam.rules.dialog.word")}
          hint={
            type === "sender"
              ? kind === "auto" && guessed && value.trim() !== rule?.value
                ? t("spam.senders.guessed", { kind: t(`spam.senders.kinds.${guessed}`) })
                : t("spam.senders.valueHint")
              : t("spam.words.entriesHint")
          }
        >
          {(id) => (
            <TextInput
              id={id}
              autoFocus
              required
              autoCapitalize="none"
              spellCheck={false}
              className="font-mono"
              placeholder={
                type === "sender" ? t("spam.senders.valuePlaceholder") : t("spam.rules.dialog.wordPlaceholder")
              }
              value={value}
              onChange={(event) => setValue(event.target.value)}
            />
          )}
        </Field>

        <div className="grid gap-4 sm:grid-cols-2">
          {admin && (
            <Field label={t("spam.senders.scope")}>
              {() => <ScopePicker value={scope} onChange={setScope} label={t("spam.senders.scope")} />}
            </Field>
          )}
          {type === "sender" ? (
            <Field label={t("spam.senders.kind")}>
              {(id) => (
                <Select id={id} value={kind} onChange={(event) => setKind(event.target.value as SenderKind | "auto")}>
                  <option value="auto">
                    {rule?.type === "sender" && value.trim() === rule.value
                      ? t("spam.rules.dialog.kindKeep", { kind: t(`spam.senders.kinds.${rule.kind}`) })
                      : t("spam.senders.kindAuto")}
                  </option>
                  {KINDS.map((option) => (
                    <option key={option} value={option}>
                      {t(`spam.senders.kinds.${option}`)}
                    </option>
                  ))}
                </Select>
              )}
            </Field>
          ) : (
            <Field
              label={t("spam.words.points")}
              hint={t("spam.rules.dialog.pointsHint", {
                points: formatNumber(defaultPoints, i18n.language),
                max: formatNumber(maxPoints, i18n.language),
              })}
              error={
                pointsInvalid
                  ? t("spam.rules.dialog.pointsInvalid", { max: formatNumber(maxPoints, i18n.language) })
                  : undefined
              }
            >
              {(id) => (
                <TextInput
                  id={id}
                  inputMode="decimal"
                  placeholder={formatNumber(defaultPoints, i18n.language)}
                  value={points}
                  onChange={(event) => setPoints(event.target.value)}
                />
              )}
            </Field>
          )}
        </div>

        <div className="grid gap-4 sm:grid-cols-2">
          <ExpiryField value={expiresAt} onChange={setExpiresAt} />
          <Field label={t("spam.senders.note")}>
            {(id) => (
              <TextInput
                id={id}
                maxLength={200}
                placeholder={t("spam.senders.notePlaceholder")}
                value={note}
                onChange={(event) => setNote(event.target.value)}
              />
            )}
          </Field>
        </div>

        {rule && (
          <p className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-control bg-canvas px-3 py-2 text-[12px] text-muted">
            <span className="inline-flex items-center gap-1">
              <Hash className="size-3" aria-hidden />
              {t("spam.rules.hits", { count: rule.hits })}
            </span>
            {rule.createdBy && (
              <span className="inline-flex items-center gap-1">
                <UserRound className="size-3" aria-hidden />
                {t("spam.rules.createdBy", { name: rule.createdBy })}
              </span>
            )}
          </p>
        )}

        {save.isError && (
          <p role="alert" className="rounded-control bg-danger-tint px-3 py-2 text-[13px] text-danger">
            {errorText(save.error)}
          </p>
        )}
        <div className="flex flex-wrap justify-end gap-2">
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button type="submit" variant="primary" busy={save.isPending} disabled={!value.trim() || pointsInvalid}>
            {editing ? t("common.save") : t("spam.rules.dialog.add")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

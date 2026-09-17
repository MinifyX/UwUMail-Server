import { Forward, Trash2 } from "lucide-react";
import { type FormEvent, useState } from "react";
import { Button, IconButton } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Field, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import type { DomainDetail, ForwardAddress } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { usePrefs } from "@/state/prefs";
import { useRemoveForwardAddress, useSetForwardAddress } from "./queries";

const textareaClass =
  "w-full rounded-control border border-line bg-surface px-3.5 py-2.5 font-mono text-[13px] text-ink placeholder:text-faint focus:border-pink focus:shadow-focus focus:outline-none disabled:opacity-60";

/** Addresses without a mailbox that pass their mail on, here or to other servers. */
export function ForwardsCard({ domain }: { domain: DomainDetail }) {
  const { t } = useT();
  const errorText = useErrorText();
  const pro = usePrefs((s) => s.mode) === "pro";
  const save = useSetForwardAddress(domain.name);
  const remove = useRemoveForwardAddress(domain.name);
  const [local, setLocal] = useState("");
  const [targets, setTargets] = useState("");
  const [note, setNote] = useState("");

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const list = targets
      .split(/[\s,;]+/)
      .map((target) => target.trim())
      .filter(Boolean);
    save.mutate(
      { local: local.trim(), targets: list, note: note.trim() },
      {
        onSuccess: () => {
          setLocal("");
          setTargets("");
          setNote("");
        },
      },
    );
  };

  /** Puts an existing forwarding address into the form to change its targets. */
  const edit = (forward: ForwardAddress) => {
    setLocal(forward.address.slice(0, forward.address.lastIndexOf("@")));
    setTargets(forward.targets.join("\n"));
    setNote(forward.note);
  };

  return (
    <Card title={t("domains.forwards.title")}>
      <div className="flex flex-col gap-4">
        <p className="-mt-1 text-[13px] text-muted">{t("domains.forwards.explain")}</p>
        {domain.forwards.length > 0 && (
          <ul className="flex flex-col divide-y divide-hairline">
            {domain.forwards.map((forward) => (
              <li key={forward.address} className="flex items-start gap-3 py-2.5">
                <Forward className="mt-0.5 size-4 shrink-0 text-muted" aria-hidden />
                <button
                  type="button"
                  className="min-w-0 flex-1 text-left"
                  onClick={() => edit(forward)}
                  title={t("domains.forwards.edit")}
                >
                  <span className="block truncate text-sm font-semibold">{forward.address}</span>
                  <span className="block text-[13px] break-words text-muted">
                    {t("domains.forwards.to", { targets: forward.targets.join(", ") })}
                  </span>
                  {forward.note && <span className="block text-[12px] text-faint">{forward.note}</span>}
                </button>
                <IconButton
                  icon={Trash2}
                  label={t("domains.forwards.remove", { address: forward.address })}
                  disabled={remove.isPending}
                  onClick={() => {
                    if (window.confirm(t("domains.forwards.removeConfirm", { address: forward.address }))) {
                      remove.mutate(forward.address.slice(0, forward.address.lastIndexOf("@")));
                    }
                  }}
                />
              </li>
            ))}
          </ul>
        )}
        <form className="flex flex-col gap-3" onSubmit={submit}>
          <Field label={t("domains.forwards.address")}>
            {(id) => (
              <div className="flex items-center gap-2">
                <TextInput
                  id={id}
                  required
                  autoComplete="off"
                  autoCapitalize="none"
                  spellCheck={false}
                  placeholder={t("domains.forwards.addressPlaceholder")}
                  value={local}
                  onChange={(event) => setLocal(event.target.value)}
                />
                <span className="shrink-0 text-sm text-muted">@{domain.name}</span>
              </div>
            )}
          </Field>
          <Field
            label={t("domains.forwards.targets")}
            hint={t("domains.forwards.targetsHint")}
            error={save.isError ? errorText(save.error) : undefined}
          >
            {(id) => (
              <textarea
                id={id}
                required
                rows={2}
                spellCheck={false}
                className={textareaClass}
                placeholder="kasse@example.org"
                value={targets}
                onChange={(event) => setTargets(event.target.value)}
              />
            )}
          </Field>
          {pro && (
            <Field label={t("domains.forwards.note")}>
              {(id) => (
                <TextInput
                  id={id}
                  maxLength={200}
                  placeholder={t("domains.forwards.notePlaceholder")}
                  value={note}
                  onChange={(event) => setNote(event.target.value)}
                />
              )}
            </Field>
          )}
          <div className="flex justify-end">
            <Button type="submit" icon={Forward} busy={save.isPending}>
              {t("domains.forwards.save")}
            </Button>
          </div>
        </form>
      </div>
    </Card>
  );
}

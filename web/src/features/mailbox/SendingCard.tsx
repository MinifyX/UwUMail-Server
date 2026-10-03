import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Send } from "lucide-react";
import { useState } from "react";
import { Card } from "@/components/ui/Card";
import { Field, Select } from "@/components/ui/Field";
import { useSession } from "@/features/session/session";
import { useT } from "@/i18n";
import { api } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";
import { DomainSignatures } from "./DomainSignatures";

/** The choices of the undo window, in seconds; the server applies it to every message sent over JMAP. */
export const UNDO_CHOICES = ["0", "5", "10", "20", "30"] as const;
/** What the server uses when nobody chose. */
export const DEFAULT_UNDO = "10";

export const identitiesKey = ["account", "identities"] as const;

function UndoSetting() {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const session = useSession();
  const stored = session.data?.preferences.mailUndoSend;
  const [value, setValue] = useState<string>(
    typeof stored === "string" && (UNDO_CHOICES as readonly string[]).includes(stored) ? stored : DEFAULT_UNDO,
  );
  const save = useMutation({
    mutationFn: (next: string) =>
      api<Record<string, unknown>>("/api/account/preferences", { method: "PATCH", body: { mailUndoSend: next } }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["session"] });
      toast(t("mailbox.sending.saved"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  return (
    <Field label={t("mailbox.sending.undo")} hint={t("mailbox.sending.undoHint")}>
      {(id) => (
        <Select
          id={id}
          value={value}
          disabled={save.isPending}
          onChange={(event) => {
            setValue(event.target.value);
            save.mutate(event.target.value);
          }}
        >
          {UNDO_CHOICES.map((choice) => (
            <option key={choice} value={choice}>
              {choice === "0" ? t("mailbox.sending.undoOff") : t("mailbox.sending.undoSeconds", { seconds: choice })}
            </option>
          ))}
        </Select>
      )}
    </Field>
  );
}

/** How mail leaves: the undo window, and the signatures per domain. */
export function SendingCard() {
  const { t } = useT();

  return (
    <Card
      className="lg:col-span-2"
      title={
        <span className="flex items-center gap-2">
          <Send className="size-4 text-muted" aria-hidden />
          {t("mailbox.sending.title")}
        </span>
      }
    >
      <div className="flex flex-col gap-6">
        <UndoSetting />
        <div className="flex flex-col gap-3">
          <h3 className="text-sm font-bold">{t("mailbox.signatures.title")}</h3>
          <p className="text-sm text-muted">{t("mailbox.signatures.intro")}</p>
          <DomainSignatures />
        </div>
      </div>
    </Card>
  );
}

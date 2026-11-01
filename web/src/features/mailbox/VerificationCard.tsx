import { useMutation, useQueryClient } from "@tanstack/react-query";
import { KeyRound } from "lucide-react";
import { useState } from "react";
import { Card } from "@/components/ui/Card";
import { Field, Select, Toggle } from "@/components/ui/Field";
import { useSession } from "@/features/session/session";
import { useT } from "@/i18n";
import { api } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";

/** When a mail whose code expired leaves the inbox; the server's cleanup applies it (docs/verification-codes.md). */
export const CLEANUP_CHOICES = ["off", "immediately", "24h", "7d"] as const;
/** What the server uses when nobody chose. */
export const DEFAULT_CLEANUP = "24h";

const CLEANUP_LABELS = {
  off: "mailbox.verification.cleanupOff",
  immediately: "mailbox.verification.cleanupImmediately",
  "24h": "mailbox.verification.cleanup24h",
  "7d": "mailbox.verification.cleanup7d",
} as const;

/**
 * Verification codes in incoming mail: whether the server also guesses codes in mail without the
 * Verification-Info header, and when expired code mails are cleaned up. Both are preferences the
 * apps also see as JMAP UserSettings (`verificationHeuristics`, `verificationCleanup`).
 */
export function VerificationCard() {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const preferences = useSession().data?.preferences ?? {};
  const storedCleanup = preferences.mailVerificationCleanup;
  const [heuristics, setHeuristics] = useState(preferences.mailVerificationHeuristics !== "off");
  const [cleanup, setCleanup] = useState<string>(
    typeof storedCleanup === "string" && (CLEANUP_CHOICES as readonly string[]).includes(storedCleanup)
      ? storedCleanup
      : DEFAULT_CLEANUP,
  );
  const save = useMutation({
    mutationFn: (changes: Record<string, string>) =>
      api<Record<string, unknown>>("/api/account/preferences", { method: "PATCH", body: changes }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["session"] });
      toast(t("mailbox.verification.saved"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  return (
    <Card
      className="lg:col-span-2"
      title={
        <span className="flex items-center gap-2">
          <KeyRound className="size-4 text-muted" aria-hidden />
          {t("mailbox.verification.title")}
        </span>
      }
    >
      <div className="flex flex-col gap-6">
        <p className="text-sm text-muted">{t("mailbox.verification.intro")}</p>
        <Toggle
          checked={heuristics}
          disabled={save.isPending}
          onChange={(on) => {
            setHeuristics(on);
            save.mutate({ mailVerificationHeuristics: on ? "on" : "off" });
          }}
          label={t("mailbox.verification.heuristics")}
          description={t("mailbox.verification.heuristicsHint")}
        />
        <Field label={t("mailbox.verification.cleanup")} hint={t("mailbox.verification.cleanupHint")}>
          {(id) => (
            <Select
              id={id}
              value={cleanup}
              disabled={save.isPending}
              onChange={(event) => {
                setCleanup(event.target.value);
                save.mutate({ mailVerificationCleanup: event.target.value });
              }}
            >
              {CLEANUP_CHOICES.map((choice) => (
                <option key={choice} value={choice}>
                  {t(CLEANUP_LABELS[choice])}
                </option>
              ))}
            </Select>
          )}
        </Field>
      </div>
    </Card>
  );
}

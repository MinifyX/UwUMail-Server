import { useMutation, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Field, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { type AccountSpamView, api, type SpamLimits, type SpamLimitsView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";

const toText = (value: number | null) => (value === null ? "" : String(value));
const toLimit = (text: string) => (text.trim() === "" ? null : Number(text.replace(",", ".")));

/** One's own spam limits: from how many points mail goes to Junk or is refused, empty for the server's. */
export function SpamLimitsCard({ limits, queryKey }: { limits: SpamLimitsView; queryKey: readonly string[] }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [junk, setJunk] = useState(toText(limits.own.junk));
  const [reject, setReject] = useState(toText(limits.own.reject));

  const save = useMutation({
    mutationFn: (body: SpamLimits) => api<SpamLimitsView>("/api/account/spam/limits", { method: "PUT", body }),
    onSuccess: (next) => {
      queryClient.setQueryData<AccountSpamView>(queryKey, (view) => view && { ...view, limits: next });
      setJunk(toText(next.own.junk));
      setReject(toText(next.own.reject));
      toast(t("common.saved"), "success");
    },
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    save.mutate({ junk: toLimit(junk), reject: toLimit(reject) });
  };

  const input = (id: string, value: string, onChange: (value: string) => void, placeholder: string) => (
    <TextInput
      id={id}
      type="number"
      inputMode="decimal"
      min={limits.min}
      max={limits.max}
      step={0.5}
      placeholder={placeholder}
      value={value}
      onChange={(event) => onChange(event.target.value)}
    />
  );
  const server = limits.server;

  return (
    <Card title={t("spam.limits.title")}>
      <form className="flex flex-col gap-4" onSubmit={submit}>
        <p className="-mt-1 text-[13px] text-muted">{t("spam.limits.explain")}</p>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label={t("spam.limits.junk")} hint={t("spam.limits.junkHint", { server: server.junk })}>
            {(id) => input(id, junk, setJunk, t("spam.limits.serverValue", { value: server.junk }))}
          </Field>
          <Field
            label={t("spam.limits.reject")}
            hint={
              server.reject === null
                ? t("spam.limits.rejectHintNever")
                : t("spam.limits.rejectHint", { server: server.reject })
            }
          >
            {(id) =>
              input(
                id,
                reject,
                setReject,
                server.reject === null
                  ? t("spam.limits.never")
                  : t("spam.limits.serverValue", { value: server.reject }),
              )
            }
          </Field>
        </div>
        {save.isError && (
          <p role="alert" className="text-[13px] text-danger">
            {errorText(save.error)}
          </p>
        )}
        <div className="flex justify-end">
          <Button type="submit" busy={save.isPending}>
            {t("common.save")}
          </Button>
        </div>
      </form>
    </Card>
  );
}

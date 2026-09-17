import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Smartphone } from "lucide-react";
import { useState, type FormEvent } from "react";
import { Button } from "@/components/ui/Button";
import { Field, TextInput } from "@/components/ui/Field";
import { Cancelled, usePasswordConfirmation } from "@/features/security/ConfirmPassword";
import { securityKey } from "@/features/security/queries";
import { useT } from "@/i18n";
import { api, type AppPasswordInfo } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";

function guessDevice(): string {
  const agent = navigator.userAgent;
  if (agent.includes("iPad")) return "iPad";
  if (agent.includes("iPhone")) return "iPhone";
  if (agent.includes("Macintosh")) return "Mac";
  return "iPhone";
}

/** Downloads a configuration profile that sets up mail on an Apple device with its own app password. */
export function AppleProfile() {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const { confirmed, dialog } = usePasswordConfirmation();
  const [device, setDevice] = useState(guessDevice);
  const create = useMutation({
    mutationFn: () =>
      confirmed((password) =>
        api<{ appPassword: AppPasswordInfo; url: string }>("/api/account/apple-profiles", {
          method: "POST",
          body: { device: device.trim(), password },
        }),
      ),
    onSuccess: ({ url }) => {
      void queryClient.invalidateQueries({ queryKey: securityKey });
      toast(t("account.apps.appleStarted"), "success");
      window.location.assign(url);
    },
    onError: (error) => {
      if (!(error instanceof Cancelled)) toast(errorText(error), "error");
    },
  });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    create.mutate();
  };

  return (
    <>
      <form className="mt-4 flex flex-col gap-3 border-t border-hairline pt-4" onSubmit={submit}>
        <div>
          <p className="text-sm font-semibold">{t("account.apps.appleTitle")}</p>
          <p className="text-[13px] text-muted">{t("account.apps.appleIntro")}</p>
        </div>
        <div className="flex flex-wrap items-end gap-2">
          <Field label={t("account.apps.appleDevice")} className="min-w-[180px] flex-1">
            {(id) => (
              <TextInput
                id={id}
                required
                maxLength={60}
                value={device}
                onChange={(event) => setDevice(event.target.value)}
              />
            )}
          </Field>
          <Button type="submit" icon={Smartphone} busy={create.isPending} disabled={!device.trim()}>
            {t("account.apps.appleButton")}
          </Button>
        </div>
      </form>
      {dialog}
    </>
  );
}

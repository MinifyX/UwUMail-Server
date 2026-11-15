import { Card, Toggle } from "@uwusuite/design";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useSession } from "@/features/session/session";
import { useT } from "@/i18n";
import { api } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";

/** Whether the server follows parcels from shipping mail. On unless switched off; off also deletes them. */
export function ShipmentsCard() {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const stored = useSession().data?.preferences.shipments !== "off";
  const [pending, setPending] = useState<boolean | null>(null);
  const save = useMutation({
    mutationFn: (on: boolean) =>
      api<Record<string, unknown>>("/api/account/preferences", {
        method: "PATCH",
        body: { shipments: on ? "on" : "off" },
      }),
    onMutate: (on) => setPending(on),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["session"] });
      setPending(null);
      toast(t("account.shipments.saved"), "success");
    },
    onError: (error) => {
      setPending(null);
      toast(errorText(error), "error");
    },
  });

  return (
    <Card title={t("account.shipments.title")}>
      <Toggle
        checked={pending ?? stored}
        disabled={save.isPending}
        onChange={(on) => save.mutate(on)}
        label={t("account.shipments.label")}
        description={t("account.shipments.description")}
      />
    </Card>
  );
}

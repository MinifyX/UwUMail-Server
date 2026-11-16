import { Button, Card, Dialog, Toggle } from "@uwusuite/design";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useSession } from "@/features/session/session";
import { useT } from "@/i18n";
import { api } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";

/** Whether the server follows parcels from shipping mail. On unless switched off; off also deletes them. */
export function ShipmentsCard() {
  // Switched off for the whole server by the admin: nothing to show.
  if (useSession().data?.features?.shipments === false) return null;
  return <ShipmentsSwitch />;
}

/** One on/off switch in the person's preferences, on unless stored as "off". */
function usePreference(key: string) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const stored = useSession().data?.preferences[key] !== "off";
  const [pending, setPending] = useState<boolean | null>(null);
  const save = useMutation({
    mutationFn: (on: boolean) =>
      api<Record<string, unknown>>("/api/account/preferences", {
        method: "PATCH",
        body: { [key]: on ? "on" : "off" },
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
  return { on: pending ?? stored, save };
}

function ShipmentsSwitch() {
  const { t } = useT();
  const { on, save } = usePreference("shipments");
  const lookup = usePreference("shipmentLookup");
  // Offered only when the admin has turned the lookup on and at least one carrier can be asked.
  const lookupOffered = useSession().data?.features?.shipmentLookup === true;
  // Switching off deletes the parcels, so it is asked first.
  const [asking, setAsking] = useState(false);

  return (
    <Card title={t("account.shipments.title")}>
      <Toggle
        checked={on}
        disabled={save.isPending}
        onChange={(next) => (next ? save.mutate(true) : setAsking(true))}
        label={t("account.shipments.label")}
        description={t("account.shipments.description")}
      />
      {lookupOffered && on && (
        <Toggle
          checked={lookup.on}
          disabled={lookup.save.isPending}
          onChange={(next) => lookup.save.mutate(next)}
          label={t("account.shipments.lookupLabel")}
          description={t("account.shipments.lookupDescription")}
        />
      )}
      <Dialog
        open={asking}
        onClose={() => setAsking(false)}
        width="sm"
        tone="warning"
        title={t("account.shipments.confirmTitle")}
        footer={
          <>
            <Button autoFocus onClick={() => setAsking(false)}>
              {t("common.cancel")}
            </Button>
            <Button
              variant="danger"
              onClick={() => {
                setAsking(false);
                save.mutate(false);
              }}
            >
              {t("account.shipments.confirmOff")}
            </Button>
          </>
        }
      >
        <p className="px-6 pb-2 text-sm text-muted">{t("account.shipments.confirmText")}</p>
      </Dialog>
    </Card>
  );
}

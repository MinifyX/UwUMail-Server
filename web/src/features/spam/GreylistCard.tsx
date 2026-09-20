import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, ShieldCheck, Trash2, TriangleAlert } from "lucide-react";
import { useState } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Dialog } from "@/components/ui/Dialog";
import { EmptyState } from "@/components/ui/EmptyState";
import { useT } from "@/i18n";
import { api, type GreylistDecision, type GreylistHold, type GreylistView } from "@/lib/api";
import { formatDateTime } from "@/lib/format";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";

export const greylistKey = ["account", "greylist"] as const;

/** Throwing a message away cannot be taken back, so those two ask first. */
const ASKS_FIRST: GreylistDecision[] = ["discard", "discard-spam"];

/** Who it is from: the name the sender put in the message, falling back to the envelope. */
function sender(hold: GreylistHold) {
  return hold.headerFrom.trim() || hold.envelopeFrom;
}

function Waiting({
  hold,
  busy,
  onDecide,
}: {
  hold: GreylistHold;
  busy: boolean;
  onDecide: (action: GreylistDecision) => void;
}) {
  const { t, i18n } = useT();
  return (
    <li className="flex flex-col gap-3 border-b border-hairline py-4 first:pt-0 last:border-b-0 last:pb-0">
      <div className="flex flex-col gap-0.5">
        <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-0.5">
          <span className="min-w-0 flex-1 truncate text-[14px] font-semibold">{sender(hold)}</span>
          <span className="text-[12px] text-muted tabular-nums">{formatDateTime(hold.at, i18n.language)}</span>
        </div>
        <span className="text-[13px] break-words text-muted">
          {hold.subject ?? <em className="not-italic opacity-70">{t("spam.greylist.noSubject")}</em>}
        </span>
        <span className="text-[12px] text-muted">{t("spam.greylist.sentTo", { address: hold.address })}</span>
      </div>
      <div className="flex flex-wrap gap-2">
        <Button variant="primary" icon={Check} busy={busy} onClick={() => onDecide("deliver")}>
          {t("spam.greylist.decide.deliver")}
        </Button>
        <Button icon={ShieldCheck} busy={busy} onClick={() => onDecide("allow-deliver")}>
          {t("spam.greylist.decide.allowDeliver")}
        </Button>
        <Button icon={Trash2} busy={busy} onClick={() => onDecide("discard")}>
          {t("spam.greylist.decide.discard")}
        </Button>
        <Button icon={TriangleAlert} busy={busy} onClick={() => onDecide("discard-spam")}>
          {t("spam.greylist.decide.discardSpam")}
        </Button>
      </div>
    </li>
  );
}

/** Asks before a message is thrown away, because the sender's retry will not bring it back. */
function ConfirmDiscard({
  pending,
  onConfirm,
  onCancel,
}: {
  pending: { hold: GreylistHold; action: GreylistDecision } | null;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const { t } = useT();
  return (
    <Dialog open={pending !== null} onClose={onCancel} title={t("spam.greylist.confirm.title")} width="sm">
      {pending && (
        <div className="flex flex-col gap-4 px-6 pb-6">
          <p className="text-[14px]">
            {pending.action === "discard-spam" ? t("spam.greylist.confirm.bodySpam") : t("spam.greylist.confirm.body")}
          </p>
          <p className="rounded-control bg-canvas px-3 py-2 text-[13px]">
            <span className="font-semibold">{sender(pending.hold)}</span>
            <br />
            {pending.hold.subject ?? t("spam.greylist.noSubject")}
          </p>
          <div className="flex flex-wrap justify-end gap-2">
            <Button onClick={onCancel}>{t("spam.greylist.confirm.keep")}</Button>
            <Button variant="danger" icon={Trash2} onClick={onConfirm}>
              {t("spam.greylist.confirm.discard")}
            </Button>
          </div>
        </div>
      )}
    </Dialog>
  );
}

/**
 * What greylisting is holding back for this person, and what they decide to do with it.
 *
 * The list shows who wrote and what about, and nothing more. Reading a waiting message means
 * delivering it to one's own mailbox first — a settings page is the wrong place to render a message
 * the filter just called suspicious.
 */
export function GreylistCard() {
  const { t } = useT();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const [pending, setPending] = useState<{ hold: GreylistHold; action: GreylistDecision } | null>(null);
  const [busyId, setBusyId] = useState<number | null>(null);
  const query = useQuery({ queryKey: greylistKey, queryFn: () => api<GreylistView>("/api/account/greylist") });

  const decide = useMutation({
    mutationFn: ({ hold, action }: { hold: GreylistHold; action: GreylistDecision }) =>
      api<GreylistView>(`/api/account/greylist/${hold.id}`, { method: "POST", body: { action } }),
    onMutate: ({ hold }) => setBusyId(hold.id),
    onSuccess: (view, { action }) => {
      queryClient.setQueryData(greylistKey, view);
      const delivered = action === "deliver" || action === "allow-deliver";
      toast(t(delivered ? "spam.greylist.done.delivered" : "spam.greylist.done.discarded"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
    onSettled: () => setBusyId(null),
  });

  const run = (hold: GreylistHold, action: GreylistDecision) => {
    if (ASKS_FIRST.includes(action)) {
      setPending({ hold, action });
      return;
    }
    decide.mutate({ hold, action });
  };

  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  const { enabled, waiting } = query.data;

  return (
    <Card title={t("spam.greylist.title")}>
      <div className="flex flex-col gap-4">
        <p className="-mt-1 max-w-prose text-[13px] text-muted">{t("spam.greylist.explain")}</p>
        {!enabled && <p className="rounded-control bg-canvas px-3 py-2 text-[13px]">{t("spam.greylist.off")}</p>}
        {waiting.length === 0 ? (
          <EmptyState
            scene="done"
            compact
            title={t("spam.greylist.empty")}
            body={enabled ? t("spam.greylist.emptyHint") : undefined}
          />
        ) : (
          <ul className="flex flex-col">
            {waiting.map((hold) => (
              <Waiting key={hold.id} hold={hold} busy={busyId === hold.id} onDecide={(action) => run(hold, action)} />
            ))}
          </ul>
        )}
      </div>
      <ConfirmDiscard
        pending={pending}
        onCancel={() => setPending(null)}
        onConfirm={() => {
          if (pending) decide.mutate(pending);
          setPending(null);
        }}
      />
    </Card>
  );
}

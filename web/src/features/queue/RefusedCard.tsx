import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Trash2 } from "lucide-react";
import { useEffect } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { EmptyState } from "@/components/ui/EmptyState";
import { useT } from "@/i18n";
import { api, type RefusedMail } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDateTime } from "@/lib/format";
import { toast } from "@/state/toasts";

/** Where `/admin/queue#refused` scrolls to; alerts about refused mail link here. */
export const REFUSED_ID = "refused";
const REFUSED_KEY = ["admin", "queue", "refused"];

export function RefusedList({ refused }: { refused: RefusedMail[] }) {
  const { t, i18n } = useT();
  return (
    <ul className="flex flex-col">
      {refused.map((mail) => (
        <li key={mail.id} className="flex flex-col gap-1.5 border-t border-hairline py-3">
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
            <time className="text-[12px] text-muted" dateTime={new Date(mail.at * 1000).toISOString()}>
              {formatDateTime(mail.at, i18n.language)}
            </time>
            {mail.gaveUp && (
              <span className="inline-flex h-6 items-center rounded-full bg-warning-tint px-2.5 text-[12px] font-semibold text-warning">
                {t("queue.refused.gaveUp")}
              </span>
            )}
          </div>
          <p className="text-sm break-words">
            <span className="text-muted">{t("queue.refused.from")}</span> {mail.from || t("queue.bounce")}{" "}
            <span className="text-muted">{t("queue.refused.to")}</span> <span className="font-medium">{mail.to}</span>
          </p>
          <code className="rounded-control bg-canvas px-2.5 py-1.5 font-mono text-[12px] break-words whitespace-pre-wrap text-muted">
            {mail.error}
          </code>
        </li>
      ))}
    </ul>
  );
}

/** The outgoing mails of the last 30 days that another server would not take. */
export function RefusedCard() {
  const { t } = useT();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const refused = useQuery({
    queryKey: REFUSED_KEY,
    queryFn: () => api<{ refused: RefusedMail[] }>("/api/admin/queue/refused"),
  });
  const clear = useMutation({
    mutationFn: () => api<void>("/api/admin/queue/refused", { method: "DELETE" }),
    onSuccess: () => {
      toast(t("queue.refused.cleared"), "success");
      void queryClient.invalidateQueries({ queryKey: REFUSED_KEY });
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  // Coming from an alert: once the list has its real height, bring it into view.
  const loaded = !refused.isPending;
  useEffect(() => {
    if (loaded && window.location.hash === `#${REFUSED_ID}`) {
      document.getElementById(REFUSED_ID)?.scrollIntoView({ block: "start" });
    }
  }, [loaded]);

  const list = refused.data?.refused ?? [];

  return (
    <Card
      id={REFUSED_ID}
      className="scroll-mt-4"
      title={t("queue.refused.title")}
      action={
        list.length > 0 && (
          <Button
            size="sm"
            variant="danger"
            icon={Trash2}
            busy={clear.isPending}
            onClick={() => {
              if (window.confirm(t("queue.refused.clearConfirm"))) clear.mutate();
            }}
          >
            {t("queue.refused.clear")}
          </Button>
        )
      }
    >
      <p className="mb-3 text-sm text-muted">{t("queue.refused.hint")}</p>
      {refused.isPending ? (
        <Loading />
      ) : refused.isError ? (
        <LoadError error={refused.error} onRetry={() => void refused.refetch()} />
      ) : list.length === 0 ? (
        <EmptyState compact scene="inbox" title={t("queue.refused.empty.title")} body={t("queue.refused.empty.body")} />
      ) : (
        <RefusedList refused={list} />
      )}
    </Card>
  );
}

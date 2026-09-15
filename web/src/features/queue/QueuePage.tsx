import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import clsx from "clsx";
import { RotateCw, Trash2 } from "lucide-react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { PageHeader } from "@/components/ui/Card";
import { EmptyState } from "@/components/ui/EmptyState";
import { useT } from "@/i18n";
import { api, type QueuedMessage, type QueueRecipient } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatBytes, formatDateTime, formatRelative } from "@/lib/format";
import { usePrefs } from "@/state/prefs";
import { toast } from "@/state/toasts";

const STATUS_STYLES: Record<QueueRecipient["status"], string> = {
  pending: "bg-warning-tint text-warning",
  delivered: "bg-success-tint text-success",
  failed: "bg-danger-tint text-danger",
};

function useQueueAction(run: (id: number) => Promise<void>, success: (id: number) => string) {
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: run,
    onSuccess: (_, id) => {
      toast(success(id), "success");
      void queryClient.invalidateQueries({ queryKey: ["admin", "queue"] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "overview"] });
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}

function Message({ message }: { message: QueuedMessage }) {
  const { t, i18n } = useT();
  const pro = usePrefs((s) => s.mode) === "pro";
  const retry = useQueueAction(
    (id) => api<void>(`/api/admin/queue/${id}/retry`, { method: "POST", body: {} }),
    () => t("queue.toasts.retry"),
  );
  const drop = useQueueAction(
    (id) => api<void>(`/api/admin/queue/${id}`, { method: "DELETE" }),
    (id) => t("queue.toasts.dropped", { id }),
  );
  const pending = message.recipients.some((recipient) => recipient.status === "pending");

  return (
    <li className="rounded-card border border-hairline bg-surface p-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="truncate font-semibold">
            {pro && <span className="mr-2 text-muted">#{message.id}</span>}
            {message.from || t("queue.bounce")}
          </p>
          <p className="text-[12px] text-muted">
            {formatBytes(message.size, i18n.language)} ·{" "}
            {t("queue.created", { time: formatRelative(message.createdAt, i18n.language) })} ·{" "}
            {t("queue.expires", { time: formatDateTime(message.expiresAt, i18n.language) })}
          </p>
        </div>
        <div className="flex gap-2">
          {pending && (
            <Button size="sm" icon={RotateCw} busy={retry.isPending} onClick={() => retry.mutate(message.id)}>
              {t("queue.retry")}
            </Button>
          )}
          <Button
            size="sm"
            variant="danger"
            icon={Trash2}
            busy={drop.isPending}
            onClick={() => {
              if (window.confirm(t("queue.dropConfirm", { id: message.id }))) drop.mutate(message.id);
            }}
          >
            {t("queue.drop")}
          </Button>
        </div>
      </div>
      <ul className="mt-3 flex flex-col">
        {message.recipients.map((recipient) => (
          <li key={recipient.address} className="flex flex-col gap-1 border-t border-hairline py-2.5">
            <div className="flex flex-wrap items-center gap-2">
              <span
                className={clsx(
                  "inline-flex h-6 items-center rounded-full px-2.5 text-[12px] font-semibold",
                  STATUS_STYLES[recipient.status],
                )}
              >
                {t(`queue.status.${recipient.status}`)}
              </span>
              <span className="min-w-0 truncate text-sm font-medium">{recipient.address}</span>
              <span className="text-[12px] text-muted">{t("queue.attempts", { count: recipient.attempts })}</span>
              {recipient.status === "pending" && (
                <span className="text-[12px] text-muted">
                  · {t("queue.nextAttempt", { time: formatRelative(recipient.nextAttemptAt, i18n.language) })}
                </span>
              )}
            </div>
            {recipient.lastError && (
              <code className="rounded-control bg-canvas px-2.5 py-1.5 text-[12px] break-words text-muted">
                {recipient.lastError}
              </code>
            )}
          </li>
        ))}
      </ul>
    </li>
  );
}

export function QueuePage() {
  const { t } = useT();
  const queue = useQuery({
    queryKey: ["admin", "queue"],
    queryFn: () => api<QueuedMessage[]>("/api/admin/queue"),
    refetchInterval: 15_000,
  });

  if (queue.isPending) return <Loading />;
  if (queue.isError) return <LoadError error={queue.error} onRetry={() => void queue.refetch()} />;

  return (
    <div className="flex flex-col gap-5">
      <PageHeader title={t("queue.title")} intro={t("queue.intro")} />
      {queue.data.length === 0 ? (
        <EmptyState scene="inbox" title={t("queue.empty.title")} body={t("queue.empty.body")} />
      ) : (
        <ul className="flex flex-col gap-3">
          {queue.data.map((message) => (
            <Message key={message.id} message={message} />
          ))}
        </ul>
      )}
    </div>
  );
}

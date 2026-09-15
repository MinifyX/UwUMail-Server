import { useMutation, useQuery } from "@tanstack/react-query";
import { Check, X } from "lucide-react";
import type { ReactNode } from "react";
import { NyuScene } from "@/components/nyu/scenes";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { EmptyState } from "@/components/ui/EmptyState";
import { Wordmark } from "@/components/ui/Logo";
import { useT } from "@/i18n";
import { api, ApiError, type ForwardLinkInfo } from "@/lib/api";
import { useErrorText } from "@/lib/errors";

/**
 * Where the owner of an address agrees (or not) to get someone's forwarded mail. Needs no login:
 * the address may belong to anyone. Opening the link changes nothing, so link scanners are harmless.
 */
export function ForwardConfirmPage({ token }: { token: string }) {
  const { t } = useT();
  const errorText = useErrorText();
  const path = `/api/forwarding-links/${encodeURIComponent(token)}`;
  const link = useQuery({
    queryKey: ["forwarding-link", token],
    queryFn: () => api<ForwardLinkInfo>(path),
    retry: false,
  });
  const answer = useMutation({
    mutationFn: (confirm: boolean) =>
      api<ForwardLinkInfo>(`${path}/${confirm ? "confirm" : "decline"}`, { method: "POST", body: {} }).then(
        (result) => ({ ...result, confirm }),
      ),
  });

  const wrapper = (content: ReactNode) => (
    <main className="flex min-h-screen items-center justify-center px-4 py-10">
      <div className="w-full max-w-[440px] animate-slide-up">
        <div className="mb-6 flex justify-center">
          <Wordmark className="text-2xl" />
        </div>
        {content}
      </div>
    </main>
  );

  if (answer.data) {
    return wrapper(
      <EmptyState
        scene={answer.data.confirm ? "done" : "emptyFolder"}
        title={answer.data.confirm ? t("forwardConfirm.confirmedTitle") : t("forwardConfirm.declinedTitle")}
        body={
          answer.data.confirm
            ? t("forwardConfirm.confirmedBody", { from: answer.data.from, address: answer.data.address })
            : t("forwardConfirm.declinedBody", { from: answer.data.from })
        }
      />,
    );
  }
  if (link.isPending) return <Loading fullPage />;
  if (link.error instanceof ApiError && ["linkInvalid", "notFound"].includes(link.error.code)) {
    return wrapper(
      <EmptyState scene="loadError" title={t("forwardConfirm.invalidTitle")} body={t("forwardConfirm.invalidBody")} />,
    );
  }
  if (link.isError) return <LoadError error={link.error} onRetry={() => void link.refetch()} />;

  const info = link.data;
  return wrapper(
    <div className="rounded-[22px] border border-hairline bg-surface px-6 pt-4 pb-7 shadow-float sm:px-8">
      <NyuScene name="inbox" className="mx-auto h-auto w-[180px]" />
      <h1 className="mt-1 text-center text-[20px] font-bold">{t("forwardConfirm.title")}</h1>
      <p className="mt-2 text-center text-sm text-muted">
        {t("forwardConfirm.body", { from: info.from, address: info.address })}
      </p>
      <p className="mt-2 text-center text-[13px] text-faint">{t("forwardConfirm.hint")}</p>
      {answer.isError && (
        <p role="alert" className="mt-3 text-center text-[13px] text-danger">
          {errorText(answer.error)}
        </p>
      )}
      <div className="mt-5 flex flex-col gap-2">
        <Button
          variant="primary"
          size="lg"
          icon={Check}
          busy={answer.isPending && answer.variables === true}
          onClick={() => answer.mutate(true)}
        >
          {t("forwardConfirm.confirm")}
        </Button>
        <Button
          size="lg"
          icon={X}
          busy={answer.isPending && answer.variables === false}
          onClick={() => answer.mutate(false)}
        >
          {t("forwardConfirm.decline")}
        </Button>
      </div>
    </div>,
  );
}

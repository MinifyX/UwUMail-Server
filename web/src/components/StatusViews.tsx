import { RotateCw } from "lucide-react";
import { EmptyState } from "@/components/ui/EmptyState";
import { Button } from "@/components/ui/Button";
import { LogoSymbol } from "@/components/ui/Logo";
import { useT } from "@/i18n";
import { ApiError } from "@/lib/api";

export function Loading({ fullPage = false }: { fullPage?: boolean }) {
  const { t } = useT();
  return (
    <div
      role="status"
      className={
        fullPage
          ? "flex min-h-screen flex-col items-center justify-center gap-3"
          : "flex flex-col items-center gap-3 py-16"
      }
    >
      <LogoSymbol className="nyu-blink h-14 w-auto animate-pulse" />
      <span className="text-[13px] text-muted">{t("common.loading")}</span>
    </div>
  );
}

export function LoadError({ error, onRetry }: { error: unknown; onRetry: () => void }) {
  const { t } = useT();
  const offline = error instanceof ApiError && error.code === "offline";
  const key = offline ? "errors.offline" : "errors.loadFailed";
  return (
    <EmptyState
      scene={offline ? "offline" : "loadError"}
      title={t(`${key}.title`)}
      body={t(`${key}.body`)}
      action={
        <Button icon={RotateCw} onClick={onRetry}>
          {t("common.retry")}
        </Button>
      }
    />
  );
}

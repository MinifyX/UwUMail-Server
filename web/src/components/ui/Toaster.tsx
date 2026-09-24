import clsx from "clsx";
import { CircleAlert, CircleCheck, Mail, X } from "lucide-react";
import { LogoSymbol } from "@/components/ui/Logo";
import { useT } from "@/i18n";
import { useBrand } from "@/state/brand";
import { useToasts } from "@/state/toasts";

const ICONS = { info: Mail, success: CircleCheck, error: CircleAlert } as const;

export function Toaster() {
  const { toasts, dismiss } = useToasts();
  const { t } = useT();
  const mascot = useBrand((s) => s.mascot);

  return (
    <div
      aria-live="polite"
      className="pointer-events-none fixed bottom-5 left-1/2 z-50 flex w-[min(440px,calc(100vw-32px))] -translate-x-1/2 flex-col items-center gap-2 max-[699px]:top-2 max-[699px]:bottom-auto"
    >
      {toasts.map((item) => {
        const Icon = ICONS[item.tone];
        return (
          <div
            key={item.id}
            role={item.tone === "error" ? "alert" : "status"}
            className="pointer-events-auto flex w-full animate-slide-up items-center gap-3 rounded-2xl bg-[#1c1420] py-2.5 pr-2 pl-4 text-[13px] font-medium text-white shadow-float dark:bg-[#f8f2f6] dark:text-[#1c1420]"
          >
            <span className="relative shrink-0">
              <Icon
                className={clsx(
                  "size-[18px]",
                  item.tone === "error"
                    ? "text-[#ff8096] dark:text-danger"
                    : "text-(--uwu-toast-accent) dark:text-pink-solid",
                )}
                aria-hidden
              />
              {item.effect === "sent" && mascot && (
                <LogoSymbol
                  mood="happy"
                  className="nyu-flyer animate-nyu-fly pointer-events-none absolute -top-2 -left-2 h-8 w-auto"
                />
              )}
            </span>
            <span className="flex-1">{item.message}</span>
            {item.action && (
              <button
                type="button"
                onClick={() => {
                  dismiss(item.id);
                  item.action?.run();
                }}
                className="shrink-0 rounded-full px-3 py-1.5 text-[13px] font-bold text-(--uwu-toast-accent) hover:bg-white/10 dark:text-pink-ink dark:hover:bg-black/5"
              >
                {item.action.label}
              </button>
            )}
            <button
              type="button"
              aria-label={t("common.close")}
              onClick={() => dismiss(item.id)}
              className="grid size-7 place-items-center rounded-full opacity-70 hover:bg-white/10 hover:opacity-100 dark:hover:bg-black/10"
            >
              <X className="size-4" aria-hidden />
            </button>
          </div>
        );
      })}
    </div>
  );
}

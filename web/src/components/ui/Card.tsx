import clsx from "clsx";
import { Check, Copy, type LucideIcon } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { useT } from "@/i18n";
import { IconButton } from "./Button";

export function Card({
  title,
  intro,
  icon: Icon,
  action,
  children,
  className,
  id,
}: {
  title?: ReactNode;
  /** One line under the title saying what the card is for. */
  intro?: ReactNode;
  /** A tile with an icon before the title. */
  icon?: LucideIcon;
  action?: ReactNode;
  children: ReactNode;
  className?: string;
  /** For links and scrolling to the card. */
  id?: string;
}) {
  return (
    <section id={id} className={clsx("rounded-card border border-hairline bg-surface p-5", className)}>
      {(title || action) && (
        <header className={clsx("flex items-center justify-between gap-3", intro || Icon ? "mb-4" : "mb-3")}>
          {Icon && (
            <span className="flex size-10 shrink-0 items-center justify-center self-start rounded-xl bg-pink-tint text-pink-ink">
              <Icon className="size-5" aria-hidden />
            </span>
          )}
          <div className="min-w-0 flex-1">
            {title && <h2 className="text-[15px] font-bold">{title}</h2>}
            {intro && <p className="mt-0.5 text-[13px] text-muted">{intro}</p>}
          </div>
          {action}
        </header>
      )}
      {children}
    </section>
  );
}

export function PageHeader({
  title,
  intro,
  art,
  eyebrow,
  actions,
  large,
}: {
  title: ReactNode;
  intro?: ReactNode;
  art?: ReactNode;
  /** A small line above the title, such as today's date. */
  eyebrow?: ReactNode;
  /** The page's main action(s), beside the title (below it on a narrow screen). */
  actions?: ReactNode;
  /** The big title of an overview; on a phone every page gets it. */
  large?: boolean;
}) {
  return (
    <header className="flex flex-wrap items-center justify-between gap-x-6 gap-y-3">
      <div className="min-w-[min(200px,100%)] flex-1">
        {eyebrow && <p className="text-[13px] text-muted">{eyebrow}</p>}
        <h1
          className={clsx(
            "leading-tight tracking-[-0.01em] max-[699px]:text-[28px] max-[699px]:font-extrabold",
            large ? "mt-0.5 text-[28px] font-extrabold" : "text-[22px] font-bold",
          )}
        >
          {title}
        </h1>
        {intro && <div className="mt-1 max-w-[62ch] text-sm text-muted">{intro}</div>}
      </div>
      {actions && <div className="flex flex-wrap items-center gap-2">{actions}</div>}
      {art && <div className="hidden shrink-0 sm:block">{art}</div>}
    </header>
  );
}

export function CopyButton({ value, label }: { value: string; label?: string }) {
  const { t } = useT();
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1500);
    return () => window.clearTimeout(timer);
  }, [copied]);
  return (
    <IconButton
      size="sm"
      icon={copied ? Check : Copy}
      label={copied ? t("common.copied") : (label ?? t("common.copy"))}
      active={copied}
      onClick={() => {
        void navigator.clipboard?.writeText(value).then(() => setCopied(true));
      }}
    />
  );
}

/** A label and its value; with `copy`, the value gets a copy button. */
export function KeyValue({ label, value, copy }: { label: ReactNode; value: ReactNode; copy?: string }) {
  return (
    <div className="flex min-h-10 items-center justify-between gap-4 border-b border-hairline py-1.5 last:border-b-0">
      <span className="shrink-0 text-[13px] text-muted">{label}</span>
      <span className="flex min-w-0 items-center gap-1 text-right text-sm font-medium break-all">
        {value}
        {copy && <CopyButton value={copy} />}
      </span>
    </div>
  );
}

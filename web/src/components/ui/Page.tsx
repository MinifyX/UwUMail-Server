import { useEffect, useState, type ReactNode } from "react";
import { useT } from "@/i18n";
import { IconButton, ICONS, cx } from "@uwusuite/design";

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
          className={cx(
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
      icon={copied ? ICONS.done : ICONS.copy}
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

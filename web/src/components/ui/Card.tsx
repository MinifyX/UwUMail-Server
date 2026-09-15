import clsx from "clsx";
import { Check, Copy } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { useT } from "@/i18n";
import { IconButton } from "./Button";

export function Card({
  title,
  action,
  children,
  className,
}: {
  title?: ReactNode;
  action?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={clsx("rounded-card border border-hairline bg-surface p-5", className)}>
      {(title || action) && (
        <header className="mb-3 flex items-center justify-between gap-3">
          {title && <h2 className="text-[15px] font-bold">{title}</h2>}
          {action}
        </header>
      )}
      {children}
    </section>
  );
}

export function PageHeader({ title, intro, art }: { title: ReactNode; intro?: ReactNode; art?: ReactNode }) {
  return (
    <header className="flex items-center justify-between gap-6">
      <div className="min-w-0">
        <h1 className="text-[22px] font-bold tracking-[-0.01em]">{title}</h1>
        {intro && <p className="mt-1 text-sm text-muted">{intro}</p>}
      </div>
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

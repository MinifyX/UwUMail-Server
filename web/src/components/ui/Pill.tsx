import clsx from "clsx";
import type { ButtonHTMLAttributes, ReactNode } from "react";

interface PillProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  active?: boolean;
  count?: number;
}

export function Pill({ active, count, className, children, ...rest }: PillProps) {
  return (
    <button
      type="button"
      aria-pressed={active}
      className={clsx(
        "inline-flex h-8 shrink-0 items-center gap-1.5 rounded-full px-3.5 text-[13px] font-medium transition-colors duration-150",
        active ? "bg-pink-tint text-pink-ink" : "border border-line text-muted hover:border-faint/60 hover:text-ink",
        className,
      )}
      {...rest}
    >
      {children}
      {count !== undefined && count > 0 && (
        <span className="rounded-full bg-pink px-1.5 text-[11px] leading-[18px] font-bold text-white">{count}</span>
      )}
    </button>
  );
}

export function Badge({ count, className }: { count: number; className?: string }) {
  if (count <= 0) return null;
  return (
    <span
      className={clsx(
        "inline-flex h-[18px] min-w-[18px] items-center justify-center rounded-full bg-pink px-1.5 text-[11px] font-bold text-white",
        className,
      )}
    >
      {count > 999 ? "999+" : count}
    </span>
  );
}

export type TagTone = "pink" | "success" | "warning" | "danger" | "info" | "neutral";

const TAG_TONES: Record<TagTone, string> = {
  pink: "bg-pink-tint text-pink-ink",
  success: "bg-success-tint text-success-ink",
  warning: "bg-warning-tint text-warning-ink",
  danger: "bg-danger-tint text-danger-ink",
  info: "bg-canvas text-ink",
  neutral: "bg-canvas text-muted",
};

/** A short state word beside something, such as "Admin", "Locked" or "New". The word carries it, not the colour. */
export function Tag({ tone = "neutral", className, children }: { tone?: TagTone; className?: string; children: ReactNode }) {
  return (
    <span
      className={clsx(
        "inline-flex h-[22px] shrink-0 items-center gap-1 rounded-full px-2 text-[12px] leading-none font-bold whitespace-nowrap",
        TAG_TONES[tone],
        className,
      )}
    >
      {children}
    </span>
  );
}

/** A state as a dot and a word: never colour alone. */
export function StatusDot({ tone, className }: { tone: "success" | "warning" | "danger" | "offline"; className?: string }) {
  const color = { success: "bg-success", warning: "bg-warning", danger: "bg-danger", offline: "bg-offline" }[tone];
  return <span className={clsx("inline-block size-2 shrink-0 rounded-full", color, className)} aria-hidden />;
}

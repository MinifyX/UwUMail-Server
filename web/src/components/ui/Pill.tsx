import clsx from "clsx";
import type { ButtonHTMLAttributes } from "react";

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

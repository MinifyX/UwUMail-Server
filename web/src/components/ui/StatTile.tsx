import clsx from "clsx";
import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";

export type StatTone = "normal" | "warning" | "danger";

const ICON_TONES: Record<StatTone, string> = {
  normal: "text-pink-ink",
  warning: "text-warning-ink",
  danger: "text-danger-ink",
};

/**
 * One number on an overview: the icon, the value big, what it is below. The tone colours only the
 * icon; the note says in words what is wrong, so colour is never the only hint.
 */
export function StatTile({
  icon: Icon,
  label,
  value,
  note,
  tone = "normal",
  children,
  className,
}: {
  icon?: LucideIcon;
  label: ReactNode;
  value: ReactNode;
  note?: ReactNode;
  tone?: StatTone;
  /** Something under the value, such as a bar. */
  children?: ReactNode;
  className?: string;
}) {
  return (
    <div className={clsx("flex min-w-0 flex-col rounded-card border border-hairline bg-surface p-4", className)}>
      {Icon && <Icon className={clsx("mb-2 size-5", ICON_TONES[tone])} aria-hidden />}
      <p className="truncate text-[20px] leading-tight font-bold tracking-[-0.01em] tabular-nums">{value}</p>
      <p className="mt-0.5 truncate text-[13px] text-muted">{label}</p>
      {children && <div className="mt-2">{children}</div>}
      {note && <p className="mt-1 text-[12px] text-muted">{note}</p>}
    </div>
  );
}

/** How full something is, as a thin bar; the number beside it says the same in words. */
export function MeterBar({ share, className }: { share: number; className?: string }) {
  const clamped = Math.max(0, Math.min(1, share));
  return (
    <div className={clsx("h-1.5 overflow-hidden rounded-full bg-canvas", className)} aria-hidden>
      <div
        className={clsx("h-full rounded-full", clamped > 0.9 ? "bg-danger" : clamped > 0.75 ? "bg-warning" : "bg-pink")}
        style={{ width: `${Math.max(clamped * 100, clamped > 0 ? 2 : 0)}%` }}
      />
    </div>
  );
}

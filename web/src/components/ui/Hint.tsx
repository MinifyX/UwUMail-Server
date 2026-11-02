import clsx from "clsx";
import { CircleAlert, CircleCheck, Info, type LucideIcon } from "lucide-react";
import type { ReactNode } from "react";

type Tone = "info" | "success" | "warning";

const TONES: Record<Tone, { className: string; icon: LucideIcon }> = {
  info: { className: "bg-canvas text-ink [&>svg]:text-pink-ink", icon: Info },
  success: { className: "bg-success-tint text-ink [&>svg]:text-success-ink", icon: CircleCheck },
  warning: { className: "bg-warning-tint text-ink [&>svg]:text-warning-ink", icon: CircleAlert },
};

/** A short note inside a card: an icon in the tone's colour and plain text beside it. */
export function Hint({
  tone = "info",
  icon,
  children,
  className,
}: {
  tone?: Tone;
  icon?: LucideIcon;
  children: ReactNode;
  className?: string;
}) {
  const Icon = icon ?? TONES[tone].icon;
  return (
    <div className={clsx("flex gap-2.5 rounded-control px-3.5 py-3 text-[13px]", TONES[tone].className, className)}>
      <Icon className="mt-px size-4 shrink-0" aria-hidden />
      <div className="min-w-0">{children}</div>
    </div>
  );
}

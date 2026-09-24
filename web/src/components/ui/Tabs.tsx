import clsx from "clsx";
import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import { Link } from "@/lib/router";

export interface Tab<T extends string> {
  value: T;
  label: ReactNode;
  to: string;
  icon?: LucideIcon;
  /** A small number beside the label, e.g. waiting messages. */
  count?: number;
}

/**
 * Tabs that are links, so every tab has its own address. On a narrow screen the row scrolls
 * sideways instead of wrapping, and the current tab stays where it is.
 */
export function Tabs<T extends string>({ tabs, value, label }: { tabs: Tab<T>[]; value: T; label: string }) {
  return (
    <nav aria-label={label} className="-mx-1 [scrollbar-width:none] overflow-x-auto px-1 pb-1">
      <ul className="inline-flex min-w-max gap-1 rounded-full bg-canvas p-1">
        {tabs.map((tab) => {
          const active = tab.value === value;
          const Icon = tab.icon;
          return (
            <li key={tab.value}>
              <Link
                to={tab.to}
                aria-current={active ? "page" : undefined}
                className={clsx(
                  "inline-flex h-8 items-center gap-1.5 rounded-full px-3.5 text-[13px] font-semibold whitespace-nowrap transition-colors",
                  active ? "bg-surface text-pink-ink shadow-sm" : "text-muted hover:text-ink",
                )}
              >
                {Icon && <Icon className="size-3.5" aria-hidden />}
                {tab.label}
                {tab.count !== undefined && tab.count > 0 && (
                  <span className="rounded-full bg-pink px-1.5 text-[11px] leading-[18px] font-bold text-white">
                    {tab.count > 999 ? "999+" : tab.count}
                  </span>
                )}
              </Link>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}

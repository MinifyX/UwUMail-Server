import clsx from "clsx";
import { Mail } from "lucide-react";
import { Nyu, type NyuMood } from "@/components/nyu/Nyu";
import { DEFAULT_BRAND, useBrand } from "@/state/brand";

interface LogoSymbolProps {
  className?: string;
  title?: string;
  mood?: NyuMood;
  /** Changing this number makes Nyu hop once, e.g. when new mail arrives. */
  hop?: number;
  /** Shows Nyu or not regardless of the brand in use, e.g. in a preview of unsaved settings. */
  mascot?: boolean;
}

/**
 * The server's symbol: the uploaded logo, or Nyu, the envelope cat, without the app icon's tile.
 * Without logo and mascot it is a plain envelope in the accent colour.
 */
export function LogoSymbol({ className, title, mood, hop = 0, mascot: override }: LogoSymbolProps) {
  const logo = useBrand((s) => s.logo);
  const mascot = useBrand((s) => override ?? s.mascot);
  if (logo) {
    return (
      <img
        key={hop}
        src={logo}
        alt={title ?? ""}
        aria-hidden={!title}
        className={clsx("object-contain", hop > 0 && "origin-bottom animate-nyu-hop", className)}
      />
    );
  }
  if (!mascot) {
    return (
      <Mail
        key={hop}
        className={clsx("text-pink", hop > 0 && "origin-bottom animate-nyu-hop", className)}
        strokeWidth={2.2}
        role={title ? "img" : undefined}
        aria-label={title}
        aria-hidden={!title}
      />
    );
  }
  return (
    <svg
      key={hop}
      viewBox="56 40 400 388"
      className={clsx("nyu-host overflow-visible", hop > 0 && "origin-bottom animate-nyu-hop", className)}
      role={title ? "img" : undefined}
      aria-hidden={!title}
    >
      {title && <title>{title}</title>}
      <Nyu mood={mood} tilt={-6} />
    </svg>
  );
}

/** Symbol and name: "UwUMail" with its pink "UwU", or the name an admin chose. */
export function Wordmark({ className, hop }: { className?: string; hop?: number }) {
  const name = useBrand((s) => s.name);
  return (
    <span className={clsx("inline-flex min-w-0 items-center gap-2 font-extrabold tracking-[-0.02em]", className)}>
      <LogoSymbol className="h-[1.3em] w-auto max-w-[3em] shrink-0" hop={hop} />
      {name === DEFAULT_BRAND.name ? (
        <span>
          <span className="text-pink">UwU</span>Mail
        </span>
      ) : (
        <span className="truncate">{name}</span>
      )}
    </span>
  );
}

import clsx from "clsx";
import { Nyu, type NyuMood } from "@/components/nyu/Nyu";

interface LogoSymbolProps {
  className?: string;
  title?: string;
  mood?: NyuMood;
  /** Changing this number makes Nyu hop once, e.g. when new mail arrives. */
  hop?: number;
}

/** The UwUMail symbol: Nyu, the envelope cat, without the app icon's tile. */
export function LogoSymbol({ className, title, mood, hop = 0 }: LogoSymbolProps) {
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

export function Wordmark({ className, hop }: { className?: string; hop?: number }) {
  return (
    <span className={clsx("inline-flex items-center gap-2 font-extrabold tracking-[-0.02em]", className)}>
      <LogoSymbol className="h-[1.3em] w-auto" hop={hop} />
      <span>
        UwU<span className="text-pink">Mail</span>
      </span>
    </span>
  );
}

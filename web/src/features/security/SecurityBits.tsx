import { Check, Copy } from "lucide-react";
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/Button";
import { useT } from "@/i18n";
import type { SecurityEventInfo } from "@/lib/api";

/** Draws a QR code from the server's modules ("1" = dark), with the quiet zone scanners need. */
export function QrCode({ size, modules, label }: { size: number; modules: string; label: string }) {
  const quiet = 4;
  let path = "";
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      if (modules[y * size + x] === "1") path += `M${x + quiet} ${y + quiet}h1v1h-1z`;
    }
  }
  const total = size + quiet * 2;
  return (
    <svg
      viewBox={`0 0 ${total} ${total}`}
      role="img"
      aria-label={label}
      className="aspect-square w-full max-w-[220px] rounded-control bg-white"
      shapeRendering="crispEdges"
    >
      <path d={path} fill="#1c1420" />
    </svg>
  );
}

/** Copies text and says so for a moment. */
export function CopyTextButton({ value, label }: { value: string; label: string }) {
  const { t } = useT();
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1800);
    return () => window.clearTimeout(timer);
  }, [copied]);
  return (
    <Button
      size="sm"
      icon={copied ? Check : Copy}
      onClick={() => void navigator.clipboard?.writeText(value).then(() => setCopied(true))}
    >
      {copied ? t("common.copied") : label}
    </Button>
  );
}

export function RecoveryCodesBox({ codes }: { codes: string[] }) {
  const { t } = useT();
  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm text-muted">{t("security.recovery.shownOnce")}</p>
      <ol className="grid grid-cols-2 gap-x-6 gap-y-1.5 rounded-control bg-canvas px-4 py-3 font-mono text-[15px]">
        {codes.map((code) => (
          <li key={code} className="select-all">
            {code}
          </li>
        ))}
      </ol>
      <div>
        <CopyTextButton value={codes.join("\n")} label={t("security.recovery.copy")} />
      </div>
    </div>
  );
}

/** "Firefox on Windows" from a user agent string; good enough to recognise one's own devices. */
export function describeDevice(userAgent: string, t: (key: string, values?: Record<string, string>) => string) {
  const browser = /Edg\//.test(userAgent)
    ? "Edge"
    : /OPR\//.test(userAgent)
      ? "Opera"
      : /Firefox\//.test(userAgent)
        ? "Firefox"
        : /Chrome\//.test(userAgent)
          ? "Chrome"
          : /Safari\//.test(userAgent)
            ? "Safari"
            : null;
  const system = /iPhone/.test(userAgent)
    ? "iPhone"
    : /iPad/.test(userAgent)
      ? "iPad"
      : /Android/.test(userAgent)
        ? "Android"
        : /Windows/.test(userAgent)
          ? "Windows"
          : /Mac OS X|Macintosh/.test(userAgent)
            ? "macOS"
            : /Linux/.test(userAgent)
              ? "Linux"
              : null;
  if (browser && system) return t("security.sessions.device", { browser, system });
  return browser ?? system ?? t("security.sessions.unknownDevice");
}

const KNOWN_EVENTS = [
  "login",
  "passwordChanged",
  "passwordChosenWithLink",
  "passwordSetByAdmin",
  "totpEnabled",
  "totpDisabled",
  "passkeyAdded",
  "passkeyRemoved",
  "recoveryCodesCreated",
  "recoveryCodeUsed",
  "appPasswordCreated",
  "appPasswordRevoked",
  "appsNeedAppPassword",
  "appsMayUseMainPassword",
  "mainPasswordRefused",
  "secondFactorsReset",
  "sessionEnded",
  "sessionsEnded",
];

export function useEventText() {
  const { t } = useT();
  return (event: SecurityEventInfo) => {
    const kind = KNOWN_EVENTS.includes(event.kind) ? event.kind : "other";
    const details = event.details ?? {};
    const method = typeof details.method === "string" ? details.method : "password";
    return t(`security.events.${kind}`, {
      name: typeof details.name === "string" ? details.name : "",
      count: typeof details.count === "number" ? details.count : typeof details.left === "number" ? details.left : 0,
      method: t(`security.loginMethods.${method}`),
      protocol: typeof details.protocol === "string" ? details.protocol.toUpperCase() : "",
      actor: event.actor,
    });
  };
}

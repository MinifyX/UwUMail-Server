import clsx from "clsx";
import { Pause, Play, Search } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { EmptyState } from "@/components/ui/EmptyState";
import { Button } from "@/components/ui/Button";
import { PageHeader } from "@/components/ui/Card";
import { Segmented, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, ApiError, type LogLine } from "@/lib/api";

type Level = "all" | "warn" | "error";
const MAX_LINES = 1000;

const LEVEL_STYLES: Record<LogLine["level"], string> = {
  error: "text-danger",
  warn: "text-warning",
  info: "text-success",
  debug: "text-muted",
  trace: "text-faint",
};

/** The server log, polled every two seconds for new lines. */
export function LogsPage() {
  const { t, i18n } = useT();
  const [level, setLevel] = useState<Level>("all");
  const [search, setSearch] = useState("");
  const [live, setLive] = useState(true);
  const [lines, setLines] = useState<LogLine[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const bottom = useRef<HTMLDivElement>(null);
  const latest = useRef(0);

  useEffect(() => {
    let cancelled = false;
    latest.current = 0;
    const load = async (replace: boolean) => {
      const params = new URLSearchParams({ after: String(replace ? 0 : latest.current), limit: "500" });
      if (level !== "all") params.set("level", level);
      if (search.trim()) params.set("search", search.trim());
      try {
        const result = await api<{ lines: LogLine[]; latest: number }>(`/api/admin/logs?${params}`);
        if (cancelled) return;
        latest.current = result.latest;
        setLines((current) => (replace ? result.lines : [...current, ...result.lines]).slice(-MAX_LINES));
      } catch (error) {
        if (!cancelled && error instanceof ApiError && error.status === 404) setUnavailable(true);
      }
    };
    const debounce = window.setTimeout(() => void load(true), 250);
    const timer = live ? window.setInterval(() => void load(false), 2000) : undefined;
    return () => {
      cancelled = true;
      window.clearTimeout(debounce);
      window.clearInterval(timer);
    };
  }, [level, search, live]);

  useEffect(() => {
    if (live) bottom.current?.scrollIntoView({ block: "end" });
  }, [lines, live]);

  const time = new Intl.DateTimeFormat(i18n.language, { hour: "2-digit", minute: "2-digit", second: "2-digit" });

  return (
    <div className="flex flex-col gap-4">
      <PageHeader title={t("logs.title")} intro={t("logs.intro")} />
      <div className="flex flex-wrap items-center gap-3">
        <Segmented<Level>
          label={t("logs.level.label")}
          value={level}
          onChange={setLevel}
          options={[
            { value: "all", label: t("logs.level.all") },
            { value: "warn", label: t("logs.level.warn") },
            { value: "error", label: t("logs.level.error") },
          ]}
        />
        <label className="relative w-full sm:w-64">
          <span className="sr-only">{t("logs.search")}</span>
          <Search
            className="pointer-events-none absolute top-1/2 left-3.5 size-4 -translate-y-1/2 text-faint"
            aria-hidden
          />
          <TextInput
            type="search"
            className="h-10 pl-10"
            placeholder={t("logs.search")}
            value={search}
            onChange={(event) => setSearch(event.target.value)}
          />
        </label>
        <Button className="sm:ml-auto" icon={live ? Pause : Play} onClick={() => setLive((value) => !value)}>
          {live ? t("logs.pause") : t("logs.resume")}
        </Button>
      </div>

      {unavailable ? (
        <EmptyState compact scene="loadError" title={t("logs.unavailable")} />
      ) : (
        <div className="max-h-[calc(100vh-280px)] min-h-[320px] overflow-auto rounded-card border border-hairline bg-surface p-3 font-mono text-[12px] leading-5">
          {lines.length === 0 ? (
            <p className="p-4 text-center font-sans text-muted">{t("logs.empty")}</p>
          ) : (
            lines.map((line) => (
              <div key={line.seq} className="flex gap-3 rounded px-1 hover:bg-elevated">
                <span className="shrink-0 text-faint">{time.format(new Date(line.at))}</span>
                <span className={clsx("w-11 shrink-0 font-semibold uppercase", LEVEL_STYLES[line.level])}>
                  {line.level}
                </span>
                <span className="min-w-0 break-words">
                  {line.message}
                  {line.fields.map(([key, value]) => (
                    <span key={key} className="ml-2 text-muted">
                      {key}=<span className="text-ink">{value.replace(/^"(.*)"$/, "$1")}</span>
                    </span>
                  ))}
                </span>
              </div>
            ))
          )}
          <div ref={bottom} />
        </div>
      )}
      <p className="text-[12px] text-faint">{live ? t("logs.live") : t("logs.paused")}</p>
    </div>
  );
}

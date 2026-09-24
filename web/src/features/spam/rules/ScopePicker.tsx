import { useQuery } from "@tanstack/react-query";
import clsx from "clsx";
import { Check, ChevronDown, Globe, Search, Server, User, Users } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import { useT } from "@/i18n";
import { api, type RuleScopeCount } from "@/lib/api";
import { formatNumber } from "@/lib/format";
import { scopeText } from "./scope";

/**
 * Picks a scope out of however many domains and people there are: a button that opens a searchable
 * list, each entry with how many rules it has. As a filter it also offers every scope, all domains
 * and all people; as a target only single scopes.
 */
export function ScopePicker({
  value,
  onChange,
  filter = false,
  label,
  className,
}: {
  value: string;
  onChange: (key: string) => void;
  /** Offers "all", "all domains" and "all people" as well. */
  filter?: boolean;
  label: string;
  className?: string;
}) {
  const { t, i18n } = useT();
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  const [debounced, setDebounced] = useState("");
  const root = useRef<HTMLDivElement>(null);
  const listId = useId();

  useEffect(() => {
    const timer = window.setTimeout(() => setDebounced(search), 200);
    return () => window.clearTimeout(timer);
  }, [search]);
  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);

  const query = useQuery({
    queryKey: ["admin", "spam", "scopes", debounced],
    queryFn: () => api<{ scopes: RuleScopeCount[] }>(`/api/admin/spam/scopes?search=${encodeURIComponent(debounced)}`),
    enabled: open,
    staleTime: 30_000,
  });
  const scopes = query.data?.scopes ?? [];
  const domains = scopes.filter((scope) => scope.scope.type === "domain");
  const people = scopes.filter((scope) => scope.scope.type === "account");
  const server = scopes.find((scope) => scope.scope.type === "server");

  const choose = (key: string) => {
    onChange(key);
    setOpen(false);
    setSearch("");
  };
  const option = (key: string, text: string, icon: typeof Globe, count?: number) => {
    const Icon = icon;
    const active = key === value || (key === "all" && value === "");
    return (
      <li key={key}>
        <button
          type="button"
          role="option"
          aria-selected={active}
          onClick={() => choose(key)}
          className={clsx(
            "flex w-full items-center gap-2 rounded-control px-2.5 py-2 text-left text-[13px] hover:bg-pink-tint/60",
            active && "font-semibold text-pink-ink",
          )}
        >
          <Icon className="size-3.5 shrink-0 text-muted" aria-hidden />
          <span className="min-w-0 flex-1 truncate">{text}</span>
          {count !== undefined && (
            <span className="text-[12px] text-muted tabular-nums">{formatNumber(count, i18n.language)}</span>
          )}
          {active && <Check className="size-3.5 text-pink-ink" aria-hidden />}
        </button>
      </li>
    );
  };

  return (
    <div ref={root} className={clsx("relative", className)}>
      <button
        type="button"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={listId}
        aria-label={label}
        onClick={() => setOpen((current) => !current)}
        className="flex h-10 w-full items-center gap-2 rounded-control border border-line bg-surface px-3 text-left text-sm hover:border-faint/60 focus:border-pink focus:shadow-focus focus:outline-none"
      >
        <span className="min-w-0 flex-1 truncate">{scopeText(value, t)}</span>
        <ChevronDown className="size-4 shrink-0 text-muted" aria-hidden />
      </button>
      {open && (
        <div className="absolute z-30 mt-1 w-[min(22rem,calc(100vw-2rem))] rounded-[14px] border border-line bg-surface p-2 shadow-float">
          <label className="mb-1 flex items-center gap-2 rounded-control border border-line px-2.5">
            <Search className="size-3.5 text-muted" aria-hidden />
            <input
              autoFocus
              value={search}
              onChange={(event) => setSearch(event.target.value)}
              placeholder={t("spam.rules.scope.search")}
              aria-label={t("spam.rules.scope.search")}
              className="h-9 min-w-0 flex-1 bg-transparent text-[13px] outline-none"
            />
          </label>
          <ul id={listId} role="listbox" aria-label={label} className="max-h-80 overflow-y-auto">
            {filter && !search && option("all", t("spam.rules.scope.all"), Search)}
            {!search && option("server", t("spam.rules.scope.server"), Server, server?.count)}
            {filter && !search && option("domains", t("spam.rules.scope.domains"), Globe)}
            {filter && !search && option("accounts", t("spam.rules.scope.accounts"), Users)}
            {domains.length > 0 && (
              <li className="px-2.5 pt-2 pb-1 text-[11px] font-bold tracking-wide text-muted uppercase">
                {t("spam.rules.scope.domainsTitle")}
              </li>
            )}
            {domains.map((scope) =>
              option(scope.key, scope.scope.type === "domain" ? scope.scope.name : "", Globe, scope.count),
            )}
            {people.length > 0 && (
              <li className="px-2.5 pt-2 pb-1 text-[11px] font-bold tracking-wide text-muted uppercase">
                {t("spam.rules.scope.accountsTitle")}
              </li>
            )}
            {people.map((scope) =>
              option(scope.key, scope.scope.type === "account" ? scope.scope.name : "", User, scope.count),
            )}
            {query.isPending && <li className="px-2.5 py-2 text-[13px] text-muted">{t("common.loading")}</li>}
            {!query.isPending && search && domains.length + people.length === 0 && (
              <li className="px-2.5 py-2 text-[13px] text-muted">{t("spam.rules.scope.nothing")}</li>
            )}
          </ul>
        </div>
      )}
    </div>
  );
}

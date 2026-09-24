import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import clsx from "clsx";
import {
  ArrowDownUp,
  Ban,
  Check,
  ChevronLeft,
  ChevronRight,
  Clock,
  Download,
  FolderInput,
  Hash,
  Pencil,
  Plus,
  Search,
  SlidersHorizontal,
  Trash2,
  Upload,
  X,
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { LoadError } from "@/components/StatusViews";
import { Button, IconButton } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Dialog } from "@/components/ui/Dialog";
import { Field, Select } from "@/components/ui/Field";
import { Pill } from "@/components/ui/Pill";
import { useT } from "@/i18n";
import {
  api,
  type Rule,
  type RuleKind,
  type RuleListName,
  type RuleSort,
  type RuleState,
  type RulesReport,
  type RulesView,
} from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDate, formatNumber, formatRelative } from "@/lib/format";
import { navigate, usePath, useSearch } from "@/lib/router";
import { toast } from "@/state/toasts";
import { ExpiryField } from "./ExpiryField";
import { ImportDialog } from "./ImportDialog";
import { RuleDialog, type RuleDraft } from "./RuleDialog";
import { ScopePicker } from "./ScopePicker";
import { scopeLabel } from "./scope";

const LISTS: RuleListName[] = ["block", "allow", "points"];
const SENDER_KINDS: RuleKind[] = ["address", "domain", "pattern", "ip", "host"];
const WORD_KINDS: RuleKind[] = ["word", "regex"];
const STATES: RuleState[] = ["temporary", "unused", "stale"];
const SORTS: RuleSort[] = ["value", "created", "hits", "lastHit", "expires"];

/** The table's filters, kept in the address so a link (or the back button) brings them back. */
interface Filters {
  search: string;
  scope: string;
  lists: RuleListName[];
  kinds: RuleKind[];
  state: RuleState | "";
  sort: RuleSort;
  desc: boolean;
  page: number;
  perPage: number;
}

function readFilters(search: string): Filters {
  const params = new URLSearchParams(search);
  const list = <T extends string>(name: string, allowed: readonly T[]) =>
    (params.get(name) ?? "").split(",").filter((item): item is T => (allowed as readonly string[]).includes(item));
  const sort = params.get("sort") as RuleSort | null;
  const state = params.get("state") as RuleState | null;
  return {
    search: params.get("q") ?? "",
    scope: params.get("scope") ?? "all",
    lists: list("list", LISTS),
    kinds: list("kind", [...SENDER_KINDS, ...WORD_KINDS]),
    state: state && STATES.includes(state) ? state : "",
    sort: sort && SORTS.includes(sort) ? sort : "value",
    desc: params.get("desc") === "1",
    page: Math.max(0, Number(params.get("page") ?? 0) || 0),
    perPage: Number(params.get("per") ?? 25) || 25,
  };
}

function writeFilters(filters: Filters): string {
  const params = new URLSearchParams();
  if (filters.search) params.set("q", filters.search);
  if (filters.scope && filters.scope !== "all") params.set("scope", filters.scope);
  if (filters.lists.length) params.set("list", filters.lists.join(","));
  if (filters.kinds.length) params.set("kind", filters.kinds.join(","));
  if (filters.state) params.set("state", filters.state);
  if (filters.sort !== "value") params.set("sort", filters.sort);
  if (filters.desc) params.set("desc", "1");
  if (filters.page) params.set("page", String(filters.page));
  if (filters.perPage !== 25) params.set("per", String(filters.perPage));
  const text = params.toString();
  return text ? `?${text}` : "";
}

/** The query string the API takes for these filters. */
function apiQuery(filters: Filters, admin: boolean): string {
  const params = new URLSearchParams();
  if (filters.search) params.set("search", filters.search);
  if (admin && filters.scope !== "all") params.set("scope", filters.scope);
  if (filters.lists.length) params.set("list", filters.lists.join(","));
  if (filters.kinds.length) params.set("kind", filters.kinds.join(","));
  if (filters.state) params.set("state", filters.state);
  params.set("sort", filters.sort);
  if (filters.desc) params.set("desc", "true");
  params.set("page", String(filters.page));
  params.set("perPage", String(filters.perPage));
  return params.toString();
}

const itemKey = (rule: Pick<Rule, "type" | "id">) => `${rule.type}:${rule.id}`;

function ListBadge({ list }: { list: RuleListName }) {
  const { t } = useT();
  const styles: Record<RuleListName, string> = {
    block: "bg-danger-tint text-danger",
    allow: "bg-success-tint text-success",
    points: "bg-warning-tint text-warning",
  };
  const Icon = list === "block" ? Ban : list === "allow" ? Check : Hash;
  return (
    <span
      className={clsx(
        "inline-flex h-6 shrink-0 items-center gap-1 rounded-full px-2 text-[12px] font-semibold",
        styles[list],
      )}
    >
      <Icon className="size-3" aria-hidden />
      {t(`spam.rules.effect.${list}`)}
    </span>
  );
}

/**
 * The spam filter's rules as one table: allowed and blocked senders and words, for the whole server,
 * every domain and every person (admins) or one's own. Searched, filtered, sorted and paged by the
 * server, so it stays quick with thousands of entries; many can be changed at once.
 */
export function RulesPanel({ admin }: { admin: boolean }) {
  const { t, i18n } = useT();
  const language = i18n.language;
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const path = usePath();
  const search = useSearch();
  const filters = useMemo(() => readFilters(search), [search]);
  const base = admin ? "/api/admin/spam" : "/api/account/spam";
  const key = [admin ? "admin" : "account", "spam", "rules", apiQuery(filters, admin)];
  const query = useQuery({
    queryKey: key,
    queryFn: () => api<RulesView>(`${base}/rules?${apiQuery(filters, admin)}`),
    placeholderData: keepPreviousData,
  });

  const [searchText, setSearchText] = useState(filters.search);
  const [openedAt] = useState(() => Math.floor(Date.now() / 1000));
  const [moreFilters, setMoreFilters] = useState(filters.kinds.length > 0 || Boolean(filters.state));
  const [selected, setSelected] = useState<Map<string, Rule>>(new Map());
  const [editing, setEditing] = useState<Rule | null>(null);
  const [creating, setCreating] = useState<Partial<RuleDraft> | null>(null);
  const [importing, setImporting] = useState(false);
  const [bulk, setBulk] = useState<"scope" | "expiry" | "delete" | null>(null);
  const [bulkScope, setBulkScope] = useState("server");
  const [bulkExpiry, setBulkExpiry] = useState<number | null>(null);

  const update = (change: Partial<Filters>, keepPage = false) => {
    const next = { ...filters, ...change, ...(keepPage ? {} : { page: 0 }) };
    navigate(`${path}${writeFilters(next)}`, { replace: true, scroll: false });
  };

  // The search box follows the address (the back button), and the address the box, a moment after typing.
  const [seenSearch, setSeenSearch] = useState(filters.search);
  if (seenSearch !== filters.search) {
    setSeenSearch(filters.search);
    setSearchText(filters.search);
  }
  useEffect(() => {
    if (searchText === filters.search) return;
    const timer = window.setTimeout(() => update({ search: searchText }), 300);
    return () => window.clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [searchText]);

  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: [admin ? "admin" : "account", "spam"] });
  };
  const bulkChange = useMutation({
    mutationFn: (body: { action: string; scope?: string; expiresAt?: number | null }) =>
      api<RulesReport>(`${base}/rules/bulk`, {
        method: "POST",
        body: { ...body, items: [...selected.values()].map((rule) => ({ type: rule.type, id: rule.id })) },
      }),
    onSuccess: (report) => {
      toast(
        report.skippedCount > 0
          ? t("spam.rules.bulk.doneSkipped", { changed: report.changed, skipped: report.skippedCount })
          : t("spam.rules.bulk.done", { count: report.changed }),
        report.skippedCount > 0 ? "info" : "success",
      );
      if (report.skipped[0]) toast(report.skipped[0].reason, "info");
      setSelected(new Map());
      setBulk(null);
      refresh();
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const remove = useMutation({
    mutationFn: (rule: Rule) =>
      api<RulesReport>(`${base}/rules/bulk`, {
        method: "POST",
        body: { action: "delete", items: [{ type: rule.type, id: rule.id }] },
      }),
    onSuccess: (_, rule) => {
      toast(t("spam.senders.removed", { value: rule.value }), "success");
      setSelected((current) => {
        const next = new Map(current);
        next.delete(itemKey(rule));
        return next;
      });
      refresh();
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  const view = query.data;
  const rules = view?.rules ?? [];
  const pages = view ? Math.max(1, Math.ceil(view.total / view.perPage)) : 1;
  const allOnPage = rules.length > 0 && rules.every((rule) => selected.has(itemKey(rule)));
  const selectedRules = [...selected.values()];
  const onlySenders = selectedRules.every((rule) => rule.type === "sender");
  const toggle = (rule: Rule) =>
    setSelected((current) => {
      const next = new Map(current);
      if (next.has(itemKey(rule))) next.delete(itemKey(rule));
      else next.set(itemKey(rule), rule);
      return next;
    });
  const togglePage = () =>
    setSelected((current) => {
      const next = new Map(current);
      for (const rule of rules) {
        if (allOnPage) next.delete(itemKey(rule));
        else next.set(itemKey(rule), rule);
      }
      return next;
    });
  const toggleIn = <T extends string>(values: T[], value: T) =>
    values.includes(value) ? values.filter((item) => item !== value) : [...values, value];
  const kindsShown =
    filters.lists.length === 0
      ? [...SENDER_KINDS, ...WORD_KINDS]
      : [
          ...(filters.lists.some((list) => list !== "points") ? SENDER_KINDS : []),
          ...(filters.lists.includes("points") ? WORD_KINDS : []),
        ];
  const filtered = Boolean(
    filters.search || filters.lists.length || filters.kinds.length || filters.state || filters.scope !== "all",
  );
  const exportHref = `${base}/rules/export?${apiQuery({ ...filters, page: 0 }, admin)}`;
  const defaultScope =
    filters.scope.startsWith("domain:") || filters.scope.startsWith("account:") || filters.scope === "server"
      ? filters.scope
      : "server";

  return (
    <Card>
      <div className="flex flex-col gap-4">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div>
            <h2 className="text-[15px] font-bold">{admin ? t("spam.rules.titleAdmin") : t("spam.rules.title")}</h2>
            <p className="max-w-prose text-[13px] text-muted">
              {admin ? t("spam.rules.introAdmin") : t("spam.rules.intro")}
            </p>
          </div>
          <div className="flex flex-wrap gap-2">
            <Button size="sm" icon={Upload} onClick={() => setImporting(true)}>
              {t("spam.rules.import.button")}
            </Button>
            <a
              href={exportHref}
              download
              className="inline-flex h-8 items-center gap-1.5 rounded-full border border-line bg-surface px-3 text-[13px] font-semibold hover:bg-elevated"
            >
              <Download className="size-4" aria-hidden />
              {t("spam.rules.export")}
            </a>
            <Button
              size="sm"
              variant="primary"
              icon={Plus}
              onClick={() =>
                setCreating({
                  scope: defaultScope,
                  type: filters.lists.length === 1 && filters.lists[0] === "points" ? "word" : "sender",
                  list: filters.lists.length === 1 && filters.lists[0] === "allow" ? "allow" : "block",
                })
              }
            >
              {t("spam.rules.add")}
            </Button>
          </div>
        </div>

        {/* Filters */}
        <div className="flex flex-col gap-3 rounded-[14px] bg-canvas p-3">
          <div className="flex flex-wrap items-center gap-2">
            <label className="flex h-10 min-w-0 flex-1 basis-60 items-center gap-2 rounded-control border border-line bg-surface px-3 focus-within:border-pink focus-within:shadow-focus">
              <Search className="size-4 text-muted" aria-hidden />
              <input
                value={searchText}
                onChange={(event) => setSearchText(event.target.value)}
                placeholder={admin ? t("spam.rules.searchAdmin") : t("spam.rules.search")}
                aria-label={t("spam.rules.search")}
                className="h-full min-w-0 flex-1 bg-transparent text-sm outline-none"
              />
              {searchText && (
                <button type="button" aria-label={t("spam.rules.clearSearch")} onClick={() => setSearchText("")}>
                  <X className="size-4 text-muted" aria-hidden />
                </button>
              )}
            </label>
            {admin && (
              <ScopePicker
                filter
                value={filters.scope}
                onChange={(scope) => update({ scope })}
                label={t("spam.rules.scope.label")}
                className="min-w-0 flex-1 basis-52 sm:max-w-72"
              />
            )}
            <Button
              size="sm"
              variant={moreFilters ? "secondary" : "ghost"}
              icon={SlidersHorizontal}
              aria-expanded={moreFilters}
              onClick={() => setMoreFilters((current) => !current)}
            >
              {t("spam.rules.moreFilters")}
            </Button>
          </div>
          <div className="flex flex-wrap items-center gap-1.5" role="group" aria-label={t("spam.rules.listFilter")}>
            <Pill active={filters.lists.length === 0} onClick={() => update({ lists: [], kinds: [] })}>
              {t("spam.rules.lists.all")}
            </Pill>
            {LISTS.map((list) => (
              <Pill
                key={list}
                active={filters.lists.includes(list)}
                onClick={() => update({ lists: toggleIn(filters.lists, list) })}
              >
                {t(`spam.rules.lists.${list}Plural`)}
                <span className="text-[12px] text-muted tabular-nums">
                  {formatNumber(view?.lists[list] ?? 0, language)}
                </span>
              </Pill>
            ))}
          </div>
          {moreFilters && (
            <div className="flex flex-col gap-3 border-t border-hairline pt-3">
              <div className="flex flex-wrap items-center gap-1.5" role="group" aria-label={t("spam.senders.kind")}>
                <span className="mr-1 text-[12px] font-semibold text-muted">{t("spam.senders.kind")}</span>
                {kindsShown.map((kind) => (
                  <Pill
                    key={kind}
                    active={filters.kinds.includes(kind)}
                    onClick={() => update({ kinds: toggleIn(filters.kinds, kind) })}
                  >
                    {t(`spam.rules.kinds.${kind}`)}
                    <span className="text-[12px] text-muted tabular-nums">
                      {formatNumber(view?.kinds[kind] ?? 0, language)}
                    </span>
                  </Pill>
                ))}
              </div>
              <div className="flex flex-wrap gap-3">
                <Field label={t("spam.rules.state.label")} className="min-w-48 flex-1 sm:max-w-64">
                  {(id) => (
                    <Select
                      id={id}
                      value={filters.state}
                      onChange={(event) => update({ state: event.target.value as RuleState | "" })}
                    >
                      <option value="">{t("spam.rules.state.all")}</option>
                      {STATES.map((state) => (
                        <option key={state} value={state}>
                          {t(`spam.rules.state.${state}`)}
                        </option>
                      ))}
                    </Select>
                  )}
                </Field>
                <Field label={t("spam.rules.sort.label")} className="min-w-48 flex-1 sm:max-w-64">
                  {(id) => (
                    <div className="flex gap-2">
                      <Select
                        id={id}
                        value={filters.sort}
                        onChange={(event) => update({ sort: event.target.value as RuleSort }, true)}
                      >
                        {SORTS.map((sort) => (
                          <option key={sort} value={sort}>
                            {t(`spam.rules.sort.${sort}`)}
                          </option>
                        ))}
                      </Select>
                      <IconButton
                        icon={ArrowDownUp}
                        label={filters.desc ? t("spam.rules.sort.descending") : t("spam.rules.sort.ascending")}
                        active={filters.desc}
                        onClick={() => update({ desc: !filters.desc }, true)}
                      />
                    </div>
                  )}
                </Field>
              </div>
            </div>
          )}
          {filtered && (
            <div className="flex flex-wrap items-center justify-between gap-2 text-[13px]">
              <span className="text-muted">
                {t("spam.rules.found", {
                  count: view?.total ?? 0,
                  formatted: formatNumber(view?.total ?? 0, language),
                })}
              </span>
              <button
                type="button"
                className="font-semibold text-pink-ink hover:underline"
                onClick={() => {
                  setSearchText("");
                  navigate(path, { replace: true, scroll: false });
                }}
              >
                {t("spam.rules.resetFilters")}
              </button>
            </div>
          )}
        </div>

        {/* Selection */}
        {selected.size > 0 && (
          <div className="sticky top-2 z-20 flex flex-wrap items-center gap-2 rounded-[14px] border border-pink/40 bg-surface px-3 py-2 shadow-float">
            <span className="mr-auto text-[13px] font-semibold">
              {t("spam.rules.bulk.selected", { count: selected.size })}
            </span>
            {onlySenders && (
              <>
                <Button
                  size="sm"
                  icon={Check}
                  onClick={() => bulkChange.mutate({ action: "allow" })}
                  busy={bulkChange.isPending}
                >
                  {t("spam.rules.bulk.allow")}
                </Button>
                <Button
                  size="sm"
                  icon={Ban}
                  onClick={() => bulkChange.mutate({ action: "block" })}
                  busy={bulkChange.isPending}
                >
                  {t("spam.rules.bulk.block")}
                </Button>
              </>
            )}
            {admin && (
              <Button size="sm" icon={FolderInput} onClick={() => setBulk("scope")}>
                {t("spam.rules.bulk.move")}
              </Button>
            )}
            <Button size="sm" icon={Clock} onClick={() => setBulk("expiry")}>
              {t("spam.rules.bulk.expiry")}
            </Button>
            <Button size="sm" variant="danger" icon={Trash2} onClick={() => setBulk("delete")}>
              {t("spam.rules.bulk.delete")}
            </Button>
            <IconButton icon={X} size="sm" label={t("spam.rules.bulk.clear")} onClick={() => setSelected(new Map())} />
          </div>
        )}

        {/* Table */}
        {query.isError ? (
          <LoadError error={query.error} onRetry={() => void query.refetch()} />
        ) : !view ? (
          <div className="flex flex-col gap-2" aria-busy>
            {Array.from({ length: 6 }, (_, index) => (
              <div key={index} className="h-12 animate-pulse rounded-control bg-canvas" />
            ))}
          </div>
        ) : rules.length === 0 ? (
          <div className="flex flex-col items-center gap-3 rounded-[14px] bg-canvas px-4 py-10 text-center">
            <p className="text-sm font-semibold">{filtered ? t("spam.rules.emptyFiltered") : t("spam.rules.empty")}</p>
            <p className="max-w-md text-[13px] text-muted">
              {filtered ? t("spam.rules.emptyFilteredHint") : t("spam.rules.emptyHint")}
            </p>
            {!filtered && (
              <Button variant="primary" icon={Plus} onClick={() => setCreating({ scope: defaultScope })}>
                {t("spam.rules.add")}
              </Button>
            )}
          </div>
        ) : (
          <>
            <div className={clsx("overflow-x-auto transition-opacity max-sm:hidden", query.isFetching && "opacity-70")}>
              <table className="w-full min-w-[720px] table-fixed border-separate border-spacing-0 text-left text-sm">
                <thead className="max-sm:hidden">
                  <tr className="text-[12px] text-muted">
                    <th className="w-9 border-b border-hairline py-2 pl-1 text-left">
                      <input
                        type="checkbox"
                        className="size-4 accent-pink"
                        checked={allOnPage}
                        onChange={togglePage}
                        aria-label={t("spam.rules.bulk.selectPage")}
                      />
                    </th>
                    <th className="border-b border-hairline py-2 font-semibold">{t("spam.rules.columns.rule")}</th>
                    <th className="w-28 border-b border-hairline py-2 pr-3 font-semibold whitespace-nowrap">
                      {t("spam.rules.columns.list")}
                    </th>
                    {admin && (
                      <th className="w-44 border-b border-hairline py-2 pr-3 font-semibold">
                        {t("spam.rules.columns.scope")}
                      </th>
                    )}
                    <th className="w-40 border-b border-hairline py-2 pr-3 font-semibold whitespace-nowrap">
                      {t("spam.rules.columns.hits")}
                    </th>
                    <th className="w-28 border-b border-hairline py-2 pr-3 font-semibold whitespace-nowrap">
                      {t("spam.rules.columns.expires")}
                    </th>
                    <th className="w-20 border-b border-hairline py-2" />
                  </tr>
                </thead>
                <tbody>
                  {rules.map((rule) => {
                    const checked = selected.has(itemKey(rule));
                    const expiresSoon = rule.expiresAt !== null && rule.expiresAt - openedAt < 3 * 86_400;
                    return (
                      <tr
                        key={itemKey(rule)}
                        className={clsx(
                          "group align-middle max-sm:flex max-sm:flex-wrap max-sm:items-center max-sm:gap-x-2 max-sm:border-b max-sm:border-hairline max-sm:py-2",
                          checked && "bg-pink-tint/40",
                        )}
                      >
                        <td className="border-b border-hairline py-2 pl-1 max-sm:border-0 max-sm:py-0">
                          <input
                            type="checkbox"
                            className="size-4 accent-pink"
                            checked={checked}
                            onChange={() => toggle(rule)}
                            aria-label={t("spam.rules.bulk.select", { value: rule.value })}
                          />
                        </td>
                        <td className="border-b border-hairline py-2 pr-3 max-sm:max-w-none max-sm:min-w-0 max-sm:flex-1 max-sm:border-0 max-sm:py-0">
                          <button
                            type="button"
                            onClick={() => setEditing(rule)}
                            className="block w-full min-w-0 text-left"
                          >
                            <code className="block truncate text-[13px] font-semibold text-ink group-hover:text-pink-ink">
                              {rule.value}
                            </code>
                            <span className="block truncate text-[12px] text-muted">
                              {[
                                t(`spam.rules.kinds.${rule.kind}`),
                                rule.type === "word"
                                  ? t("spam.words.pointsShort", {
                                      points: formatNumber(rule.points ?? view.defaultPoints, language),
                                    })
                                  : null,
                                rule.note || null,
                              ]
                                .filter(Boolean)
                                .join(" · ")}
                            </span>
                          </button>
                        </td>
                        <td className="border-b border-hairline py-2 pr-3 max-sm:border-0 max-sm:py-0">
                          <ListBadge list={rule.list} />
                        </td>
                        {admin && (
                          <td className="border-b border-hairline py-2 pr-3 max-sm:max-w-none max-sm:border-0 max-sm:py-0">
                            <span className="block truncate text-[13px]" title={scopeLabel(rule.scope, t)}>
                              {scopeLabel(rule.scope, t)}
                            </span>
                          </td>
                        )}
                        <td className="truncate border-b border-hairline py-2 pr-3 text-[13px] whitespace-nowrap max-sm:border-0 max-sm:py-0">
                          {rule.hits === 0 ? (
                            <span className="text-faint">{t("spam.rules.neverHit")}</span>
                          ) : (
                            <span title={rule.lastHitAt ? formatDate(rule.lastHitAt, language) : undefined}>
                              <span className="font-semibold tabular-nums">{formatNumber(rule.hits, language)}×</span>
                              {rule.lastHitAt && (
                                <span className="text-muted"> · {formatRelative(rule.lastHitAt, language)}</span>
                              )}
                            </span>
                          )}
                        </td>
                        <td className="border-b border-hairline py-2 pr-3 text-[13px] whitespace-nowrap max-sm:border-0 max-sm:py-0">
                          {rule.expiresAt === null ? (
                            <span className="text-faint">{t("spam.rules.expiry.foreverShort")}</span>
                          ) : (
                            <span
                              className={clsx(
                                "inline-flex items-center gap-1",
                                expiresSoon ? "text-warning" : "text-muted",
                              )}
                              title={formatDate(rule.expiresAt, language)}
                            >
                              <Clock className="size-3" aria-hidden />
                              {formatRelative(rule.expiresAt, language)}
                            </span>
                          )}
                        </td>
                        <td className="border-b border-hairline py-2 max-sm:ml-auto max-sm:border-0 max-sm:py-0">
                          <span className="flex justify-end gap-0.5">
                            <IconButton
                              icon={Pencil}
                              size="sm"
                              label={t("spam.rules.edit", { value: rule.value })}
                              onClick={() => setEditing(rule)}
                            />
                            <IconButton
                              icon={Trash2}
                              size="sm"
                              label={t("spam.rules.remove", { value: rule.value })}
                              onClick={() => remove.mutate(rule)}
                            />
                          </span>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
            {/* On a phone every rule is a small card: the value gets the whole width. */}
            <ul className={clsx("flex flex-col sm:hidden", query.isFetching && "opacity-70")}>
              {rules.map((rule) => (
                <li
                  key={itemKey(rule)}
                  className={clsx(
                    "flex gap-3 border-b border-hairline py-3 last:border-b-0",
                    selected.has(itemKey(rule)) && "bg-pink-tint/40",
                  )}
                >
                  <input
                    type="checkbox"
                    className="mt-1 size-4 shrink-0 accent-pink"
                    checked={selected.has(itemKey(rule))}
                    onChange={() => toggle(rule)}
                    aria-label={t("spam.rules.bulk.select", { value: rule.value })}
                  />
                  <div className="flex min-w-0 flex-1 flex-col gap-1.5">
                    <button type="button" onClick={() => setEditing(rule)} className="min-w-0 text-left">
                      <code className="block text-[13px] font-semibold break-all text-ink">{rule.value}</code>
                      <span className="block text-[12px] text-muted">
                        {[
                          t(`spam.rules.kinds.${rule.kind}`),
                          admin ? scopeLabel(rule.scope, t) : null,
                          rule.note || null,
                        ]
                          .filter(Boolean)
                          .join(" · ")}
                      </span>
                    </button>
                    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[12px] text-muted">
                      <ListBadge list={rule.list} />
                      <span>
                        {rule.hits === 0
                          ? t("spam.rules.neverHit")
                          : `${formatNumber(rule.hits, language)}×${rule.lastHitAt ? ` · ${formatRelative(rule.lastHitAt, language)}` : ""}`}
                      </span>
                      {rule.expiresAt !== null && (
                        <span className="inline-flex items-center gap-1">
                          <Clock className="size-3" aria-hidden />
                          {formatRelative(rule.expiresAt, language)}
                        </span>
                      )}
                    </div>
                  </div>
                  <IconButton
                    icon={Trash2}
                    size="sm"
                    label={t("spam.rules.remove", { value: rule.value })}
                    onClick={() => remove.mutate(rule)}
                  />
                </li>
              ))}
            </ul>
          </>
        )}

        {/* Pages */}
        {view && view.total > 0 && (
          <div className="flex flex-wrap items-center justify-between gap-3 text-[13px]">
            <span className="text-muted">
              {t("spam.rules.range", {
                from: formatNumber(view.page * view.perPage + 1, language),
                to: formatNumber(Math.min(view.total, (view.page + 1) * view.perPage), language),
                total: formatNumber(view.total, language),
              })}
            </span>
            <div className="flex items-center gap-2">
              <label className="flex items-center gap-2 text-muted">
                {t("spam.rules.perPage")}
                <Select
                  className="w-24"
                  value={String(filters.perPage)}
                  onChange={(event) => update({ perPage: Number(event.target.value) })}
                >
                  {(view.pageSizes ?? [25, 50, 100, 250]).map((size) => (
                    <option key={size} value={size}>
                      {size}
                    </option>
                  ))}
                </Select>
              </label>
              <IconButton
                icon={ChevronLeft}
                label={t("spam.rules.previous")}
                disabled={filters.page === 0}
                onClick={() => update({ page: filters.page - 1 }, true)}
              />
              <span className="tabular-nums">{t("spam.rules.pageOf", { page: filters.page + 1, pages })}</span>
              <IconButton
                icon={ChevronRight}
                label={t("spam.rules.next")}
                disabled={filters.page + 1 >= pages}
                onClick={() => update({ page: filters.page + 1 }, true)}
              />
            </div>
          </div>
        )}
      </div>

      {(creating || editing) && view && (
        <RuleDialog
          open
          admin={admin}
          base={base}
          rule={editing}
          initial={creating ?? undefined}
          defaultPoints={view.defaultPoints}
          maxPoints={view.maxPoints}
          onClose={() => {
            setCreating(null);
            setEditing(null);
          }}
          onSaved={(rule) => {
            toast(
              editing ? t("spam.rules.changed", { value: rule.value }) : t("spam.rules.added", { value: rule.value }),
              "success",
            );
            setCreating(null);
            setEditing(null);
            refresh();
          }}
        />
      )}
      {importing && (
        <ImportDialog
          open
          admin={admin}
          base={base}
          initialScope={defaultScope}
          onClose={() => setImporting(false)}
          onImported={refresh}
        />
      )}
      <Dialog
        open={bulk === "scope"}
        onClose={() => setBulk(null)}
        title={t("spam.rules.bulk.moveTitle", { count: selected.size })}
        width="sm"
      >
        <div className="flex flex-col gap-4 px-6 pt-2 pb-6">
          <p className="text-[13px] text-muted">{t("spam.rules.bulk.moveHint")}</p>
          <ScopePicker value={bulkScope} onChange={setBulkScope} label={t("spam.senders.scope")} />
          <div className="flex justify-end gap-2">
            <Button onClick={() => setBulk(null)}>{t("common.cancel")}</Button>
            <Button
              variant="primary"
              busy={bulkChange.isPending}
              onClick={() => bulkChange.mutate({ action: "scope", scope: bulkScope })}
            >
              {t("spam.rules.bulk.move")}
            </Button>
          </div>
        </div>
      </Dialog>
      <Dialog
        open={bulk === "expiry"}
        onClose={() => setBulk(null)}
        title={t("spam.rules.bulk.expiryTitle", { count: selected.size })}
        width="sm"
      >
        <div className="flex flex-col gap-4 px-6 pt-2 pb-6">
          <ExpiryField value={bulkExpiry} onChange={setBulkExpiry} />
          <div className="flex justify-end gap-2">
            <Button onClick={() => setBulk(null)}>{t("common.cancel")}</Button>
            <Button
              variant="primary"
              busy={bulkChange.isPending}
              onClick={() => bulkChange.mutate({ action: "expiry", expiresAt: bulkExpiry })}
            >
              {t("common.save")}
            </Button>
          </div>
        </div>
      </Dialog>
      <Dialog
        open={bulk === "delete"}
        onClose={() => setBulk(null)}
        title={t("spam.rules.bulk.deleteTitle", { count: selected.size })}
        width="sm"
      >
        <div className="flex flex-col gap-4 px-6 pt-2 pb-6">
          <p className="text-[13px] text-muted">{t("spam.rules.bulk.deleteHint")}</p>
          <div className="flex justify-end gap-2">
            <Button onClick={() => setBulk(null)}>{t("common.cancel")}</Button>
            <Button
              variant="danger"
              icon={Trash2}
              busy={bulkChange.isPending}
              onClick={() => bulkChange.mutate({ action: "delete" })}
            >
              {t("spam.rules.bulk.delete")}
            </Button>
          </div>
        </div>
      </Dialog>
    </Card>
  );
}

/** A link to the rules tab with these filters, e.g. the overview's "unused rules". */
export function rulesLink(
  base: string,
  filters: { list?: RuleListName; state?: RuleState; scope?: string; sort?: RuleSort; desc?: boolean },
) {
  const params = new URLSearchParams();
  if (filters.list) params.set("list", filters.list);
  if (filters.state) params.set("state", filters.state);
  if (filters.scope) params.set("scope", filters.scope);
  if (filters.sort) params.set("sort", filters.sort);
  if (filters.desc) params.set("desc", "1");
  const text = params.toString();
  return text ? `${base}?${text}` : base;
}

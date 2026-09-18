import clsx from "clsx";
import { Search, UserPlus } from "lucide-react";
import { useMemo, useState } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { PageHeader } from "@/components/ui/Card";
import { EmptyState } from "@/components/ui/EmptyState";
import { TextInput } from "@/components/ui/Field";
import { Pill } from "@/components/ui/Pill";
import { useT } from "@/i18n";
import type { Person, PersonStatus, Session } from "@/lib/api";
import { formatDate } from "@/lib/format";
import { usePhone } from "@/lib/media";
import { Link } from "@/lib/router";
import { usePrefs } from "@/state/prefs";
import { CreatePersonDialog } from "./CreatePersonDialog";
import { AdminPill, PersonAvatar, StatusPill, StorageLine } from "./PersonBits";
import { usePeople } from "./queries";

type Filter = "all" | PersonStatus;
const FILTERS: Filter[] = ["all", "active", "invited", "disabled", "deleted"];

export const personUrl = (login: string) => `/admin/people/${encodeURIComponent(login)}`;

function matches(person: Person, filter: Filter, search: string) {
  // The trash only shows up when asked for.
  if (filter === "all" ? person.status === "deleted" : person.status !== filter) return false;
  if (!search) return true;
  const needle = search.toLowerCase();
  return (
    person.name.toLowerCase().includes(needle) || person.addresses.some((address) => address.address.includes(needle))
  );
}

export function PeoplePage({ session }: { session: Session }) {
  const { t, i18n } = useT();
  const mode = usePrefs((s) => s.mode);
  const people = usePeople();
  const phone = usePhone();
  const [filter, setFilter] = useState<Filter>("all");
  const [search, setSearch] = useState("");
  const [creating, setCreating] = useState(false);

  const counts = useMemo(() => {
    const result: Record<Filter, number> = { all: 0, active: 0, invited: 0, disabled: 0, deleted: 0 };
    for (const person of people.data ?? []) {
      result[person.status] += 1;
      if (person.status !== "deleted") result.all += 1;
    }
    return result;
  }, [people.data]);

  if (people.isPending) return <Loading />;
  if (people.isError) return <LoadError error={people.error} onRetry={() => void people.refetch()} />;

  const visible = people.data.filter((person) => matches(person, filter, search.trim()));
  const onlyMe = people.data.length === 1 && people.data[0]?.login === session.account.login;
  const pro = mode === "pro";
  // The table is 720 pixels wide and would scroll inside the page on a phone, which feels exactly
  // like the page itself sliding away. Pro mode keeps the cards there; nothing is lost, only laid
  // out differently.
  const table = pro && !phone;

  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <PageHeader title={t("people.title")} intro={!pro && t("people.intro")} />
        <Button variant="primary" icon={UserPlus} onClick={() => setCreating(true)}>
          {t("people.add")}
        </Button>
      </div>

      {onlyMe ? (
        <EmptyState
          scene="noAccount"
          title={t("people.empty.title")}
          body={t("people.empty.body")}
          action={
            <Button variant="primary" icon={UserPlus} onClick={() => setCreating(true)}>
              {t("people.add")}
            </Button>
          }
        />
      ) : (
        <>
          <div className="flex flex-wrap items-center gap-3">
            <div className="flex flex-wrap gap-2">
              {FILTERS.filter((value) => value === "all" || counts[value] > 0).map((value) => (
                <Pill
                  key={value}
                  active={filter === value}
                  count={value === "all" ? undefined : counts[value]}
                  onClick={() => setFilter(value)}
                >
                  {t(`people.filter.${value}`)}
                </Pill>
              ))}
            </div>
            <label className="relative ml-auto w-full sm:w-64">
              <span className="sr-only">{t("people.search")}</span>
              <Search
                className="pointer-events-none absolute top-1/2 left-3.5 size-4 -translate-y-1/2 text-faint"
                aria-hidden
              />
              <TextInput
                type="search"
                className="h-10 pl-10"
                placeholder={t("people.search")}
                value={search}
                onChange={(event) => setSearch(event.target.value)}
              />
            </label>
          </div>

          {visible.length === 0 ? (
            <EmptyState
              compact={pro}
              scene="search"
              title={t("people.noResults.title")}
              body={t("people.noResults.body")}
            />
          ) : table ? (
            <div className="overflow-x-auto rounded-card border border-hairline bg-surface">
              <table className="w-full min-w-[720px] text-left text-sm">
                <thead className="border-b border-hairline text-[12px] font-semibold text-muted">
                  <tr>
                    <th className="px-4 py-2.5">{t("people.table.person")}</th>
                    <th className="px-4 py-2.5">{t("people.table.status")}</th>
                    <th className="w-48 px-4 py-2.5">{t("people.table.storage")}</th>
                    <th className="px-4 py-2.5">{t("people.table.addresses")}</th>
                    <th className="px-4 py-2.5">{t("people.table.created")}</th>
                  </tr>
                </thead>
                <tbody>
                  {visible.map((person) => (
                    <tr key={person.login} className="border-b border-hairline last:border-b-0 hover:bg-elevated">
                      <td className="px-4 py-2">
                        <Link to={personUrl(person.login)} className="flex items-center gap-3 rounded-lg">
                          <PersonAvatar person={person} size="sm" />
                          <span className="min-w-0">
                            <span className="block truncate font-semibold">{person.name || person.login}</span>
                            <span className="block truncate text-[12px] text-muted">{person.login}</span>
                          </span>
                        </Link>
                      </td>
                      <td className="px-4 py-2">
                        <span className="flex flex-wrap gap-1.5">
                          <StatusPill status={person.status} />
                          {person.role === "admin" && <AdminPill />}
                        </span>
                      </td>
                      <td className="px-4 py-2">
                        <StorageLine person={person} />
                      </td>
                      <td className="px-4 py-2 text-muted">{person.addresses.length}</td>
                      <td className="px-4 py-2 text-muted">{formatDate(person.createdAt, i18n.language)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : (
            <ul className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
              {visible.map((person) => (
                <li key={person.login}>
                  <Link
                    to={personUrl(person.login)}
                    className={clsx(
                      "flex h-full flex-col gap-3 rounded-card border border-hairline bg-surface p-4 transition-colors hover:border-pink-tint-strong hover:bg-pink-tint/30",
                    )}
                  >
                    <span className="flex items-center gap-3">
                      <PersonAvatar person={person} />
                      <span className="min-w-0 flex-1">
                        <span className="block truncate font-semibold">
                          {person.name || person.login.split("@")[0]}
                          {person.login === session.account.login && (
                            <span className="ml-1.5 text-[12px] font-medium text-muted">({t("people.you")})</span>
                          )}
                        </span>
                        <span className="block truncate text-[13px] text-muted">{person.login}</span>
                      </span>
                    </span>
                    <span className="flex flex-wrap gap-1.5">
                      <StatusPill status={person.status} />
                      {person.role === "admin" && <AdminPill />}
                      {person.addresses.length > 1 && (
                        <span className="inline-flex h-6 items-center text-[12px] text-muted">
                          {t("people.addresses", { count: person.addresses.length })}
                        </span>
                      )}
                    </span>
                    <StorageLine person={person} />
                  </Link>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
      <CreatePersonDialog open={creating} onClose={() => setCreating(false)} />
    </div>
  );
}

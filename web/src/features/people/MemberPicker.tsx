import { Search } from "lucide-react";
import { useState } from "react";
import { TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import type { Person } from "@/lib/api";

/** One chosen member: their login, and whether they may send with the shared address. */
export interface PickedMember {
  login: string;
  maySend: boolean;
}

/**
 * Picks members from the people of the server, for a group or a shared mailbox. With `sendLabel`,
 * each chosen member also gets a switch for sending with the shared address.
 */
export function MemberPicker({
  people,
  value,
  onChange,
  sendLabel,
}: {
  people: Person[];
  value: PickedMember[];
  onChange: (members: PickedMember[]) => void;
  sendLabel?: string;
}) {
  const { t } = useT();
  const [search, setSearch] = useState("");
  const needle = search.trim().toLowerCase();
  const shown = people.filter(
    (person) => !needle || person.login.includes(needle) || person.name.toLowerCase().includes(needle),
  );
  const picked = (login: string) => value.find((member) => member.login === login);
  const toggle = (login: string) =>
    onChange(picked(login) ? value.filter((member) => member.login !== login) : [...value, { login, maySend: false }]);
  const setSend = (login: string, maySend: boolean) =>
    onChange(value.map((member) => (member.login === login ? { ...member, maySend } : member)));

  return (
    <div className="flex flex-col gap-2">
      <label className="relative">
        <span className="sr-only">{t("groups.members.search")}</span>
        <Search
          className="pointer-events-none absolute top-1/2 left-3.5 size-4 -translate-y-1/2 text-faint"
          aria-hidden
        />
        <TextInput
          type="search"
          className="h-10 pl-10"
          placeholder={t("groups.members.search")}
          value={search}
          onChange={(event) => setSearch(event.target.value)}
        />
      </label>
      <p className="text-[12px] text-muted">{t("groups.members.chosen", { count: value.length })}</p>
      <ul className="flex max-h-60 flex-col overflow-y-auto rounded-control border border-hairline">
        {shown.map((person) => {
          const member = picked(person.login);
          return (
            <li
              key={person.login}
              className="flex min-h-10 items-center gap-3 border-b border-hairline px-3 py-1.5 last:border-b-0"
            >
              <label className="flex min-w-0 flex-1 items-center gap-3">
                <input
                  type="checkbox"
                  className="size-4 accent-pink"
                  checked={Boolean(member)}
                  onChange={() => toggle(person.login)}
                />
                <span className="min-w-0">
                  <span className="block truncate text-sm font-semibold">{person.name || person.login}</span>
                  <span className="block truncate text-[12px] text-muted">{person.login}</span>
                </span>
              </label>
              {sendLabel && member && (
                <label className="flex shrink-0 items-center gap-2 text-[12px] text-muted">
                  <input
                    type="checkbox"
                    className="size-4 accent-pink"
                    checked={member.maySend}
                    onChange={(event) => setSend(person.login, event.target.checked)}
                  />
                  {sendLabel}
                </label>
              )}
            </li>
          );
        })}
        {shown.length === 0 && <li className="px-3 py-2 text-[13px] text-muted">{t("groups.members.none")}</li>}
      </ul>
    </div>
  );
}

/** People who can be members: not in the trash, not a shared mailbox, and (unless allowed) no services. */
export function memberChoices(people: Person[] | undefined, services: boolean): Person[] {
  return (people ?? []).filter(
    (person) => person.status !== "deleted" && !person.sharedMailbox && (services || person.role !== "service"),
  );
}

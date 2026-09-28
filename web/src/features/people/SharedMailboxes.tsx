import { Bot, Inbox, Plus, Save } from "lucide-react";
import { useState, type FormEvent } from "react";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Dialog } from "@/components/ui/Dialog";
import { Field, Select, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import type { Person } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { Link, navigate } from "@/lib/router";
import { toast } from "@/state/toasts";
import { QuotaSelect } from "./CreatePersonDialog";
import { MemberPicker, memberChoices, type PickedMember } from "./MemberPicker";
import { PersonAvatar, StorageLine } from "./PersonBits";
import {
  useCreateSharedMailbox,
  useMailDomains,
  useEndSharedMailbox,
  useMakeSharedMailbox,
  usePeople,
  useSetSharedMembers,
} from "./queries";

const personUrl = (login: string) => `/admin/people/${encodeURIComponent(login)}`;

export function SharedMailboxPill() {
  const { t } = useT();
  return (
    <span className="inline-flex h-6 items-center rounded-full border border-line px-2.5 text-[12px] font-semibold text-muted">
      {t("sharedMailboxes.pill")}
    </span>
  );
}

function CreateSharedMailboxDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { t } = useT();
  const errorText = useErrorText();
  const domains = useMailDomains();
  const people = usePeople();
  const create = useCreateSharedMailbox();
  const [name, setName] = useState("");
  const [local, setLocal] = useState("");
  const [domain, setDomain] = useState("");
  const [quota, setQuota] = useState(0);
  const [members, setMembers] = useState<PickedMember[]>([]);
  const chosenDomain = domain || domains.data?.[0]?.name || "";

  const submit = (event: FormEvent) => {
    event.preventDefault();
    create.mutate(
      { address: `${local.trim()}@${chosenDomain}`, name: name.trim(), quotaBytes: quota, members },
      {
        onSuccess: (result) => {
          toast(t("sharedMailboxes.toasts.created", { address: result.person.login }), "success");
          onClose();
          navigate(personUrl(result.person.login));
        },
      },
    );
  };

  return (
    <Dialog open={open} onClose={onClose} title={t("sharedMailboxes.create")} closeOnOutsideClick={false}>
      <form className="flex flex-col gap-4 px-6 pt-1 pb-6" onSubmit={submit}>
        <p className="text-[13px] text-muted">{t("sharedMailboxes.createIntro")}</p>
        <Field label={t("sharedMailboxes.name")}>
          {(id) => (
            <TextInput
              id={id}
              maxLength={200}
              placeholder={t("sharedMailboxes.namePlaceholder")}
              value={name}
              onChange={(event) => setName(event.target.value)}
            />
          )}
        </Field>
        <Field label={t("sharedMailboxes.address")} error={create.isError ? errorText(create.error) : undefined}>
          {(id) => (
            <div className="flex flex-wrap items-center gap-2">
              <TextInput
                id={id}
                required
                autoComplete="off"
                autoCapitalize="none"
                spellCheck={false}
                className="min-w-0 flex-1 basis-40"
                placeholder={t("sharedMailboxes.addressPlaceholder")}
                value={local}
                onChange={(event) => setLocal(event.target.value)}
              />
              <span className="flex min-w-0 flex-1 basis-48 items-center gap-2">
                <span className="text-muted">@</span>
                <Select
                  aria-label={t("people.domain")}
                  className="min-w-0 flex-1"
                  value={chosenDomain}
                  onChange={(event) => setDomain(event.target.value)}
                >
                  {domains.data?.map((entry) => (
                    <option key={entry.name} value={entry.name}>
                      {entry.name}
                    </option>
                  ))}
                </Select>
              </span>
            </div>
          )}
        </Field>
        <Field label={t("people.detail.quota")}>
          {(id) => <QuotaSelect id={id} value={quota} onChange={setQuota} />}
        </Field>
        <Field label={t("sharedMailboxes.members")} hint={t("sharedMailboxes.membersHint")}>
          {() => (
            <MemberPicker
              people={memberChoices(people.data, false)}
              value={members}
              onChange={setMembers}
              sendLabel={t("sharedMailboxes.maySend")}
            />
          )}
        </Field>
        <div className="flex justify-end gap-2">
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button type="submit" variant="primary" icon={Plus} busy={create.isPending} disabled={!local.trim()}>
            {t("sharedMailboxes.create")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

/** Shared mailboxes, apart from the people: nobody signs in to the portal as them, their members use them. */
export function SharedMailboxesSection({ people }: { people: Person[] }) {
  const { t } = useT();
  const [creating, setCreating] = useState(false);
  const shared = people.filter((person) => person.sharedMailbox && person.status !== "deleted");

  return (
    <Card
      title={t("sharedMailboxes.title")}
      action={
        <Button size="sm" icon={Plus} onClick={() => setCreating(true)}>
          {t("sharedMailboxes.create")}
        </Button>
      }
    >
      <div className="flex flex-col gap-3">
        <p className="-mt-1 text-[13px] text-muted">{t("sharedMailboxes.explain")}</p>
        {shared.length > 0 && (
          <ul className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {shared.map((person) => (
              <li key={person.login}>
                <Link
                  to={personUrl(person.login)}
                  className="flex h-full flex-col gap-3 rounded-card border border-hairline bg-surface p-4 transition-colors hover:border-pink-tint-strong hover:bg-pink-tint/30"
                >
                  <span className="flex items-center gap-3">
                    <PersonAvatar person={person} />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate font-semibold">{person.name || person.login.split("@")[0]}</span>
                      <span className="block truncate text-[13px] text-muted">{person.login}</span>
                    </span>
                  </span>
                  <StorageLine person={person} />
                </Link>
              </li>
            ))}
          </ul>
        )}
      </div>
      {creating && <CreateSharedMailboxDialog open onClose={() => setCreating(false)} />}
    </Card>
  );
}

/** Who uses a shared mailbox, and who may send with its address. */
export function SharedMembersCard({ person }: { person: Person }) {
  const { t } = useT();
  const people = usePeople();
  const save = useSetSharedMembers(person.login, t("sharedMailboxes.toasts.saved"));
  const [members, setMembers] = useState<PickedMember[]>(
    (person.members ?? []).map((member) => ({ login: member.login, maySend: member.maySend })),
  );

  return (
    <Card title={t("sharedMailboxes.members")}>
      <div className="flex flex-col gap-3">
        <p className="-mt-1 flex gap-2 text-[13px] text-muted">
          <Inbox className="mt-0.5 size-4 shrink-0" aria-hidden />
          {t("sharedMailboxes.membersIntro")}
        </p>
        <MemberPicker
          people={memberChoices(people.data, false)}
          value={members}
          onChange={setMembers}
          sendLabel={t("sharedMailboxes.maySend")}
        />
        <div className="flex justify-end">
          <Button variant="primary" icon={Save} busy={save.isPending} onClick={() => save.mutate(members)}>
            {t("sharedMailboxes.save")}
          </Button>
        </div>
      </div>
    </Card>
  );
}

/** Turns a person or a service into a shared mailbox. The mail, folders and addresses stay. */
export function ConvertToShared({ person }: { person: Person }) {
  const { t } = useT();
  const errorText = useErrorText();
  const people = usePeople();
  const [asking, setAsking] = useState(false);
  const [members, setMembers] = useState<PickedMember[]>([]);
  const convert = useMakeSharedMailbox(person.login, t("sharedMailboxes.toasts.converted", { login: person.login }));
  const from = person.role === "service" ? "Service" : "Person";
  const close = () => {
    setAsking(false);
    setMembers([]);
    convert.reset();
  };
  return (
    <div className="flex flex-col items-start gap-2 border-t border-hairline pt-4 sm:flex-row sm:items-center sm:justify-between sm:gap-4">
      <p className="min-w-0 flex-1 text-[13px] text-muted">{t("sharedMailboxes.convertHint")}</p>
      <Button icon={Inbox} onClick={() => setAsking(true)}>
        {t("sharedMailboxes.convert")}
      </Button>
      <Dialog
        open={asking}
        onClose={close}
        title={t("sharedMailboxes.convertTitle", { login: person.login })}
        closeOnOutsideClick={false}
      >
        <div className="flex flex-col gap-4 px-6 pt-1 pb-6">
          <p className="text-sm text-muted">{t(`sharedMailboxes.convertBody${from}`)}</p>
          <Field label={t("sharedMailboxes.members")} hint={t("sharedMailboxes.membersHint")}>
            {() => (
              <MemberPicker
                // Never a member of itself.
                people={memberChoices(people.data, false).filter((choice) => choice.login !== person.login)}
                value={members}
                onChange={setMembers}
                sendLabel={t("sharedMailboxes.maySend")}
              />
            )}
          </Field>
          {convert.isError && (
            <p role="alert" className="text-[13px] text-danger">
              {errorText(convert.error)}
            </p>
          )}
          <div className="flex justify-end gap-2">
            <Button onClick={close}>{t("common.cancel")}</Button>
            <Button
              variant="primary"
              icon={Inbox}
              busy={convert.isPending}
              onClick={() => convert.mutate(members, { onSuccess: close })}
            >
              {t("sharedMailboxes.convert")}
            </Button>
          </div>
        </div>
      </Dialog>
    </div>
  );
}

/** Turns a shared mailbox back into a plain service: the members lose it, its app passwords stay. */
export function EndShared({ person }: { person: Person }) {
  const { t } = useT();
  const errorText = useErrorText();
  const [asking, setAsking] = useState(false);
  const end = useEndSharedMailbox(person.login, () => t("sharedMailboxes.toasts.ended", { login: person.login }));
  return (
    <div className="flex flex-col items-start gap-2 border-t border-hairline pt-4 sm:flex-row sm:items-center sm:justify-between sm:gap-4">
      <p className="min-w-0 flex-1 text-[13px] text-muted">{t("sharedMailboxes.endHint")}</p>
      <Button icon={Bot} onClick={() => setAsking(true)}>
        {t("sharedMailboxes.end")}
      </Button>
      <Dialog
        open={asking}
        onClose={() => setAsking(false)}
        title={t("sharedMailboxes.endTitle", { login: person.login })}
        width="sm"
      >
        <div className="flex flex-col gap-4 px-6 pt-1 pb-6">
          <p className="text-sm text-muted">{t("sharedMailboxes.endBody")}</p>
          {end.isError && (
            <p role="alert" className="text-[13px] text-danger">
              {errorText(end.error)}
            </p>
          )}
          <div className="flex justify-end gap-2">
            <Button onClick={() => setAsking(false)}>{t("common.cancel")}</Button>
            <Button
              variant="primary"
              icon={Bot}
              busy={end.isPending}
              onClick={() => end.mutate(undefined, { onSuccess: () => setAsking(false) })}
            >
              {t("sharedMailboxes.end")}
            </Button>
          </div>
        </div>
      </Dialog>
    </div>
  );
}

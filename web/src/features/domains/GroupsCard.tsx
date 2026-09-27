import { Pencil, Plus, Trash2, UsersRound } from "lucide-react";
import { useState, type FormEvent } from "react";
import { Button, IconButton } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Dialog } from "@/components/ui/Dialog";
import { Field, Select, TextInput, Toggle } from "@/components/ui/Field";
import { useT } from "@/i18n";
import type { DomainDetail, GroupInfo, WhoMaySend } from "@/lib/api";
import { MemberPicker, memberChoices, type PickedMember } from "@/features/people/MemberPicker";
import { usePeople } from "@/features/people/queries";
import { useCreateGroup, useRemoveGroup, useUpdateGroup } from "./queries";

const WHO: WhoMaySend[] = ["anyone", "members", "domain"];
const localOf = (address: string) => address.slice(0, address.lastIndexOf("@"));

/** Creates a group, or changes one when `group` is given. */
function GroupDialog({
  domain,
  group,
  open,
  onClose,
}: {
  domain: DomainDetail;
  group: GroupInfo | null;
  open: boolean;
  onClose: () => void;
}) {
  const { t } = useT();
  const people = usePeople();
  const create = useCreateGroup(domain.name, t("groups.toasts.created"));
  const update = useUpdateGroup(domain.name, t("groups.toasts.saved"));
  const [local, setLocal] = useState(group ? localOf(group.address) : "");
  const [name, setName] = useState(group?.name ?? "");
  const [who, setWho] = useState<WhoMaySend>(group?.whoMaySend ?? "anyone");
  const [sendAs, setSendAs] = useState(group?.membersMaySendAs ?? false);
  const [members, setMembers] = useState<PickedMember[]>(
    group?.members.map((member) => ({ login: member.login, maySend: false })) ?? [],
  );
  const busy = create.isPending || update.isPending;

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const body = {
      local: group ? localOf(group.address) : local.trim(),
      name: name.trim(),
      whoMaySend: who,
      membersMaySendAs: sendAs,
      members: members.map((member) => member.login),
    };
    (group ? update : create).mutate(body, { onSuccess: onClose });
  };

  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={group ? group.address : t("groups.create")}
      closeOnOutsideClick={false}
    >
      <form className="flex flex-col gap-4 px-6 pt-1 pb-6" onSubmit={submit}>
        {!group && (
          <Field label={t("groups.address")}>
            {(id) => (
              <div className="flex items-center gap-2">
                <TextInput
                  id={id}
                  required
                  autoComplete="off"
                  autoCapitalize="none"
                  spellCheck={false}
                  placeholder={t("groups.addressPlaceholder")}
                  value={local}
                  onChange={(event) => setLocal(event.target.value)}
                />
                <span className="shrink-0 text-sm text-muted">@{domain.name}</span>
              </div>
            )}
          </Field>
        )}
        <Field label={t("groups.name")}>
          {(id) => (
            <TextInput
              id={id}
              maxLength={200}
              placeholder={t("groups.namePlaceholder")}
              value={name}
              onChange={(event) => setName(event.target.value)}
            />
          )}
        </Field>
        <Field label={t("groups.whoMaySend")} hint={t(`groups.who.${who}Hint`)}>
          {(id) => (
            <Select id={id} value={who} onChange={(event) => setWho(event.target.value as WhoMaySend)}>
              {WHO.map((value) => (
                <option key={value} value={value}>
                  {t(`groups.who.${value}`, { domain: domain.name })}
                </option>
              ))}
            </Select>
          )}
        </Field>
        <Toggle checked={sendAs} onChange={setSendAs} label={t("groups.sendAs")} description={t("groups.sendAsHint")} />
        <Field label={t("groups.members.title")}>
          {() => <MemberPicker people={memberChoices(people.data, true)} value={members} onChange={setMembers} />}
        </Field>
        <div className="flex justify-end gap-2">
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button type="submit" variant="primary" busy={busy}>
            {group ? t("groups.save") : t("groups.create")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

/** Addresses of the domain that deliver to several people here, such as info@ or vorstand@. */
export function GroupsCard({ domain }: { domain: DomainDetail }) {
  const { t } = useT();
  const remove = useRemoveGroup(domain.name, t("groups.toasts.removed"));
  const [editing, setEditing] = useState<GroupInfo | "new" | null>(null);
  const groups = domain.groups ?? [];

  return (
    <Card
      title={t("groups.title")}
      action={
        <Button size="sm" icon={Plus} onClick={() => setEditing("new")}>
          {t("groups.create")}
        </Button>
      }
    >
      <div className="flex flex-col gap-3">
        <p className="-mt-1 text-[13px] text-muted">{t("groups.explain")}</p>
        {groups.length > 0 && (
          <ul className="flex flex-col divide-y divide-hairline">
            {groups.map((group) => (
              <li key={group.address} className="flex items-start gap-3 py-2.5">
                <UsersRound className="mt-0.5 size-4 shrink-0 text-muted" aria-hidden />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm font-semibold">
                    {group.name ? `${group.name} · ${group.address}` : group.address}
                  </span>
                  <span className="block text-[13px] break-words text-muted">
                    {group.members.length === 0
                      ? t("groups.noMembers")
                      : group.members.map((member) => member.name || member.login).join(", ")}
                  </span>
                  <span className="block text-[12px] text-faint">
                    {t(`groups.who.${group.whoMaySend}`, { domain: domain.name })}
                    {group.membersMaySendAs && ` · ${t("groups.sendAsShort")}`}
                  </span>
                </span>
                <IconButton
                  icon={Pencil}
                  label={t("groups.edit", { address: group.address })}
                  onClick={() => setEditing(group)}
                />
                <IconButton
                  icon={Trash2}
                  label={t("groups.remove", { address: group.address })}
                  disabled={remove.isPending}
                  onClick={() => {
                    if (window.confirm(t("groups.removeConfirm", { address: group.address }))) {
                      remove.mutate(localOf(group.address));
                    }
                  }}
                />
              </li>
            ))}
          </ul>
        )}
      </div>
      {editing && (
        <GroupDialog
          key={editing === "new" ? "new" : editing.address}
          domain={domain}
          group={editing === "new" ? null : editing}
          open
          onClose={() => setEditing(null)}
        />
      )}
    </Card>
  );
}

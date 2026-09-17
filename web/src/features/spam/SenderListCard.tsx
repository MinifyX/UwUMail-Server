import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Plus, Trash2 } from "lucide-react";
import { useState, type FormEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Field, Segmented, Select, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import {
  api,
  type NewSender,
  type SenderKind,
  type SenderListEntry,
  type SenderListName,
  type SendersView,
} from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { usePrefs } from "@/state/prefs";
import { toast } from "@/state/toasts";
import { guessSenderKind } from "./senders";

const KINDS: SenderKind[] = ["address", "domain", "pattern", "ip", "host"];

function EntryRow({
  entry,
  admin,
  busy,
  onRemove,
}: {
  entry: SenderListEntry;
  admin: boolean;
  busy: boolean;
  onRemove: () => void;
}) {
  const { t } = useT();
  const details = [t(`spam.senders.kinds.${entry.kind}`)];
  if (admin) details.push(entry.domain ?? t("spam.senders.wholeServer"));
  if (entry.note) details.push(entry.note);
  return (
    <li className="flex min-h-11 items-center gap-2 border-b border-hairline py-1.5 last:border-b-0">
      <span className="min-w-0 flex-1">
        <span className="block truncate text-sm font-semibold">{entry.value}</span>
        <span className="block truncate text-[12px] text-muted">{details.join(" · ")}</span>
      </span>
      <Button size="sm" variant="danger" icon={Trash2} busy={busy} onClick={onRemove}>
        {t("spam.senders.remove")}
      </Button>
    </li>
  );
}

/** Allowed and blocked senders: one's own in My account, the server's and the domains' for admins. */
export function SenderListCard({ admin }: { admin: boolean }) {
  const { t } = useT();
  const errorText = useErrorText();
  const pro = usePrefs((s) => s.mode) === "pro";
  const queryClient = useQueryClient();
  const key = admin ? ["admin", "spam", "senders"] : ["account", "spam", "senders"];
  const path = admin ? "/api/admin/spam/senders" : "/api/account/spam/senders";
  const query = useQuery({ queryKey: key, queryFn: () => api<SendersView>(path) });
  const [list, setList] = useState<SenderListName>("block");
  const [kind, setKind] = useState<SenderKind | "auto">("auto");
  const [domain, setDomain] = useState("");
  const [value, setValue] = useState("");
  const [note, setNote] = useState("");

  const add = useMutation({
    mutationFn: (body: NewSender) => api<SendersView>(path, { method: "POST", body }),
    onSuccess: (next, body) => {
      queryClient.setQueryData(key, next);
      setValue("");
      setNote("");
      const text = body.list === "allow" ? "spam.senders.addedAllow" : "spam.senders.addedBlock";
      toast(t(text, { value: body.value.trim() }), "success");
    },
  });
  const remove = useMutation({
    mutationFn: (entry: SenderListEntry) => api<SendersView>(`${path}/${entry.id}`, { method: "DELETE" }),
    onSuccess: (next, entry) => {
      queryClient.setQueryData(key, next);
      toast(t("spam.senders.removed", { value: entry.value }), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    add.mutate({
      list,
      value: value.trim(),
      ...(kind === "auto" ? {} : { kind }),
      ...(note.trim() ? { note: note.trim() } : {}),
      ...(admin && domain ? { domain } : {}),
    });
  };

  const title = admin ? t("spam.senders.titleAdmin") : t("spam.senders.title");
  if (query.isPending) {
    return (
      <Card title={title}>
        <Loading />
      </Card>
    );
  }
  if (query.isError) {
    return (
      <Card title={title}>
        <LoadError error={query.error} onRetry={() => void query.refetch()} />
      </Card>
    );
  }
  const { entries, domains = [], limit } = query.data;
  const full = entries.length >= limit;
  const guessed = value.trim() ? guessSenderKind(value) : null;
  const groups: { list: SenderListName; entries: SenderListEntry[] }[] = [
    { list: "allow", entries: entries.filter((entry) => entry.list === "allow") },
    { list: "block", entries: entries.filter((entry) => entry.list === "block") },
  ];

  return (
    <Card title={title}>
      <div className="flex flex-col gap-4">
        <p className="-mt-1 text-[13px] text-muted">
          {admin ? t("spam.senders.explainAdmin") : t("spam.senders.explain")}
        </p>

        <form className="flex flex-col gap-3" onSubmit={submit}>
          <Segmented
            label={t("spam.senders.list")}
            value={list}
            onChange={setList}
            options={[
              { value: "block", label: t("spam.senders.block") },
              { value: "allow", label: t("spam.senders.allow") },
            ]}
          />
          <div className="grid gap-3 sm:grid-cols-2">
            <Field
              label={t("spam.senders.value")}
              className="sm:col-span-2"
              error={add.isError ? errorText(add.error) : undefined}
              hint={
                kind === "auto" && guessed
                  ? t("spam.senders.guessed", { kind: t(`spam.senders.kinds.${guessed}`) })
                  : t("spam.senders.valueHint")
              }
            >
              {(id) => (
                <TextInput
                  id={id}
                  required
                  autoComplete="off"
                  autoCapitalize="none"
                  spellCheck={false}
                  disabled={full}
                  placeholder={t("spam.senders.valuePlaceholder")}
                  value={value}
                  onChange={(event) => setValue(event.target.value)}
                />
              )}
            </Field>
            {admin && (
              <Field label={t("spam.senders.scope")}>
                {(id) => (
                  <Select id={id} value={domain} onChange={(event) => setDomain(event.target.value)}>
                    <option value="">{t("spam.senders.wholeServer")}</option>
                    {domains.map((name) => (
                      <option key={name} value={name}>
                        {name}
                      </option>
                    ))}
                  </Select>
                )}
              </Field>
            )}
            {pro && (
              <Field label={t("spam.senders.kind")}>
                {(id) => (
                  <Select id={id} value={kind} onChange={(event) => setKind(event.target.value as SenderKind | "auto")}>
                    <option value="auto">{t("spam.senders.kindAuto")}</option>
                    {KINDS.map((option) => (
                      <option key={option} value={option}>
                        {t(`spam.senders.kinds.${option}`)}
                      </option>
                    ))}
                  </Select>
                )}
              </Field>
            )}
            {pro && (
              <Field label={t("spam.senders.note")} className={admin ? "sm:col-span-2" : undefined}>
                {(id) => (
                  <TextInput
                    id={id}
                    maxLength={200}
                    placeholder={t("spam.senders.notePlaceholder")}
                    value={note}
                    onChange={(event) => setNote(event.target.value)}
                  />
                )}
              </Field>
            )}
          </div>
          <div className="flex flex-wrap items-center gap-3">
            <Button type="submit" icon={Plus} busy={add.isPending} disabled={full || !value.trim()}>
              {list === "allow" ? t("spam.senders.addAllow") : t("spam.senders.addBlock")}
            </Button>
            {full && <p className="text-[13px] text-muted">{t("spam.senders.full", { limit })}</p>}
          </div>
        </form>

        {entries.length === 0 ? (
          <p className="rounded-control bg-canvas px-3 py-2.5 text-[13px] text-muted">{t("spam.senders.empty")}</p>
        ) : (
          groups
            .filter((group) => group.entries.length > 0)
            .map((group) => (
              <section key={group.list} className="flex flex-col gap-1 border-t border-hairline pt-3">
                <h3 className="text-[13px] font-bold">
                  {t(group.list === "allow" ? "spam.senders.allowedTitle" : "spam.senders.blockedTitle", {
                    count: group.entries.length,
                  })}
                </h3>
                <ul className="flex flex-col">
                  {group.entries.map((entry) => (
                    <EntryRow
                      key={entry.id}
                      entry={entry}
                      admin={admin}
                      busy={remove.isPending && remove.variables?.id === entry.id}
                      onRemove={() => remove.mutate(entry)}
                    />
                  ))}
                </ul>
              </section>
            ))
        )}
      </div>
    </Card>
  );
}

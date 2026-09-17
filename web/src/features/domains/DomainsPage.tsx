import { Globe, Plus, X } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button, IconButton } from "@/components/ui/Button";
import { PageHeader } from "@/components/ui/Card";
import { ConfirmDiscardDialog } from "@/components/ui/ConfirmDiscardDialog";
import { Dialog } from "@/components/ui/Dialog";
import { EmptyState } from "@/components/ui/EmptyState";
import { Field, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { useErrorText } from "@/lib/errors";
import { Link, navigate } from "@/lib/router";
import { usePrefs } from "@/state/prefs";
import { toast } from "@/state/toasts";
import { useDomains } from "@/features/people/queries";
import { DnsStatusPill } from "./DnsBits";
import { useCreateDomain } from "./queries";

export const domainUrl = (name: string) => `/admin/domains/${encodeURIComponent(name)}`;

function CreateDomain({
  onClose,
  onCancel,
  onDirtyChange,
}: {
  onClose: () => void;
  onCancel: () => void;
  onDirtyChange: (dirty: boolean) => void;
}) {
  const { t } = useT();
  const errorText = useErrorText();
  const create = useCreateDomain();
  const [name, setName] = useState("");

  const dirty = name.trim() !== "";
  useEffect(() => {
    onDirtyChange(dirty);
    return () => onDirtyChange(false);
  }, [dirty, onDirtyChange]);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    create.mutate(name.trim(), {
      onSuccess: (detail) => {
        toast(t("domains.toasts.created", { domain: detail.name }), "success");
        onClose();
        navigate(domainUrl(detail.name));
      },
    });
  };

  return (
    <form className="flex flex-col gap-4 px-6 pt-5 pb-6" onSubmit={submit}>
      <header className="flex items-center justify-between gap-4">
        <h2 className="text-lg font-bold">{t("domains.create.title")}</h2>
        <IconButton icon={X} label={t("common.close")} onClick={onCancel} />
      </header>
      <Field
        label={t("domains.create.name")}
        hint={t("domains.create.hint")}
        error={create.isError && errorText(create.error)}
      >
        {(id) => (
          <TextInput
            id={id}
            autoFocus
            required
            autoCapitalize="none"
            spellCheck={false}
            placeholder={t("domains.create.placeholder")}
            value={name}
            onChange={(event) => setName(event.target.value.replace(/\s/g, ""))}
          />
        )}
      </Field>
      <div className="flex justify-end gap-2">
        <Button onClick={onCancel}>{t("common.cancel")}</Button>
        <Button type="submit" variant="primary" busy={create.isPending}>
          {t("domains.create.submit")}
        </Button>
      </div>
    </form>
  );
}

export function DomainsPage() {
  const { t } = useT();
  const pro = usePrefs((s) => s.mode) === "pro";
  const domains = useDomains();
  const [creating, setCreating] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [confirmingDiscard, setConfirmingDiscard] = useState(false);

  const requestClose = () => {
    if (dirty) setConfirmingDiscard(true);
    else setCreating(false);
  };

  if (domains.isPending) return <Loading />;
  if (domains.isError) return <LoadError error={domains.error} onRetry={() => void domains.refetch()} />;

  const addButton = (
    <Button variant="primary" icon={Plus} onClick={() => setCreating(true)}>
      {t("domains.add")}
    </Button>
  );

  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <PageHeader title={t("domains.title")} intro={!pro && t("domains.intro")} />
        {domains.data.length > 0 && addButton}
      </div>

      {domains.data.length === 0 ? (
        <EmptyState
          scene="noAccount"
          title={t("domains.empty.title")}
          body={t("domains.empty.body")}
          action={addButton}
        />
      ) : (
        <ul className={pro ? "rounded-card border border-hairline bg-surface" : "grid gap-3 sm:grid-cols-2"}>
          {domains.data.map((domain) => (
            <li key={domain.name} className={pro ? "border-b border-hairline last:border-b-0" : undefined}>
              <Link
                to={domainUrl(domain.name)}
                className={
                  pro
                    ? "flex flex-wrap items-center gap-x-4 gap-y-1 px-4 py-3 hover:bg-elevated"
                    : "flex h-full flex-col gap-3 rounded-card border border-hairline bg-surface p-4 transition-colors hover:border-pink-tint-strong hover:bg-pink-tint/30"
                }
              >
                <span className="flex min-w-0 items-center gap-3">
                  <span className="flex size-9 shrink-0 items-center justify-center rounded-full bg-pink-tint text-pink-ink">
                    <Globe className="size-[18px]" aria-hidden />
                  </span>
                  <span className="truncate font-semibold">{domain.name}</span>
                </span>
                <span className="flex flex-wrap items-center gap-2 text-[13px] text-muted sm:ml-auto">
                  <DnsStatusPill status={domain.dns?.status ?? null} />
                  <span>{t("domains.people", { count: domain.people })}</span>
                  {domain.aliases > 0 && <span>· {t("domains.aliases", { count: domain.aliases })}</span>}
                </span>
              </Link>
            </li>
          ))}
        </ul>
      )}
      <Dialog open={creating} onClose={requestClose} dismissable={!dirty} width="sm">
        <CreateDomain onClose={() => setCreating(false)} onCancel={requestClose} onDirtyChange={setDirty} />
      </Dialog>
      <ConfirmDiscardDialog
        open={confirmingDiscard}
        onKeepEditing={() => setConfirmingDiscard(false)}
        onDiscard={() => {
          setConfirmingDiscard(false);
          setCreating(false);
        }}
      />
    </div>
  );
}

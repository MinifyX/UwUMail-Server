import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AtSign, Folder, Plus, RotateCcw, Trash2 } from "lucide-react";
import { useState, type FormEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card, CopyButton, PageHeader } from "@/components/ui/Card";
import { Dialog } from "@/components/ui/Dialog";
import { Field, Select, TextInput } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type OwnAddressesView, type StorageView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatBytes, formatDate } from "@/lib/format";
import { toast } from "@/state/toasts";

const addressesKey = ["account", "addresses"] as const;
const storageKey = ["account", "storage"] as const;

function AddressesCard({ data }: { data: OwnAddressesView }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [local, setLocal] = useState("");
  const [domain, setDomain] = useState(data.domains[0] ?? "");
  const saved = (next: OwnAddressesView) => {
    queryClient.setQueryData(addressesKey, next);
    void queryClient.invalidateQueries({ queryKey: ["account"], exact: true });
  };
  const create = useMutation({
    mutationFn: (address: string) =>
      api<OwnAddressesView>("/api/account/aliases", { method: "POST", body: { address } }),
    onSuccess: (next, address) => {
      saved(next);
      setLocal("");
      toast(t("addresses.created", { address }), "success");
    },
  });
  const remove = useMutation({
    mutationFn: (address: string) =>
      api<OwnAddressesView>(`/api/account/aliases/${encodeURIComponent(address)}`, { method: "DELETE" }),
    onSuccess: (next, address) => {
      saved(next);
      toast(t("addresses.deleted", { address }), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    create.mutate(`${local.trim()}@${domain}`);
  };
  const canCreate = data.domains.length > 0 && data.used < data.limit;

  return (
    <Card title={t("addresses.title")}>
      <div className="flex flex-col gap-4">
        <ul className="flex flex-col">
          {data.addresses.map((address) => (
            <li
              key={address.address}
              className="flex min-h-11 items-center gap-2 border-b border-hairline py-1.5 last:border-b-0"
            >
              <span className="min-w-0 flex-1">
                <span className="block truncate text-sm font-semibold">{address.address}</span>
                <span className="block text-[12px] text-muted">
                  {address.kind === "primary"
                    ? t("account.addresses.primary")
                    : address.own
                      ? t("addresses.own")
                      : t("addresses.fromAdmin")}
                </span>
              </span>
              <CopyButton value={address.address} />
              {address.own && (
                <Button
                  size="sm"
                  variant="danger"
                  icon={Trash2}
                  busy={remove.isPending && remove.variables === address.address}
                  onClick={() => remove.mutate(address.address)}
                >
                  {t("addresses.delete")}
                </Button>
              )}
            </li>
          ))}
        </ul>

        {data.domains.length === 0 ? (
          <p className="rounded-control bg-canvas px-3 py-2.5 text-[13px] text-muted">{t("addresses.closed")}</p>
        ) : (
          <>
            <form className="flex flex-col gap-2" onSubmit={submit}>
              <Field
                label={t("addresses.new")}
                hint={t("addresses.used", { used: data.used, limit: data.limit })}
                error={create.isError ? errorText(create.error) : undefined}
              >
                {(id) => (
                  <div className="flex flex-wrap items-center gap-2">
                    <TextInput
                      id={id}
                      required
                      autoComplete="off"
                      autoCapitalize="none"
                      spellCheck={false}
                      disabled={!canCreate}
                      placeholder={t("addresses.localPlaceholder")}
                      className="min-w-0 flex-1 basis-40"
                      value={local}
                      onChange={(event) => setLocal(event.target.value)}
                    />
                    <span className="flex min-w-0 flex-1 basis-48 items-center gap-2">
                      <span className="text-muted">@</span>
                      <Select
                        aria-label={t("addresses.domain")}
                        className="min-w-0 flex-1"
                        value={domain}
                        disabled={!canCreate}
                        onChange={(event) => setDomain(event.target.value)}
                      >
                        {data.domains.map((name) => (
                          <option key={name} value={name}>
                            {name}
                          </option>
                        ))}
                      </Select>
                    </span>
                  </div>
                )}
              </Field>
              <div>
                <Button type="submit" icon={Plus} busy={create.isPending} disabled={!canCreate || !local.trim()}>
                  {t("addresses.create")}
                </Button>
              </div>
            </form>
          </>
        )}

        {data.released.length > 0 && (
          <section className="flex flex-col gap-1 border-t border-hairline pt-3">
            <h3 className="text-[13px] font-bold">{t("addresses.releasedTitle")}</h3>
            <p className="text-[12px] text-muted">{t("addresses.releasedHint")}</p>
            <ul className="flex flex-col">
              {data.released.map((released) => (
                <li key={released.address} className="flex min-h-10 items-center gap-2">
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-sm">{released.address}</span>
                    <span className="block text-[12px] text-faint">
                      {t("addresses.reservedUntil", { date: formatDate(released.reservedUntil, i18n.language) })}
                    </span>
                  </span>
                  <Button
                    size="sm"
                    icon={RotateCcw}
                    disabled={data.used >= data.limit}
                    busy={create.isPending && create.variables === released.address}
                    onClick={() => create.mutate(released.address)}
                  >
                    {t("addresses.restore")}
                  </Button>
                </li>
              ))}
            </ul>
          </section>
        )}
      </div>
    </Card>
  );
}

function StorageCard({ storage }: { storage: StorageView }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const language = i18n.language;
  const [emptying, setEmptying] = useState<"trash" | "junk" | null>(null);
  const empty = useMutation({
    mutationFn: (role: "trash" | "junk") =>
      api<{ removed: number }>(`/api/account/mailboxes/${role}/empty`, { method: "POST", body: {} }),
    onSuccess: (result) => {
      toast(t("storage.emptied", { count: result.removed }), "success");
      setEmptying(null);
      void queryClient.invalidateQueries({ queryKey: storageKey });
      void queryClient.invalidateQueries({ queryKey: ["account"], exact: true });
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const share = storage.quotaBytes > 0 ? Math.min(1, storage.usedBytes / storage.quotaBytes) : 0;
  const largest = Math.max(1, ...storage.mailboxes.map((mailbox) => mailbox.sizeBytes));
  const folderName = (mailbox: StorageView["mailboxes"][number]) =>
    mailbox.role ? t(`storage.roles.${mailbox.role}`) : mailbox.name;

  return (
    <Card title={t("storage.title")}>
      <div className="flex flex-col gap-4">
        <div className="flex flex-col gap-2">
          {storage.quotaBytes > 0 && (
            <div className="h-2.5 overflow-hidden rounded-full bg-canvas" aria-hidden>
              <div
                className={
                  share > 0.9
                    ? "h-full rounded-full bg-danger"
                    : share > 0.75
                      ? "h-full rounded-full bg-warning"
                      : "h-full rounded-full bg-pink"
                }
                style={{ width: `${Math.max(share * 100, 2)}%` }}
              />
            </div>
          )}
          <p className="text-sm text-muted">
            {storage.quotaBytes > 0
              ? t("account.storage.usedOf", {
                  used: formatBytes(storage.usedBytes, language),
                  quota: formatBytes(storage.quotaBytes, language),
                })
              : t("account.storage.used", { used: formatBytes(storage.usedBytes, language) })}
          </p>
        </div>
        <ul className="flex flex-col gap-2.5">
          {storage.mailboxes.map((mailbox) => (
            <li key={mailbox.id} className="flex flex-col gap-1">
              <div className="flex items-center gap-2 text-sm">
                <Folder className="size-4 shrink-0 text-muted" aria-hidden />
                <span className="min-w-0 flex-1 truncate font-semibold">{folderName(mailbox)}</span>
                <span className="text-[12px] text-muted">
                  {t("storage.folderSize", { count: mailbox.emails, size: formatBytes(mailbox.sizeBytes, language) })}
                </span>
                {(mailbox.role === "trash" || mailbox.role === "junk") && mailbox.emails > 0 && (
                  <Button
                    size="sm"
                    variant="danger"
                    icon={Trash2}
                    onClick={() => setEmptying(mailbox.role as "trash" | "junk")}
                  >
                    {t("storage.empty")}
                  </Button>
                )}
              </div>
              <div className="ml-6 h-1.5 overflow-hidden rounded-full bg-canvas" aria-hidden>
                <div
                  className="h-full rounded-full bg-pink/70"
                  style={{ width: `${(mailbox.sizeBytes / largest) * 100}%` }}
                />
              </div>
            </li>
          ))}
        </ul>
      </div>
      <Dialog
        open={emptying !== null}
        onClose={() => setEmptying(null)}
        title={emptying ? t("storage.emptyTitle", { folder: t(`storage.roles.${emptying}`) }) : ""}
        width="sm"
      >
        <div className="flex flex-col gap-4 px-6 pt-1 pb-6">
          <p className="text-sm text-muted">{t("storage.emptyBody")}</p>
          <div className="flex justify-end gap-2">
            <Button onClick={() => setEmptying(null)}>{t("common.cancel")}</Button>
            <Button
              variant="danger"
              icon={Trash2}
              busy={empty.isPending}
              onClick={() => emptying && empty.mutate(emptying)}
            >
              {t("storage.empty")}
            </Button>
          </div>
        </div>
      </Dialog>
    </Card>
  );
}

export function AddressesPage() {
  const { t } = useT();
  const addresses = useQuery({
    queryKey: addressesKey,
    queryFn: () => api<OwnAddressesView>("/api/account/addresses"),
  });
  const storage = useQuery({ queryKey: storageKey, queryFn: () => api<StorageView>("/api/account/storage") });

  if (addresses.isPending || storage.isPending) return <Loading />;
  if (addresses.isError) return <LoadError error={addresses.error} onRetry={() => void addresses.refetch()} />;
  if (storage.isError) return <LoadError error={storage.error} onRetry={() => void storage.refetch()} />;

  return (
    <div className="flex flex-col gap-5">
      <PageHeader
        title={
          <span className="flex items-center gap-2">
            <AtSign className="size-5 text-muted" aria-hidden />
            {t("addresses.pageTitle")}
          </span>
        }
        intro={t("addresses.intro")}
      />
      <div className="grid gap-5 lg:grid-cols-2">
        <AddressesCard key={addresses.data.domains.join()} data={addresses.data} />
        <StorageCard storage={storage.data} />
      </div>
    </div>
  );
}

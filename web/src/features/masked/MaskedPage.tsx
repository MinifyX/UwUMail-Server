import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { EyeOff, Plus, Power, PowerOff, Trash2 } from "lucide-react";
import { useState, type FormEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card, CopyButton, PageHeader } from "@/components/ui/Card";
import { Field, Select, TextInput } from "@/components/ui/Field";
import { Pill } from "@/components/ui/Pill";
import { useT } from "@/i18n";
import { api, type MaskedAddress, type MaskedAddressesView, type MaskedState } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDate } from "@/lib/format";
import { toast } from "@/state/toasts";

const maskedKey = ["account", "masked"] as const;

function CreateCard({ domains, defaultDomain }: { domains: string[]; defaultDomain: string | null }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [description, setDescription] = useState("");
  const [site, setSite] = useState("");
  const [prefix, setPrefix] = useState("");
  const [domain, setDomain] = useState(defaultDomain ?? domains[0] ?? "");
  const create = useMutation({
    mutationFn: () =>
      api<MaskedAddress>("/api/account/masked", {
        method: "POST",
        body: {
          domain,
          description: description.trim(),
          forDomain: site.trim(),
          emailPrefix: prefix.trim() || null,
        },
      }),
    onSuccess: (created) => {
      void queryClient.invalidateQueries({ queryKey: maskedKey });
      setDescription("");
      setSite("");
      setPrefix("");
      void navigator.clipboard?.writeText(created.email).catch(() => undefined);
      toast(t("masked.created", { address: created.email }), "success");
    },
  });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    create.mutate();
  };

  if (domains.length === 0) {
    return (
      <Card title={t("masked.new")}>
        <p className="rounded-control bg-canvas px-3 py-2.5 text-[13px] text-muted">{t("masked.closed")}</p>
      </Card>
    );
  }
  return (
    <Card title={t("masked.new")}>
      <form className="flex flex-col gap-3" onSubmit={submit}>
        <Field label={t("masked.description")}>
          {(id) => (
            <TextInput
              id={id}
              maxLength={200}
              placeholder={t("masked.descriptionPlaceholder")}
              value={description}
              onChange={(event) => setDescription(event.target.value)}
            />
          )}
        </Field>
        <Field label={t("masked.site")} hint={t("masked.siteHint")}>
          {(id) => (
            <TextInput
              id={id}
              maxLength={200}
              autoCapitalize="none"
              spellCheck={false}
              placeholder="https://shop.example.com"
              value={site}
              onChange={(event) => setSite(event.target.value)}
            />
          )}
        </Field>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label={t("masked.prefix")} hint={t("masked.prefixHint")}>
            {(id) => (
              <TextInput
                id={id}
                maxLength={64}
                autoCapitalize="none"
                spellCheck={false}
                pattern="[A-Za-z0-9_]*"
                placeholder={t("masked.prefixPlaceholder")}
                value={prefix}
                onChange={(event) => setPrefix(event.target.value)}
              />
            )}
          </Field>
          {domains.length > 1 && (
            <Field label={t("masked.domain")}>
              {(id) => (
                <Select id={id} value={domain} onChange={(event) => setDomain(event.target.value)}>
                  {domains.map((name) => (
                    <option key={name} value={name}>
                      {name}
                    </option>
                  ))}
                </Select>
              )}
            </Field>
          )}
        </div>
        {create.isError && (
          <p role="alert" className="text-[13px] text-danger">
            {errorText(create.error)}
          </p>
        )}
        <div>
          <Button type="submit" variant="primary" icon={Plus} busy={create.isPending}>
            {t("masked.create")}
          </Button>
        </div>
      </form>
    </Card>
  );
}

type Filter = "active" | "deleted";

function AddressList({ addresses }: { addresses: MaskedAddress[] }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [filter, setFilter] = useState<Filter>("active");
  const change = useMutation({
    mutationFn: ({ id, state }: { id: number; state: MaskedState }): Promise<unknown> =>
      state === "deleted"
        ? api<MaskedAddressesView>(`/api/account/masked/${id}`, { method: "DELETE" })
        : api<MaskedAddress>(`/api/account/masked/${id}`, { method: "PATCH", body: { state } }),
    onSuccess: (_, { state }) => {
      void queryClient.invalidateQueries({ queryKey: maskedKey });
      toast(t(`masked.toasts.${state}`), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const shown = addresses.filter((address) => (filter === "deleted") === (address.state === "deleted"));
  const deleted = addresses.length - addresses.filter((address) => address.state !== "deleted").length;

  return (
    <Card title={t("masked.listTitle")}>
      <div className="flex flex-col gap-3">
        {deleted > 0 && (
          <div className="flex flex-wrap gap-2">
            {(["active", "deleted"] as Filter[]).map((value) => (
              <Pill key={value} active={filter === value} onClick={() => setFilter(value)}>
                {t(`masked.filter.${value}`)}
              </Pill>
            ))}
          </div>
        )}
        {shown.length === 0 ? (
          <p className="text-[13px] text-muted">{t("masked.empty")}</p>
        ) : (
          <ul className="flex flex-col">
            {shown.map((address) => {
              const busy = change.isPending && change.variables?.id === address.id;
              return (
                <li
                  key={address.id}
                  className="flex flex-wrap items-center gap-2 border-b border-hairline py-2 last:border-b-0"
                >
                  <span className="min-w-0 flex-1 basis-60">
                    <span className="block truncate text-sm font-semibold">{address.email}</span>
                    <span className="block truncate text-[12px] text-muted">
                      {[address.description, address.forDomain].filter(Boolean).join(" · ") || t("masked.noNote")}
                    </span>
                    <span className="block text-[12px] text-faint">
                      {t(`masked.state.${address.state}`)}
                      {" · "}
                      {address.lastMessageAt
                        ? t("masked.lastMessage", { date: formatDate(address.lastMessageAt, i18n.language) })
                        : t("masked.noMessage")}
                    </span>
                  </span>
                  <CopyButton value={address.email} />
                  {address.state === "disabled" || address.state === "deleted" ? (
                    <Button
                      size="sm"
                      icon={Power}
                      busy={busy}
                      onClick={() => change.mutate({ id: address.id, state: "enabled" })}
                    >
                      {t("masked.enable")}
                    </Button>
                  ) : (
                    <Button
                      size="sm"
                      icon={PowerOff}
                      busy={busy}
                      onClick={() => change.mutate({ id: address.id, state: "disabled" })}
                    >
                      {t("masked.disable")}
                    </Button>
                  )}
                  {address.state !== "deleted" && (
                    <Button
                      size="sm"
                      variant="danger"
                      icon={Trash2}
                      disabled={busy}
                      onClick={() => {
                        if (window.confirm(t("masked.deleteConfirm", { address: address.email }))) {
                          change.mutate({ id: address.id, state: "deleted" });
                        }
                      }}
                    >
                      {t("masked.delete")}
                    </Button>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </Card>
  );
}

/** My account → Masked addresses: a random address for each website. */
export function MaskedPage() {
  const { t } = useT();
  const query = useQuery({ queryKey: maskedKey, queryFn: () => api<MaskedAddressesView>("/api/account/masked") });
  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  return (
    <div className="flex flex-col gap-5">
      <PageHeader
        title={
          <span className="flex items-center gap-2">
            <EyeOff className="size-5 text-muted" aria-hidden />
            {t("masked.pageTitle")}
          </span>
        }
        intro={t("masked.intro")}
      />
      <CreateCard
        key={`${query.data.domains.join()}:${query.data.defaultDomain ?? ""}`}
        domains={query.data.domains}
        defaultDomain={query.data.defaultDomain}
      />
      <AddressList addresses={query.data.addresses} />
    </div>
  );
}

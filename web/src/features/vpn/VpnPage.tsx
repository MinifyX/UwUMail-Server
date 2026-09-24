import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import clsx from "clsx";
import {
  CircleCheck,
  CircleHelp,
  FileUp,
  Globe,
  LoaderCircle,
  Play,
  Power,
  Save,
  ScrollText,
  Trash2,
  TriangleAlert,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card, CopyButton } from "@/components/ui/Card";
import { Dialog } from "@/components/ui/Dialog";
import { Field, Segmented, Select, TextInput } from "@/components/ui/Field";
import { updateCommand } from "@/features/admin/host";
import { ChoiceField, LockedHint, Section, ToggleField, type Form } from "@/features/settings/SettingsPage";
import { useT } from "@/i18n";
import {
  api,
  type EgressTest,
  type EgressView,
  type SettingsView,
  type VpnChange,
  type VpnConfig,
  type VpnFiles,
  type VpnKind,
  type VpnView,
} from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatRelative } from "@/lib/format";
import { toast } from "@/state/toasts";
import { detectProvider, parseWireguardConf } from "./wireguard";

const vpnKey = ["admin", "vpn"] as const;
const egressKey = ["admin", "egress"] as const;

/** A proxy failure this recent still counts as trouble. */
const RECENT_SECONDS = 15 * 60;

type Tone = "good" | "trouble" | "neutral" | "busy";

const TONES: Record<Tone, { icon: LucideIcon; className: string }> = {
  good: { icon: CircleCheck, className: "bg-success-tint text-success" },
  trouble: { icon: TriangleAlert, className: "bg-warning-tint text-warning" },
  neutral: { icon: CircleHelp, className: "bg-elevated text-muted" },
  busy: { icon: LoaderCircle, className: "bg-pink-tint text-pink-ink" },
};

function State({ tone, children }: { tone: Tone; children: ReactNode }) {
  const { icon: Icon, className } = TONES[tone];
  return (
    <span
      className={clsx("inline-flex h-7 items-center gap-1.5 rounded-full px-3 text-[13px] font-semibold", className)}
    >
      <Icon className={clsx("size-4", tone === "busy" && "animate-spin")} aria-hidden />
      {children}
    </span>
  );
}

const textareaClass =
  "w-full rounded-control border border-line bg-surface px-3.5 py-2.5 font-mono text-[12px] text-ink placeholder:text-faint focus:border-pink focus:shadow-focus focus:outline-none";

/** Where the portal can say where a provider hides the key; the rest get the general hint. */
const KEY_HINTS = new Set(["nordvpn", "mullvad", "protonvpn", "surfshark", "ivpn", "airvpn", "windscribe"]);

/** How requests leave right now, with the VPN container's state and a test. */
function StatusCard({ vpn }: { vpn: VpnView }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const query = useQuery({ queryKey: egressKey, queryFn: () => api<EgressView>("/api/admin/egress") });
  const test = useMutation({
    mutationFn: () => api<EgressTest>("/api/admin/egress/test", { method: "POST" }),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: egressKey }),
  });
  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  const view = query.data;
  const failure = view.lastProxyFailure;
  const recent = failure !== null && failure.at > query.dataUpdatedAt / 1000 - RECENT_SECONDS;
  const container = vpn.helper.vpn;
  const throughVpn = view.proxy === vpn.proxy.gluetun;
  const containerUp = container?.state === "running" && container.health !== "unhealthy";

  let tone: Tone = "neutral";
  let text = t("vpn.status.direct");
  if (view.proxy) {
    tone = recent ? "trouble" : "good";
    text = throughVpn ? t("vpn.status.vpn") : t("vpn.status.proxy");
    if (throughVpn && container && !containerUp) {
      tone = container.health === "starting" ? "busy" : "trouble";
      text = container.health === "starting" ? t("vpn.status.starting") : t("vpn.status.vpnDown");
    }
  }
  const routes = view.routes;

  return (
    <Card title={t("vpn.status.title")}>
      <div className="flex flex-col gap-4">
        <div className="flex flex-wrap items-center gap-2">
          <State tone={tone}>{text}</State>
          {view.proxy && <code className="text-[13px] text-muted">{view.proxy}</code>}
        </div>
        <dl className="grid gap-x-4 gap-y-2 text-[13px] sm:grid-cols-[200px_1fr]">
          {container && (
            <>
              <dt className="font-semibold text-muted">{t("vpn.status.container")}</dt>
              <dd>
                {container.state === "missing"
                  ? t("vpn.status.containerMissing")
                  : t("vpn.status.containerState", {
                      state: container.state,
                      health: container.health || "–",
                      provider: container.provider || "–",
                    })}
              </dd>
            </>
          )}
          {view.proxy && routes && (
            <>
              <dt className="font-semibold text-muted">{t("vpn.status.through")}</dt>
              <dd>
                {(["pictures", "updates", "fetch"] as const)
                  .filter((route) => routes[route])
                  .map((route) => t(`vpn.routes.${route}Short`))
                  .join(", ") || t("vpn.status.nothing")}
              </dd>
            </>
          )}
          {view.proxy && (
            <>
              <dt className="font-semibold text-muted">{t("settings.egress.fallback")}</dt>
              <dd>{t(`settings.egress.fallbackOptions.${view.fallback}`)}</dd>
            </>
          )}
          <dt className="font-semibold text-muted">{t("vpn.status.pictures")}</dt>
          <dd>{t("settings.egress.counts", { fetched: view.fetched, failed: view.failed })}</dd>
        </dl>
        {failure && (
          <p
            className={clsx(
              "rounded-control px-3 py-2 text-[13px]",
              recent ? "bg-warning-tint text-warning" : "bg-canvas text-muted",
            )}
          >
            {t("settings.egress.proxyFailed", {
              times: view.proxyFailures,
              when: formatRelative(failure.at, i18n.language),
              error: failure.error,
            })}
            {view.fallbacks > 0 && ` ${t("settings.egress.fallbacks", { times: view.fallbacks })}`}
          </p>
        )}
        <div className="flex flex-wrap items-center justify-between gap-3">
          <p className="max-w-prose text-[13px] text-muted">{t("vpn.status.testHint")}</p>
          <Button icon={Globe} busy={test.isPending} onClick={() => test.mutate()}>
            {t("settings.egress.test")}
          </Button>
        </div>
        {test.isError && (
          <p className="rounded-control bg-danger-tint px-3 py-2 text-[13px] text-danger">{errorText(test.error)}</p>
        )}
        {test.data?.address && (
          <p className="rounded-control bg-success-tint px-3 py-2 text-[13px] text-success">
            {t(test.data.proxied ? "settings.egress.testProxied" : "settings.egress.testDirect", {
              address: test.data.address,
            })}
          </p>
        )}
        {test.data?.error && (
          <p className="rounded-control bg-danger-tint px-3 py-2 text-[13px] text-danger">
            {t("settings.egress.testFailed", { error: test.data.error })}
          </p>
        )}
      </div>
    </Card>
  );
}

/** A secret the server never sends back: empty means "keep", and a stored one can be removed. */
function SecretInput({
  id,
  set,
  value,
  onChange,
  placeholder,
}: {
  id: string;
  set: boolean;
  value: string | undefined;
  onChange: (value: string | undefined) => void;
  placeholder?: string;
}) {
  const { t } = useT();
  const removed = value === "";
  return (
    <div className="flex gap-2">
      <TextInput
        id={id}
        type="password"
        autoComplete="off"
        spellCheck={false}
        className="font-mono"
        placeholder={removed ? t("vpn.form.secretRemoved") : set ? t("vpn.form.secretSet") : placeholder}
        value={value ?? ""}
        onChange={(event) => onChange(event.target.value === "" ? undefined : event.target.value)}
      />
      {set && !removed && value === undefined && (
        <Button size="sm" className="self-center" onClick={() => onChange("")}>
          {t("common.remove")}
        </Button>
      )}
    </div>
  );
}

type Draft = VpnChange;

function draftOf(config: VpnConfig): Draft {
  return {
    provider: config.provider || "nordvpn",
    kind: config.kind,
    countries: config.countries,
    regions: config.regions,
    cities: config.cities,
    hostnames: config.hostnames,
    wireguardAddresses: config.wireguardAddresses,
    wireguardPublicKey: config.wireguardPublicKey,
    wireguardEndpointIp: config.wireguardEndpointIp,
    wireguardEndpointPort: config.wireguardEndpointPort,
    openvpnUser: config.openvpnUser,
  };
}

/** The VPN itself: provider, keys and location, saved here and started by the machine's helper. */
function VpnCard({ view }: { view: VpnView }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [draft, setDraft] = useState<Draft>(() => draftOf(view.config));
  const [advanced, setAdvanced] = useState(Boolean(view.config.regions || view.config.hostnames));
  const [files, setFiles] = useState<VpnFiles | null>(null);
  const [showLog, setShowLog] = useState(false);
  const [removing, setRemoving] = useState(false);
  const confFile = useRef<HTMLInputElement>(null);
  const ovpnFile = useRef<HTMLInputElement>(null);

  const provider = view.providers.find((item) => item.id === draft.provider);
  const custom = draft.provider === "custom";
  const set = (change: Partial<Draft>) => setDraft((current) => ({ ...current, ...change }));
  const dirty = JSON.stringify(draft) !== JSON.stringify(draftOf(view.config));
  const job = view.job;
  const running = job?.state === "running" || job?.state === "waiting";
  const container = view.helper.vpn;
  const up = container?.state === "running";
  // Anything of the VPN still there: a container in any state (also one that keeps restarting), one
  // that would come back with the next start, or the way out still pointing at it.
  const active =
    (container !== null && container !== undefined && container.state !== "missing") ||
    Boolean(container?.always) ||
    view.proxy.current === view.proxy.gluetun;

  const saved = (next: VpnView) => {
    queryClient.setQueryData(vpnKey, next);
    void queryClient.invalidateQueries({ queryKey: egressKey });
    void queryClient.invalidateQueries({ queryKey: ["admin", "settings"] });
    setDraft(draftOf(next.config));
  };
  const save = useMutation({
    mutationFn: () => api<VpnView>("/api/admin/vpn", { method: "PUT", body: draft }),
    onSuccess: (next) => {
      saved(next);
      toast(t("vpn.form.saved"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const apply = useMutation({
    mutationFn: async () => {
      if (dirty) await api<VpnView>("/api/admin/vpn", { method: "PUT", body: draft });
      return api<VpnView>("/api/admin/vpn/apply", { method: "POST" });
    },
    onSuccess: (next) => {
      saved(next);
      setShowLog(true);
      toast(t("vpn.form.starting"), "info");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const stop = useMutation({
    mutationFn: () => api<VpnView>("/api/admin/vpn/stop", { method: "POST" }),
    onSuccess: (next) => {
      saved(next);
      toast(t(view.helper.canVpn ? "vpn.form.stopping" : "vpn.form.stoppedByHand"), "info");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const remove = useMutation({
    mutationFn: () => api<VpnView>("/api/admin/vpn/remove", { method: "POST" }),
    onSuccess: (next) => {
      setRemoving(false);
      saved(next);
      setShowLog(false);
      toast(t(view.helper.canVpn ? "vpn.form.removed" : "vpn.form.removedByHand"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const showFiles = useMutation({
    mutationFn: async () => {
      if (dirty) await api<VpnView>("/api/admin/vpn", { method: "PUT", body: draft });
      return api<VpnFiles>("/api/admin/vpn/files", { method: "POST" });
    },
    onSuccess: setFiles,
    onError: (error) => toast(errorText(error), "error"),
  });
  const useGluetun = useMutation({
    mutationFn: () => api<VpnView>("/api/admin/vpn/use-gluetun", { method: "POST" }),
    onSuccess: (next) => {
      saved(next);
      toast(t("vpn.form.proxySet"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  const readConf = async (file: File | undefined) => {
    if (!file) return;
    const parsed = parseWireguardConf(await file.text());
    if (!parsed) {
      toast(t("vpn.form.confUnreadable"), "error");
      return;
    }
    // The file says whose it is: NordVPN's endpoints are *.nordhold.net, and so on. A file nobody
    // knows is an own server, which gluetun then needs the server's key and address for.
    const detected = detectProvider(parsed, file.name);
    const provider = view.providers.find((item) => item.id === detected.provider && item.wireguard);
    const target = provider?.id ?? "custom";
    set({
      provider: target,
      kind: "wireguard",
      wireguardPrivateKey: parsed.privateKey,
      wireguardPresharedKey: parsed.presharedKey,
      wireguardAddresses: parsed.addresses,
      ...(target === "custom"
        ? {
            wireguardPublicKey: parsed.publicKey,
            wireguardEndpointIp: parsed.endpointIp,
            wireguardEndpointPort: parsed.endpointPort,
            countries: "",
            cities: "",
          }
        : { countries: detected.country ?? draft.countries, cities: detected.country ? "" : draft.cities }),
    });
    toast(
      provider
        ? t("vpn.form.confDetected", {
            provider: provider.name,
            where: [detected.server, detected.country].filter(Boolean).join(", ") || "–",
          })
        : t("vpn.form.confCustom"),
      "success",
    );
  };
  const readOvpn = async (file: File | undefined) => {
    if (!file) return;
    set({ kind: "openvpn", openvpnConfig: await file.text() });
  };

  const wireguard = draft.kind === "wireguard";
  const kinds: { value: VpnKind; label: string }[] = [];
  if (provider?.wireguard ?? true) kinds.push({ value: "wireguard", label: "WireGuard" });
  if (provider?.openvpn ?? true) kinds.push({ value: "openvpn", label: "OpenVPN" });

  return (
    <Card title={t("vpn.form.title")}>
      <div className="flex flex-col gap-5">
        <p className="-mt-1 max-w-prose text-[13px] text-muted">{t("vpn.form.intro")}</p>

        {!view.helper.available ? (
          <p className="rounded-control bg-canvas px-3 py-2.5 text-[13px]">{t("vpn.helper.missing")}</p>
        ) : !view.helper.canVpn ? (
          <div className="flex flex-col gap-2 rounded-control bg-warning-tint px-3 py-2.5 text-[13px] text-warning">
            <p>{t("vpn.helper.old", { version: view.helper.version ?? "1" })}</p>
            <div className="flex items-start gap-2 rounded-control bg-surface px-3 py-1.5 text-ink">
              <code className="min-w-0 flex-1 font-mono text-[12px] break-all">{updateCommand(null)}</code>
              <CopyButton value={updateCommand(null)} />
            </div>
          </div>
        ) : null}

        <div className="grid gap-4 sm:grid-cols-2">
          <Field label={t("vpn.form.provider")}>
            {(id) => (
              <Select
                id={id}
                value={draft.provider}
                onChange={(event) => {
                  const next = view.providers.find((item) => item.id === event.target.value);
                  set({
                    provider: event.target.value,
                    kind: next && !next.wireguard ? "openvpn" : draft.kind,
                  });
                }}
              >
                <optgroup label={t("vpn.form.withWireguard")}>
                  {view.providers
                    .filter((item) => item.wireguard && item.id !== "custom")
                    .map((item) => (
                      <option key={item.id} value={item.id}>
                        {item.name}
                      </option>
                    ))}
                </optgroup>
                <optgroup label={t("vpn.form.openvpnOnly")}>
                  {view.providers
                    .filter((item) => !item.wireguard)
                    .map((item) => (
                      <option key={item.id} value={item.id}>
                        {item.name}
                      </option>
                    ))}
                </optgroup>
                <optgroup label={t("vpn.form.ownServer")}>
                  <option value="custom">{t("vpn.form.custom")}</option>
                </optgroup>
              </Select>
            )}
          </Field>
          <Field label={t("vpn.form.kind")}>
            {() => (
              <Segmented<VpnKind>
                label={t("vpn.form.kind")}
                value={draft.kind}
                onChange={(kind) => set({ kind })}
                options={kinds}
              />
            )}
          </Field>
        </div>

        {wireguard ? (
          <div className="flex flex-col gap-4">
            <div className="flex flex-wrap items-center gap-2">
              <input
                ref={confFile}
                type="file"
                accept=".conf,text/plain"
                className="hidden"
                onChange={(event) => void readConf(event.target.files?.[0])}
              />
              <Button size="sm" icon={FileUp} onClick={() => confFile.current?.click()}>
                {t("vpn.form.readConf")}
              </Button>
              <span className="text-[12px] text-muted">{t("vpn.form.readConfHint")}</span>
            </div>
            <Field
              label={t("vpn.form.privateKey")}
              hint={KEY_HINTS.has(draft.provider) ? t(`vpn.keyHints.${draft.provider}`) : t("vpn.form.privateKeyHint")}
            >
              {(id) => (
                <SecretInput
                  id={id}
                  set={view.secrets.wireguardPrivateKey}
                  value={draft.wireguardPrivateKey}
                  onChange={(wireguardPrivateKey) => set({ wireguardPrivateKey })}
                  placeholder="…="
                />
              )}
            </Field>
            {(provider?.needsAddresses || custom || draft.wireguardAddresses) && (
              <Field label={t("vpn.form.addresses")} hint={t("vpn.form.addressesHint")}>
                {(id) => (
                  <TextInput
                    id={id}
                    className="font-mono"
                    placeholder="10.64.0.2/32"
                    value={draft.wireguardAddresses}
                    onChange={(event) => set({ wireguardAddresses: event.target.value })}
                  />
                )}
              </Field>
            )}
            {custom && (
              <div className="grid gap-4 sm:grid-cols-[1fr_8rem]">
                <Field label={t("vpn.form.endpointIp")} hint={t("vpn.form.endpointIpHint")}>
                  {(id) => (
                    <TextInput
                      id={id}
                      className="font-mono"
                      placeholder="203.0.113.10"
                      value={draft.wireguardEndpointIp}
                      onChange={(event) => set({ wireguardEndpointIp: event.target.value })}
                    />
                  )}
                </Field>
                <Field label={t("vpn.form.endpointPort")}>
                  {(id) => (
                    <TextInput
                      id={id}
                      inputMode="numeric"
                      placeholder="51820"
                      value={draft.wireguardEndpointPort ?? ""}
                      onChange={(event) =>
                        set({ wireguardEndpointPort: event.target.value ? Number(event.target.value) : null })
                      }
                    />
                  )}
                </Field>
                <Field label={t("vpn.form.publicKey")} className="sm:col-span-2">
                  {(id) => (
                    <TextInput
                      id={id}
                      className="font-mono"
                      value={draft.wireguardPublicKey}
                      onChange={(event) => set({ wireguardPublicKey: event.target.value })}
                    />
                  )}
                </Field>
              </div>
            )}
            {advanced && (
              <Field label={t("vpn.form.presharedKey")} hint={t("vpn.form.presharedKeyHint")}>
                {(id) => (
                  <SecretInput
                    id={id}
                    set={view.secrets.wireguardPresharedKey}
                    value={draft.wireguardPresharedKey}
                    onChange={(wireguardPresharedKey) => set({ wireguardPresharedKey })}
                  />
                )}
              </Field>
            )}
          </div>
        ) : custom ? (
          <Field
            label={t("vpn.form.ovpn")}
            hint={view.secrets.openvpnConfig ? t("vpn.form.ovpnSet") : t("vpn.form.ovpnHint")}
          >
            {(id) => (
              <div className="flex flex-col gap-2">
                <textarea
                  id={id}
                  rows={6}
                  spellCheck={false}
                  className={textareaClass}
                  placeholder={
                    view.secrets.openvpnConfig ? t("vpn.form.secretSet") : "client\nremote vpn.example.net 1194\n…"
                  }
                  value={draft.openvpnConfig ?? ""}
                  onChange={(event) => set({ openvpnConfig: event.target.value || undefined })}
                />
                <input
                  ref={ovpnFile}
                  type="file"
                  accept=".ovpn,.conf,text/plain"
                  className="hidden"
                  onChange={(event) => void readOvpn(event.target.files?.[0])}
                />
                <div>
                  <Button size="sm" icon={FileUp} onClick={() => ovpnFile.current?.click()}>
                    {t("vpn.form.readOvpn")}
                  </Button>
                </div>
              </div>
            )}
          </Field>
        ) : (
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label={t("vpn.form.user")} hint={t("vpn.form.userHint")}>
              {(id) => (
                <TextInput
                  id={id}
                  autoComplete="off"
                  spellCheck={false}
                  value={draft.openvpnUser}
                  onChange={(event) => set({ openvpnUser: event.target.value })}
                />
              )}
            </Field>
            <Field label={t("vpn.form.password")}>
              {(id) => (
                <SecretInput
                  id={id}
                  set={view.secrets.openvpnPassword}
                  value={draft.openvpnPassword}
                  onChange={(openvpnPassword) => set({ openvpnPassword })}
                />
              )}
            </Field>
          </div>
        )}

        {!custom && (
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label={t("vpn.form.countries")} hint={t("vpn.form.countriesHint")}>
              {(id) => (
                <TextInput
                  id={id}
                  placeholder="Switzerland, Netherlands"
                  value={draft.countries}
                  onChange={(event) => set({ countries: event.target.value })}
                />
              )}
            </Field>
            <Field label={t("vpn.form.cities")} hint={t("vpn.form.citiesHint")}>
              {(id) => (
                <TextInput
                  id={id}
                  placeholder="Zurich"
                  value={draft.cities}
                  onChange={(event) => set({ cities: event.target.value })}
                />
              )}
            </Field>
            {advanced && (
              <>
                <Field label={t("vpn.form.regions")}>
                  {(id) => (
                    <TextInput
                      id={id}
                      value={draft.regions}
                      onChange={(event) => set({ regions: event.target.value })}
                    />
                  )}
                </Field>
                <Field label={t("vpn.form.hostnames")} hint={t("vpn.form.hostnamesHint")}>
                  {(id) => (
                    <TextInput
                      id={id}
                      value={draft.hostnames}
                      onChange={(event) => set({ hostnames: event.target.value })}
                    />
                  )}
                </Field>
              </>
            )}
          </div>
        )}
        <button
          type="button"
          className="self-start text-[13px] font-semibold text-pink-ink hover:underline"
          onClick={() => setAdvanced((current) => !current)}
        >
          {advanced ? t("vpn.form.lessOptions") : t("vpn.form.moreOptions")}
        </button>

        {view.complete && view.saved && !dirty && (
          <p className="rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning">
            {t("vpn.form.incomplete", { reason: view.complete })}
          </p>
        )}

        <div className="flex flex-wrap items-center justify-end gap-2 border-t border-hairline pt-4">
          {job && (
            <button
              type="button"
              onClick={() => setShowLog((current) => !current)}
              className="mr-auto inline-flex items-center gap-1.5 text-[13px] text-muted hover:text-ink"
            >
              <ScrollText className="size-4" aria-hidden />
              {job.state === "waiting"
                ? t("vpn.job.waiting")
                : running
                  ? t("vpn.job.running")
                  : job.state === "done"
                    ? t("vpn.job.done", { when: formatRelative(job.at, i18n.language) })
                    : t("vpn.job.failed", { error: job.error })}
            </button>
          )}
          <Button icon={Save} busy={save.isPending} disabled={!dirty} onClick={() => save.mutate()}>
            {t("common.save")}
          </Button>
          {(view.saved || active) && (
            <Button variant="ghost" icon={Trash2} disabled={running} onClick={() => setRemoving(true)}>
              {t("vpn.form.remove")}
            </Button>
          )}
          {active && (
            <Button variant="danger" icon={Power} busy={stop.isPending} onClick={() => stop.mutate()}>
              {t("vpn.form.stop")}
            </Button>
          )}
          {view.helper.canVpn ? (
            <>
              <Button variant="primary" icon={Play} busy={apply.isPending || running} onClick={() => apply.mutate()}>
                {up ? t("vpn.form.restart") : t("vpn.form.start")}
              </Button>
            </>
          ) : (
            <>
              <Button busy={useGluetun.isPending} disabled={view.proxy.locked} onClick={() => useGluetun.mutate()}>
                {t("vpn.form.useGluetun")}
              </Button>
              <Button variant="primary" busy={showFiles.isPending} onClick={() => showFiles.mutate()}>
                {t("vpn.form.showFiles")}
              </Button>
            </>
          )}
        </div>
        {view.proxy.locked && (
          <p className="text-[12px] text-muted">
            <LockedHint /> {t("vpn.form.proxyLocked")}
          </p>
        )}
        {showLog && job && (
          <pre className="max-h-72 overflow-auto rounded-control bg-canvas p-3 font-mono text-[12px] whitespace-pre-wrap">
            {view.log || t("vpn.job.waiting")}
          </pre>
        )}
      </div>

      <Dialog open={removing} onClose={() => setRemoving(false)} title={t("vpn.remove.title")} width="sm">
        <div className="flex flex-col gap-4 px-6 pt-2 pb-6 text-[13px]">
          <p className="text-muted">{t("vpn.remove.body")}</p>
          <div className="flex flex-wrap justify-end gap-2">
            <Button autoFocus onClick={() => setRemoving(false)}>
              {t("common.cancel")}
            </Button>
            <Button variant="danger" icon={Trash2} busy={remove.isPending} onClick={() => remove.mutate()}>
              {t("vpn.remove.confirm")}
            </Button>
          </div>
        </div>
      </Dialog>

      <Dialog open={files !== null} onClose={() => setFiles(null)} title={t("vpn.files.title")} width="lg">
        {files && (
          <div className="flex flex-col gap-4 px-6 pt-2 pb-6 text-[13px]">
            <p className="text-muted">{t("vpn.files.intro")}</p>
            <div className="flex items-center justify-between gap-2">
              <h3 className="font-bold">.env.vpn</h3>
              <CopyButton value={files.envFile} />
            </div>
            <pre className="overflow-auto rounded-control bg-canvas p-3 font-mono text-[12px]">{files.envFile}</pre>
            {files.ovpn && (
              <>
                <div className="flex items-center justify-between gap-2">
                  <h3 className="font-bold">vpn/custom.ovpn</h3>
                  <CopyButton value={files.ovpn} />
                </div>
                <pre className="max-h-48 overflow-auto rounded-control bg-canvas p-3 font-mono text-[12px]">
                  {files.ovpn}
                </pre>
              </>
            )}
            <h3 className="font-bold">{t("vpn.files.then")}</h3>
            <pre className="overflow-auto rounded-control bg-canvas p-3 font-mono text-[12px]">
              {"cd /opt/uwumail\nsudo docker compose --profile vpn up -d"}
            </pre>
            <p className="text-muted">{t("vpn.files.after")}</p>
          </div>
        )}
      </Dialog>
    </Card>
  );
}

/** Which requests take the VPN or proxy, and a proxy of one's own instead of the VPN. */
function RoutesFields({ form, vpn }: { form: Form; vpn: VpnView }) {
  const { t } = useT();
  const locked = form.locked("egress.proxy");
  const current = vpn.proxy.current;
  const pending = form.pending["egress.proxy"];
  return (
    <>
      <ToggleField
        form={form}
        settingKey="egress.pictures"
        label={t("vpn.routes.pictures")}
        hint={t("vpn.routes.picturesHint")}
      />
      <ToggleField
        form={form}
        settingKey="egress.updates"
        label={t("vpn.routes.updates")}
        hint={t("vpn.routes.updatesHint")}
      />
      <ToggleField
        form={form}
        settingKey="egress.fetch"
        label={t("vpn.routes.fetch")}
        hint={t("vpn.routes.fetchHint")}
      />
      <ChoiceField
        form={form}
        settingKey="egress.fallback"
        label={t("settings.egress.fallback")}
        hint={t("vpn.routes.fallbackHint")}
        segmented
        options={[
          { value: "block", label: t("settings.egress.fallbackOptions.block") },
          { value: "direct", label: t("settings.egress.fallbackOptions.direct") },
        ]}
      />
      <Field
        label={t("vpn.routes.proxy")}
        hint={
          locked ? (
            <LockedHint />
          ) : pending === null ? (
            t("vpn.routes.proxyRemoved")
          ) : current ? (
            t("vpn.routes.proxyCurrent", { proxy: current })
          ) : (
            t("vpn.routes.proxyHint")
          )
        }
      >
        {(id) => (
          <div className="flex gap-2">
            <TextInput
              id={id}
              disabled={locked}
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              placeholder={current ?? "socks5://user:password@proxy.example:1080"}
              value={typeof pending === "string" ? pending : ""}
              onChange={(event) => form.set("egress.proxy", event.target.value.trim() || undefined)}
            />
            {current && !locked && (
              <Button size="sm" className="self-center" onClick={() => form.set("egress.proxy", null)}>
                {t("vpn.routes.direct")}
              </Button>
            )}
          </div>
        )}
      </Field>
    </>
  );
}

export function VpnPage() {
  const { t } = useT();
  const queryClient = useQueryClient();
  const vpn = useQuery({ queryKey: vpnKey, queryFn: () => api<VpnView>("/api/admin/vpn") });
  const settings = useQuery({
    queryKey: ["admin", "settings"],
    queryFn: () => api<SettingsView>("/api/admin/settings"),
  });
  const job = vpn.data?.job;
  const running = job?.state === "running" || job?.state === "waiting";

  // While the helper works, the page follows it.
  useEffect(() => {
    if (!running) return;
    const timer = window.setInterval(() => {
      void queryClient.invalidateQueries({ queryKey: vpnKey });
      void queryClient.invalidateQueries({ queryKey: egressKey });
    }, 2000);
    return () => window.clearInterval(timer);
  }, [running, queryClient]);

  if (vpn.isPending || settings.isPending) return <Loading />;
  if (vpn.isError) return <LoadError error={vpn.error} onRetry={() => void vpn.refetch()} />;
  if (settings.isError) return <LoadError error={settings.error} onRetry={() => void settings.refetch()} />;

  return (
    <div className="flex flex-col gap-5">
      <StatusCard vpn={vpn.data} />
      <VpnCard key={JSON.stringify(vpn.data.config)} view={vpn.data} />
      <Section
        title={t("vpn.routes.title")}
        intro={t("vpn.routes.intro")}
        view={settings.data}
        keys={["egress.pictures", "egress.updates", "egress.fetch", "egress.fallback", "egress.proxy"]}
        onSaved={() => {
          void queryClient.invalidateQueries({ queryKey: vpnKey });
          void queryClient.invalidateQueries({ queryKey: egressKey });
        }}
      >
        {(form) => <RoutesFields form={form} vpn={vpn.data} />}
      </Section>
    </div>
  );
}

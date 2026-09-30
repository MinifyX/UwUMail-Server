import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import clsx from "clsx";
import { ArrowUp, FileUp, Image, Info, RefreshCw, Save, TriangleAlert, Trash2, Upload } from "lucide-react";
import { useRef, useState, type ReactNode } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card, KeyValue } from "@/components/ui/Card";
import { Field, Segmented, TextInput, Toggle } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, ApiError, type BimiView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatBytes, formatDate, formatDateTime, formatRelative } from "@/lib/format";
import { toast } from "@/state/toasts";
import { certificateWarnings, dmarcReasons, isHexColor, logoPreviewPath } from "./bimi";
import { CHECK_STYLES, Value } from "./DnsBits";

/** The upload limit of the server; the cleaned logo has to be much smaller still. */
const MAX_UPLOAD = 256 * 1024;

/** Where the domain page's DNS card is, for the links to it. */
export const DNS_CARD_ID = "domain-dns";

const bimiPath = (domain: string) => `/api/admin/domains/${encodeURIComponent(domain)}/bimi`;
const bimiKey = (domain: string) => ["admin", "domains", domain, "bimi"];

/** A change to the domain's BIMI that answers with the new view. */
function useBimiChange<Input>(domain: string, run: (input: Input) => Promise<BimiView>, success?: string) {
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  return useMutation({
    mutationFn: run,
    onSuccess: (view) => {
      queryClient.setQueryData(bimiKey(domain), view);
      // The DNS check lists the BIMI record while BIMI is on.
      void queryClient.invalidateQueries({ queryKey: ["admin", "domains", domain], exact: true });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
      if (success) toast(success, "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}

/** The stored logo as a data address for an <img>, where it cannot run anything. */
function useLogoPreview(domain: string, view: BimiView) {
  return useQuery({
    queryKey: [...bimiKey(domain), "logo", view.svgUpdatedAt],
    enabled: view.hasSvg,
    staleTime: Infinity,
    queryFn: async () => {
      const response = await fetch(logoPreviewPath(domain, view.svgUpdatedAt), { credentials: "same-origin" });
      if (!response.ok) throw new ApiError(response.status, "internal", response.statusText);
      return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(await response.text())}`;
    },
  });
}

function Subheading({ children }: { children: ReactNode }) {
  return <h3 className="text-sm font-bold">{children}</h3>;
}

function Previews({ domain, view }: { domain: string; view: BimiView }) {
  const { t } = useT();
  const preview = useLogoPreview(domain, view);
  const shapes = [
    { round: true, label: t("bimi.preview.circle") },
    { round: false, label: t("bimi.preview.square") },
  ];
  return (
    <div className="flex flex-wrap gap-4">
      {shapes.map((shape) => (
        <figure key={shape.label} className="flex flex-col items-center gap-1.5">
          <span
            className={clsx(
              "flex size-20 items-center justify-center overflow-hidden border border-hairline bg-canvas text-faint",
              shape.round ? "rounded-full" : "rounded-card",
            )}
          >
            {preview.data ? (
              <img src={preview.data} alt={t("bimi.preview.alt", { domain })} className="size-full object-cover" />
            ) : (
              <Image className="size-8" aria-hidden />
            )}
          </span>
          <figcaption className="text-[12px] text-muted">{shape.label}</figcaption>
        </figure>
      ))}
    </div>
  );
}

type BackgroundMode = "solid" | "keep";

/** Upload a new SVG with its name and background, or change only the name. */
function LogoForm({ domain, view }: { domain: string; view: BimiView }) {
  const { t, i18n } = useT();
  const input = useRef<HTMLInputElement>(null);
  const [file, setFile] = useState<{ name: string; text: string } | null>(null);
  const [title, setTitle] = useState(view.title);
  const [mode, setMode] = useState<BackgroundMode>("solid");
  const [color, setColor] = useState("#ffffff");
  const upload = useBimiChange(
    domain,
    (body: { svg: string; title?: string; background: string | null }) =>
      api<BimiView>(`${bimiPath(domain)}/svg`, { method: "PUT", body }),
    t("bimi.logo.savedToast"),
  );
  const rename = useBimiChange(
    domain,
    (name: string) => api<BimiView>(bimiPath(domain), { method: "PUT", body: { title: name } }),
    t("bimi.logo.savedToast"),
  );
  const remove = useBimiChange(
    domain,
    () => api<BimiView>(`${bimiPath(domain)}/svg`, { method: "DELETE" }),
    t("bimi.logo.removedToast"),
  );
  const changedTitle = title.trim() !== view.title && title.trim() !== "";
  const busy = upload.isPending || rename.isPending;

  const choose = async (chosen: File) => {
    if (chosen.size > MAX_UPLOAD) {
      toast(t("errors.codes.bimiSvgTooLarge"), "error");
      return;
    }
    try {
      setFile({ name: chosen.name, text: await chosen.text() });
    } catch {
      toast(t("bimi.logo.readFailed"), "error");
    }
  };

  const save = () => {
    const name = title.trim();
    if (file) {
      upload.mutate(
        { svg: file.text, ...(name ? { title: name } : {}), background: mode === "solid" ? color : null },
        { onSuccess: () => setFile(null) },
      );
    } else if (changedTitle) {
      rename.mutate(name);
    }
  };

  return (
    <div className="flex flex-col gap-3">
      <Subheading>{t("bimi.logo.title")}</Subheading>
      <div className="flex flex-wrap items-center gap-4">
        <Previews domain={domain} view={view} />
        <div className="min-w-0 flex-1 basis-40 text-[12px] text-muted">
          {view.hasSvg && view.svgBytes !== null && view.svgUpdatedAt !== null
            ? t("bimi.logo.stored", {
                size: formatBytes(view.svgBytes, i18n.language),
                time: formatRelative(view.svgUpdatedAt, i18n.language),
              })
            : t("bimi.preview.none")}
        </div>
      </div>
      {view.domainLogo && !view.hasSvg && (
        <p className="flex gap-2 rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
          <Info className="mt-0.5 size-4 shrink-0" aria-hidden />
          {t("bimi.logo.pixelLogo")}
        </p>
      )}
      <p className="text-[12px] text-muted">{t("bimi.logo.hint")}</p>
      <div className="flex flex-wrap items-center gap-2">
        <input
          ref={input}
          type="file"
          accept=".svg,image/svg+xml"
          className="hidden"
          onChange={(event) => {
            const chosen = event.target.files?.[0];
            event.target.value = "";
            if (chosen) void choose(chosen);
          }}
        />
        <Button icon={Upload} onClick={() => input.current?.click()}>
          {t(view.hasSvg ? "bimi.logo.replace" : "bimi.logo.choose")}
        </Button>
        {file && (
          <span className="min-w-0 truncate text-[13px] text-muted">{t("bimi.logo.chosen", { name: file.name })}</span>
        )}
      </div>
      <Field label={t("bimi.logo.titleLabel")} hint={t("bimi.logo.titleHint")}>
        {(id) => (
          <TextInput
            id={id}
            value={title}
            maxLength={200}
            placeholder={domain}
            onChange={(event) => setTitle(event.target.value)}
          />
        )}
      </Field>
      {file && (
        <div className="flex flex-col gap-2">
          <span className="text-[13px] font-semibold text-muted">{t("bimi.logo.background")}</span>
          <Segmented<BackgroundMode>
            label={t("bimi.logo.background")}
            value={mode}
            onChange={setMode}
            options={[
              { value: "solid", label: t("bimi.logo.backgroundSolid") },
              { value: "keep", label: t("bimi.logo.backgroundKeep") },
            ]}
          />
          {mode === "solid" && (
            <input
              type="color"
              aria-label={t("bimi.logo.color")}
              value={isHexColor(color) ? color : "#ffffff"}
              onChange={(event) => setColor(event.target.value)}
              className="h-11 w-14 shrink-0 cursor-pointer rounded-control border border-line bg-surface p-1"
            />
          )}
          <p className="text-[12px] text-muted">{t("bimi.logo.backgroundHint")}</p>
        </div>
      )}
      <div className="flex flex-wrap gap-2">
        <Button variant="primary" icon={Save} busy={busy} disabled={!file && !changedTitle} onClick={save}>
          {t("bimi.logo.save")}
        </Button>
        {view.hasSvg && (
          <Button
            variant="ghost"
            icon={Trash2}
            busy={remove.isPending}
            onClick={() => {
              if (window.confirm(t("bimi.logo.removeConfirm"))) remove.mutate(undefined);
            }}
          >
            {t("bimi.logo.remove")}
          </Button>
        )}
      </div>
    </div>
  );
}

function RecordSection({ domain, view }: { domain: string; view: BimiView }) {
  const { t, i18n } = useT();
  const check = useBimiChange(
    domain,
    () => api<BimiView>(`${bimiPath(domain)}/check`, { method: "POST", body: {} }),
    t("bimi.record.checkedToast"),
  );
  const published = view.published;
  const style = published ? CHECK_STYLES[published.status] : null;
  const note =
    published?.note && ["bimiElsewhere", "bimiMultiple"].includes(published.note)
      ? t(`domains.detail.notes.${published.note}`)
      : null;
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <Subheading>{t("bimi.record.title")}</Subheading>
        <Button size="sm" icon={RefreshCw} busy={check.isPending} onClick={() => check.mutate(undefined)}>
          {check.isPending ? t("bimi.record.checking") : t("bimi.record.check")}
        </Button>
      </div>
      <p className="text-[13px] text-muted">{t("bimi.record.intro")}</p>
      <div className="grid gap-2 md:grid-cols-[80px_1fr] md:items-start">
        <span className="pt-1.5 text-[12px] font-semibold text-muted">{t("domains.detail.name")}</span>
        <Value value={view.record.name} label={t("domains.detail.copyName")} />
        <span className="pt-1.5 text-[12px] font-semibold text-muted">{t("domains.detail.value")}</span>
        <Value value={view.record.value} label={t("domains.detail.copyValue")} />
      </div>
      {published && style ? (
        <div className="flex flex-col gap-1.5">
          <div className="flex flex-wrap items-center gap-2">
            <span
              className={clsx(
                "inline-flex h-6 items-center gap-1 rounded-full px-2.5 text-[12px] font-semibold",
                style.className,
              )}
            >
              <style.icon className="size-3.5" aria-hidden />
              {t(`bimi.record.status.${published.status}`)}
            </span>
            <span className="text-[12px] text-faint">
              {t("bimi.record.checkedAt", { time: formatDateTime(published.checkedAt, i18n.language) })}
            </span>
          </div>
          {note && <p className="text-[13px] font-medium">{note}</p>}
          {published.found.length > 0 && published.status !== "ok" && (
            <div className="flex flex-col gap-1">
              <span className="text-[12px] font-semibold text-muted">{t("domains.detail.found")}</span>
              {published.found.map((value) => (
                <code key={value} className="text-[12px] break-all whitespace-pre-wrap text-muted">
                  {value}
                </code>
              ))}
            </div>
          )}
        </div>
      ) : (
        <p className="text-[12px] text-muted">{t(view.enabled ? "bimi.record.notChecked" : "bimi.record.whenOn")}</p>
      )}
    </div>
  );
}

function DmarcSection({ view }: { view: BimiView }) {
  const { t } = useT();
  const reasons = dmarcReasons(view.dmarc);
  const ok = reasons.length === 0;
  return (
    <div className="flex flex-col gap-2">
      <Subheading>{t("bimi.dmarc.title")}</Subheading>
      {ok ? (
        <p className="text-[13px] text-muted">{t("bimi.dmarc.ok")}</p>
      ) : (
        <div className="flex flex-col gap-1.5 rounded-control bg-warning-tint px-3 py-2.5 text-[13px] text-warning">
          <p className="font-semibold">{t("bimi.dmarc.needs")}</p>
          <ul className="flex list-disc flex-col gap-0.5 pl-5">
            {reasons.map((reason) => (
              <li key={reason}>
                {t(`bimi.dmarc.reasons.${reason}`, {
                  policy: view.dmarc.policy ?? "",
                  pct: view.dmarc.pct ?? 100,
                })}
              </li>
            ))}
          </ul>
        </div>
      )}
      {view.dmarc.record && (
        <code className="text-[12px] break-all whitespace-pre-wrap text-muted">{view.dmarc.record}</code>
      )}
      {!ok && (
        <Button
          size="sm"
          variant="ghost"
          icon={ArrowUp}
          className="self-start"
          onClick={() => document.getElementById(DNS_CARD_ID)?.scrollIntoView({ behavior: "smooth" })}
        >
          {t("bimi.dmarc.showDns")}
        </Button>
      )}
    </div>
  );
}

function CertificateSection({ domain, view }: { domain: string; view: BimiView }) {
  const { t, i18n } = useT();
  const input = useRef<HTMLInputElement>(null);
  const [pem, setPem] = useState("");
  const [replacing, setReplacing] = useState(false);
  const save = useBimiChange(
    domain,
    (text: string) => api<BimiView>(`${bimiPath(domain)}/certificate`, { method: "PUT", body: { pem: text } }),
    t("bimi.certificate.savedToast"),
  );
  const remove = useBimiChange(
    domain,
    () => api<BimiView>(`${bimiPath(domain)}/certificate`, { method: "DELETE" }),
    t("bimi.certificate.removedToast"),
  );
  const certificate = view.certificate;
  const language = i18n.language;

  const form = (
    <div className="flex flex-col gap-2">
      <Field label={t("bimi.certificate.paste")} hint={t("bimi.certificate.pasteHint")}>
        {(id) => (
          <textarea
            id={id}
            rows={4}
            spellCheck={false}
            placeholder="-----BEGIN CERTIFICATE-----"
            className="w-full rounded-control border border-line bg-surface px-3.5 py-2.5 font-mono text-[12px] focus:border-pink focus:shadow-focus focus:outline-none"
            value={pem}
            onChange={(event) => setPem(event.target.value)}
          />
        )}
      </Field>
      <input
        ref={input}
        type="file"
        accept=".pem,.crt,.cer,application/x-pem-file,application/x-x509-ca-cert"
        className="hidden"
        onChange={(event) => {
          const chosen = event.target.files?.[0];
          event.target.value = "";
          if (!chosen) return;
          chosen.text().then(setPem, () => toast(t("bimi.logo.readFailed"), "error"));
        }}
      />
      <div className="flex flex-wrap gap-2">
        <Button icon={FileUp} onClick={() => input.current?.click()}>
          {t("bimi.certificate.file")}
        </Button>
        <Button
          variant="primary"
          icon={Save}
          busy={save.isPending}
          disabled={!pem.trim()}
          onClick={() =>
            save.mutate(pem.trim(), {
              onSuccess: () => {
                setPem("");
                setReplacing(false);
              },
            })
          }
        >
          {t("bimi.certificate.save")}
        </Button>
      </div>
    </div>
  );

  return (
    <div className="flex flex-col gap-3">
      <Subheading>{t("bimi.certificate.title")}</Subheading>
      <p className="text-[13px] text-muted">{t("bimi.certificate.intro")}</p>
      {certificate ? (
        <>
          {certificateWarnings(certificate).map((warning) => (
            <p
              key={warning}
              className="flex gap-2 rounded-control bg-warning-tint px-3 py-2 text-[13px] font-medium text-warning"
            >
              <TriangleAlert className="mt-0.5 size-4 shrink-0" aria-hidden />
              {t(`bimi.certificate.warnings.${warning}`, { domain })}
            </p>
          ))}
          <div>
            <KeyValue label={t("bimi.certificate.kind")} value={t(`bimi.certificate.kinds.${certificate.kind}`)} />
            <KeyValue label={t("bimi.certificate.subject")} value={certificate.subject} />
            <KeyValue label={t("bimi.certificate.issuer")} value={certificate.issuer} />
            <KeyValue
              label={t("bimi.certificate.valid")}
              value={t("bimi.certificate.validRange", {
                from: formatDate(certificate.notBefore, language),
                to: formatDate(certificate.notAfter, language),
              })}
            />
            <KeyValue label={t("bimi.certificate.names")} value={certificate.names.join(", ") || "—"} />
          </div>
          <div className="flex flex-wrap gap-2">
            {!replacing && <Button onClick={() => setReplacing(true)}>{t("bimi.certificate.replace")}</Button>}
            <Button
              variant="ghost"
              icon={Trash2}
              busy={remove.isPending}
              onClick={() => {
                if (window.confirm(t("bimi.certificate.removeConfirm"))) remove.mutate(undefined);
              }}
            >
              {t("bimi.certificate.remove")}
            </Button>
          </div>
          {replacing && form}
        </>
      ) : (
        form
      )}
    </div>
  );
}

/** A domain's BIMI: the logo mail apps show next to its mail, the record for it, and what else it needs. */
export function BimiCard({ domain }: { domain: string }) {
  const { t } = useT();
  const query = useQuery({ queryKey: bimiKey(domain), queryFn: () => api<BimiView>(bimiPath(domain)) });
  const toggle = useBimiChange(domain, (enabled: boolean) =>
    api<BimiView>(bimiPath(domain), { method: "PUT", body: { enabled } }),
  );

  const content = () => {
    if (query.isPending) return <Loading />;
    if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
    const view = query.data;
    return (
      <div className="flex flex-col gap-4">
        <p className="-mt-1 text-[13px] text-muted">{t("bimi.intro", { domain })}</p>
        <p className="flex gap-2 rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
          <Info className="mt-0.5 size-4 shrink-0" aria-hidden />
          {t("bimi.support")}
        </p>
        <Toggle
          checked={view.enabled}
          disabled={toggle.isPending || (!view.hasSvg && !view.enabled)}
          onChange={(on) =>
            toggle.mutate(on, {
              onSuccess: () => toast(t(on ? "bimi.enabledToast" : "bimi.disabledToast"), "success"),
            })
          }
          label={t("bimi.enable")}
          description={view.hasSvg ? t("bimi.enableHint") : t("bimi.enableNeedsSvg")}
        />
        {view.enabled && (
          <div>
            <KeyValue label={t("bimi.logoUrl")} value={view.logoUrl} copy={view.logoUrl} />
          </div>
        )}
        <div className="border-t border-hairline pt-4">
          <LogoForm key={`${view.svgUpdatedAt ?? 0}-${view.title}`} domain={domain} view={view} />
        </div>
        <div className="border-t border-hairline pt-4">
          <RecordSection domain={domain} view={view} />
        </div>
        <div className="border-t border-hairline pt-4">
          <DmarcSection view={view} />
        </div>
        <div className="border-t border-hairline pt-4">
          <CertificateSection domain={domain} view={view} />
        </div>
      </div>
    );
  };

  return <Card title={t("bimi.title")}>{content()}</Card>;
}

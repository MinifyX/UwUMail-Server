import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Info } from "lucide-react";
import { useMemo, useRef, useState, type FormEvent } from "react";
import { Button } from "@/components/ui/Button";
import { Field, Select } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";
import {
  ALL_DOMAINS,
  EMPTY_SIGNATURE,
  PLACEHOLDERS,
  domainChange,
  editorStart,
  identitiesOf,
  previewFor,
  replacedByAllDomains,
  sameSignature,
  tooLarge,
  type IdentitySignatureInfo,
  type SignatureChange,
  type SignatureOverview,
  type SignatureText,
} from "./signatures";

export const signaturesKey = ["account", "signatures"] as const;

const textareaClass =
  "w-full rounded-control border border-line bg-surface px-3.5 py-2.5 text-sm focus:border-pink focus:shadow-focus focus:outline-none";

function useSaveSignatures() {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (change: SignatureChange) =>
      api<SignatureOverview>("/api/account/signatures", { method: "PUT", body: change }),
    onSuccess: (overview) => {
      queryClient.setQueryData(signaturesKey, overview);
      void queryClient.invalidateQueries({ queryKey: ["account", "identities"] });
      toast(t("mailbox.signatures.saved"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
}

/** Text and HTML of one signature, with buttons that insert the placeholders. */
function SignatureEditor({
  value,
  onChange,
  idPrefix,
}: {
  value: SignatureText;
  onChange: (value: SignatureText) => void;
  idPrefix: string;
}) {
  const { t } = useT();
  const textRef = useRef<HTMLTextAreaElement>(null);
  const insert = (placeholder: string) => {
    const area = textRef.current;
    const start = area?.selectionStart ?? value.text.length;
    const end = area?.selectionEnd ?? value.text.length;
    onChange({ ...value, text: value.text.slice(0, start) + placeholder + value.text.slice(end) });
    requestAnimationFrame(() => {
      area?.focus();
      area?.setSelectionRange(start + placeholder.length, start + placeholder.length);
    });
  };
  return (
    <div className="flex flex-col gap-3">
      <Field label={t("mailbox.signatures.text")}>
        {(id) => (
          <textarea
            id={id}
            ref={textRef}
            rows={4}
            data-testid={`${idPrefix}-text`}
            className={textareaClass}
            value={value.text}
            onChange={(event) => onChange({ ...value, text: event.target.value })}
          />
        )}
      </Field>
      <div className="flex flex-wrap items-center gap-2 text-[12px] text-muted">
        <span>{t("mailbox.signatures.placeholders")}</span>
        {PLACEHOLDERS.map((placeholder) => (
          <button
            key={placeholder}
            type="button"
            className="rounded-full border border-line px-2 py-0.5 font-mono hover:border-pink"
            onClick={() => insert(placeholder)}
          >
            {placeholder}
          </button>
        ))}
      </div>
      <Field label={t("mailbox.signatures.html")} hint={t("mailbox.signatures.htmlHint")}>
        {(id) => (
          <textarea
            id={id}
            rows={3}
            spellCheck={false}
            data-testid={`${idPrefix}-html`}
            className={`${textareaClass} font-mono text-[13px]`}
            value={value.html}
            onChange={(event) => onChange({ ...value, html: event.target.value })}
          />
        )}
      </Field>
    </div>
  );
}

/** One address that may have a signature of its own instead of its domain's. */
function IdentityOverride({
  identity,
  domainSignature,
}: {
  identity: IdentitySignatureInfo;
  domainSignature: SignatureText;
}) {
  const { t } = useT();
  const save = useSaveSignatures();
  const [editing, setEditing] = useState(identity.signature !== null);
  const [value, setValue] = useState<SignatureText>(identity.signature ?? domainSignature);
  const own = identity.signature !== null;
  const dirty = !own || !sameSignature(value, identity.signature);
  const big = tooLarge(value);

  return (
    <li className="flex flex-col gap-3 py-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="min-w-0 truncate text-sm font-semibold">
          {identity.name ? `${identity.name} <${identity.email}>` : identity.email}
        </p>
        <span className="text-[12px] text-muted">
          {own ? t("mailbox.signatures.ownSignature") : t("mailbox.signatures.followsDomain")}
        </span>
      </div>
      {!editing ? (
        <div>
          <Button size="sm" onClick={() => setEditing(true)}>
            {t("mailbox.signatures.override")}
          </Button>
        </div>
      ) : (
        <form
          className="flex flex-col gap-3"
          onSubmit={(event: FormEvent) => {
            event.preventDefault();
            save.mutate({ identities: { [String(identity.id)]: value } });
          }}
        >
          <SignatureEditor value={value} onChange={setValue} idPrefix={`identity-${identity.id}`} />
          {big && <p className="text-[13px] text-danger">{t("mailbox.signatures.tooLarge")}</p>}
          <div className="flex flex-wrap justify-end gap-2">
            <Button
              size="sm"
              variant="ghost"
              onClick={() => {
                if (own) save.mutate({ identities: { [String(identity.id)]: null } });
                setEditing(false);
                setValue(domainSignature);
              }}
            >
              {t("mailbox.signatures.backToDomain")}
            </Button>
            <Button type="submit" size="sm" variant="primary" busy={save.isPending} disabled={!dirty || big}>
              {t("mailbox.signatures.save")}
            </Button>
          </div>
        </form>
      )}
    </li>
  );
}

/** The signature of one domain: the editor, where it applies, a preview and the exceptions. */
function DomainEditor({ overview, domain }: { overview: SignatureOverview; domain: string }) {
  const { t } = useT();
  const save = useSaveSignatures();
  const start = useMemo(() => editorStart(overview, domain), [overview, domain]);
  const [value, setValue] = useState<SignatureText>(start.signature);
  const [targets, setTargets] = useState<string[]>(start.targets);

  const info = overview.domains.find((entry) => entry.domain === domain);
  const company = info?.company ?? null;
  const identities = identitiesOf(overview, domain);
  const all = targets.includes(ALL_DOMAINS);
  const replaced = all ? replacedByAllDomains(overview, domain) : [];
  const dirty =
    !sameSignature(value, start.signature) || start.origin === "template" || targets.join() !== start.targets.join();
  const big = tooLarge(value);
  const exceptions = identities.filter((identity) => identity.signature !== null).length;
  const sample = identities[0];

  const toggleTarget = (target: string, on: boolean) => {
    setTargets((current) => {
      if (target === ALL_DOMAINS) return on ? [ALL_DOMAINS] : [domain];
      const next = on ? [...current, target] : current.filter((entry) => entry !== target);
      return next.length > 0 ? next : [domain];
    });
  };

  return (
    <div className="flex flex-col gap-4">
      {start.origin === "template" && (
        <p className="flex gap-2 rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
          <Info className="mt-0.5 size-4 shrink-0" aria-hidden />
          {t("mailbox.signatures.fromTemplate")}
        </p>
      )}
      {start.origin === "allDomains" && (
        <p className="flex gap-2 rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
          <Info className="mt-0.5 size-4 shrink-0" aria-hidden />
          {t("mailbox.signatures.fromAllDomains")}
        </p>
      )}
      <form
        className="flex flex-col gap-4"
        onSubmit={(event: FormEvent) => {
          event.preventDefault();
          save.mutate(domainChange(overview, targets, value));
        }}
      >
        <SignatureEditor value={value} onChange={setValue} idPrefix="domain" />
        <fieldset className="flex flex-col gap-2">
          <legend className="mb-1 text-[13px] font-semibold text-muted">{t("mailbox.signatures.appliesTo")}</legend>
          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={all}
              onChange={(event) => toggleTarget(ALL_DOMAINS, event.target.checked)}
            />
            {t("mailbox.signatures.allDomains")}
          </label>
          {!all &&
            overview.domains.map((entry) => (
              <label key={entry.domain} className="flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={targets.includes(entry.domain)}
                  onChange={(event) => toggleTarget(entry.domain, event.target.checked)}
                />
                {t("mailbox.signatures.domainOption", { domain: entry.domain, count: entry.addressCount })}
              </label>
            ))}
          {replaced.length > 0 && (
            <p className="text-[12px] text-muted">
              {t("mailbox.signatures.replaces", { domains: replaced.join(", ") })}
            </p>
          )}
        </fieldset>
        {sample && (value.text.trim() || value.html.trim()) && (
          <div className="flex flex-col gap-1">
            <p className="text-[13px] font-semibold text-muted">
              {t("mailbox.signatures.preview", { address: sample.email })}
            </p>
            <pre className="rounded-control bg-canvas px-3 py-2 font-sans text-[13px] whitespace-pre-wrap">
              {previewFor(value, sample).text || value.html}
            </pre>
          </div>
        )}
        {big && <p className="text-[13px] text-danger">{t("mailbox.signatures.tooLarge")}</p>}
        <div className="flex flex-wrap justify-end gap-2">
          {(start.origin === "domain" || start.origin === "allDomains") && (
            <Button
              size="sm"
              variant="ghost"
              busy={save.isPending}
              onClick={() => save.mutate(domainChange(overview, start.targets, null))}
            >
              {t("mailbox.signatures.remove")}
            </Button>
          )}
          <Button type="submit" size="sm" variant="primary" busy={save.isPending} disabled={!dirty || big}>
            {t("mailbox.signatures.save")}
          </Button>
        </div>
      </form>
      {company?.mode === "footer" && (
        <div className="flex flex-col gap-1 rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
          <p className="flex gap-2">
            <Info className="mt-0.5 size-4 shrink-0" aria-hidden />
            {t("mailbox.signatures.companyFooter")}
          </p>
          <pre className="font-sans whitespace-pre-wrap">
            {sample ? previewFor({ text: company.text, html: "" }, sample).text : company.text}
          </pre>
        </div>
      )}
      {identities.length > 0 && (
        <details className="rounded-control border border-hairline px-4 py-2" open={exceptions > 0}>
          <summary className="cursor-pointer py-1 text-sm font-semibold">
            {t("mailbox.signatures.exceptions", { count: exceptions })}
          </summary>
          <ul className="flex flex-col divide-y divide-hairline">
            {identities.map((identity) => (
              <IdentityOverride
                key={`${identity.id}:${identity.signature?.text ?? ""}:${identity.signature?.html ?? ""}`}
                identity={identity}
                domainSignature={value.text || value.html ? value : EMPTY_SIGNATURE}
              />
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}

/** Signatures per domain: pick a domain, write one signature for all its addresses. */
export function DomainSignatures() {
  const { t } = useT();
  const overview = useQuery({
    queryKey: signaturesKey,
    queryFn: () => api<SignatureOverview>("/api/account/signatures"),
  });
  const [chosen, setChosen] = useState<string | null>(null);
  const domains = overview.data?.domains ?? [];
  const domain = chosen && domains.some((entry) => entry.domain === chosen) ? chosen : (domains[0]?.domain ?? null);

  if (overview.isPending) return <p className="text-sm text-muted">{t("mailbox.signatures.loading")}</p>;
  if (overview.isError) return <p className="text-sm text-danger">{t("mailbox.signatures.loadError")}</p>;
  if (!domain) return <p className="text-sm text-muted">{t("mailbox.signatures.empty")}</p>;

  return (
    <div className="flex flex-col gap-4">
      <Field label={t("mailbox.signatures.domain")}>
        {(id) => (
          <Select id={id} value={domain} onChange={(event) => setChosen(event.target.value)}>
            {domains.map((entry) => (
              <option key={entry.domain} value={entry.domain}>
                {t("mailbox.signatures.domainOption", { domain: entry.domain, count: entry.addressCount })}
              </option>
            ))}
          </Select>
        )}
      </Field>
      <DomainEditor key={`${domain}:${overview.data.state}`} overview={overview.data} domain={domain} />
    </div>
  );
}

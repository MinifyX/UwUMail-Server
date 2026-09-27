import { ArrowLeftRight, EyeOff } from "lucide-react";
import { useState, type FormEvent } from "react";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Field, Select, Toggle } from "@/components/ui/Field";
import { useT } from "@/i18n";
import type { DomainDetail, DomainMaskedPolicy, KindBlockers, MaskedMode } from "@/lib/api";
import { useSetDomainKind, useSetMaskedPolicy } from "./queries";

export const MASKED_MODES: MaskedMode[] = ["off", "own", "dedicated", "both"];

const usesOwn = (mode: MaskedMode) => mode === "own" || mode === "both";
export const usesDedicated = (mode: MaskedMode) => mode === "dedicated" || mode === "both";

/** The domains a policy allows, sorted by name, as the server works them out. */
export function allowedDomains(mode: MaskedMode, own: string | null, maskedDomains: string[]): string[] {
  const domains = new Set<string>();
  if (usesOwn(mode) && own) domains.add(own);
  if (usesDedicated(mode)) maskedDomains.forEach((name) => domains.add(name));
  return [...domains].sort();
}

/** Says a domain is only for masked addresses. */
export function MaskedOnlyPill() {
  const { t } = useT();
  return (
    <span className="inline-flex shrink-0 items-center gap-1 rounded-full bg-pink-tint px-2 py-0.5 text-[12px] font-semibold whitespace-nowrap text-pink-ink">
      <EyeOff className="size-3.5" aria-hidden />
      {t("maskedDomains.pill")}
    </span>
  );
}

/** Toggles for the masked-only domains a policy names. */
export function MaskedDomainToggles({
  choices,
  chosen,
  onChange,
}: {
  choices: string[];
  chosen: string[];
  onChange: (chosen: string[]) => void;
}) {
  const { t } = useT();
  if (choices.length === 0) {
    return (
      <p className="rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
        {t("maskedDomains.policy.noChoices")}
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-3">
      {choices.map((name) => (
        <Toggle
          key={name}
          checked={chosen.includes(name)}
          onChange={(on) => onChange(on ? [...chosen, name].sort() : chosen.filter((entry) => entry !== name))}
          label={name}
        />
      ))}
    </div>
  );
}

function blockerLines(blockers: KindBlockers, t: ReturnType<typeof useT>["t"]): string[] {
  const lines: string[] = [];
  if (blockers.accounts > 0) lines.push(t("maskedDomains.convert.blockers.accounts", { count: blockers.accounts }));
  if (blockers.aliases > 0) lines.push(t("maskedDomains.convert.blockers.aliases", { count: blockers.aliases }));
  if (blockers.groups > 0) lines.push(t("maskedDomains.convert.blockers.groups", { count: blockers.groups }));
  if (blockers.forwards > 0) lines.push(t("maskedDomains.convert.blockers.forwards", { count: blockers.forwards }));
  if (blockers.catchAll) lines.push(t("maskedDomains.convert.blockers.catchAll"));
  if (blockers.sendAs > 0) lines.push(t("maskedDomains.convert.blockers.sendAs", { count: blockers.sendAs }));
  return lines;
}

/** Turns a mail domain into one only for masked addresses, or says what is in the way. */
function ConvertToMasked({ domain }: { domain: DomainDetail }) {
  const { t } = useT();
  const convert = useSetDomainKind(domain.name, t("maskedDomains.convert.done", { domain: domain.name }));
  const blockers = domain.kindBlockers ? blockerLines(domain.kindBlockers, t) : [];
  return (
    <div className="mt-4 flex flex-col items-start gap-2 border-t border-hairline pt-4">
      <h3 className="text-sm font-semibold">{t("maskedDomains.convert.title")}</h3>
      <p className="text-[13px] text-muted">{t("maskedDomains.convert.hint", { domain: domain.name })}</p>
      {blockers.length > 0 && (
        <div className="w-full rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
          <p className="font-semibold text-ink">{t("maskedDomains.convert.blocked")}</p>
          <ul className="mt-1 list-disc pl-5">
            {blockers.map((line) => (
              <li key={line}>{line}</li>
            ))}
          </ul>
        </div>
      )}
      <Button
        icon={ArrowLeftRight}
        disabled={blockers.length > 0}
        busy={convert.isPending}
        onClick={() => {
          if (window.confirm(t("maskedDomains.convert.confirm", { domain: domain.name }))) convert.mutate("masked");
        }}
      >
        {t("maskedDomains.convert.action")}
      </Button>
    </div>
  );
}

/** Where the people of a mail domain may make masked addresses. */
function PolicyForm({ domain, policy }: { domain: DomainDetail; policy: DomainMaskedPolicy }) {
  const { t } = useT();
  const save = useSetMaskedPolicy(domain.name, t("maskedDomains.policy.saved"));
  const [mode, setMode] = useState(policy.mode);
  const [chosen, setChosen] = useState(policy.maskedDomains);
  const [fallback, setFallback] = useState(policy.defaultDomain ?? "");
  const allowed = allowedDomains(mode, domain.name, chosen);
  // A default that is no longer allowed goes back to automatic, as the server would do anyway.
  const defaultDomain = allowed.includes(fallback) ? fallback : "";
  const changed =
    mode !== policy.mode ||
    chosen.join() !== policy.maskedDomains.join() ||
    (defaultDomain || null) !== policy.defaultDomain;

  const submit = (event: FormEvent) => {
    event.preventDefault();
    save.mutate({ mode, maskedDomains: chosen, defaultDomain: defaultDomain || null });
  };

  return (
    <form className="flex flex-col gap-4" onSubmit={submit}>
      <p className="text-[13px] text-muted">{t("maskedDomains.policy.intro", { domain: domain.name })}</p>
      <Field label={t("maskedDomains.policy.mode")}>
        {(id) => (
          <Select id={id} value={mode} onChange={(event) => setMode(event.target.value as MaskedMode)}>
            {MASKED_MODES.map((value) => (
              <option key={value} value={value}>
                {t(`maskedDomains.modes.${value}`)}
              </option>
            ))}
          </Select>
        )}
      </Field>
      {usesDedicated(mode) && (
        <div className="flex flex-col gap-2">
          <div>
            <h3 className="text-sm font-semibold">{t("maskedDomains.policy.domains")}</h3>
            <p className="text-[13px] text-muted">{t("maskedDomains.policy.domainsHint")}</p>
          </div>
          <MaskedDomainToggles choices={domain.maskedDomainChoices} chosen={chosen} onChange={setChosen} />
        </div>
      )}
      {allowed.length > 1 && (
        <Field label={t("maskedDomains.policy.default")} hint={t("maskedDomains.policy.defaultHint")}>
          {(id) => (
            <Select id={id} value={defaultDomain} onChange={(event) => setFallback(event.target.value)}>
              <option value="">{t("maskedDomains.policy.automatic")}</option>
              {allowed.map((name) => (
                <option key={name} value={name}>
                  {name}
                </option>
              ))}
            </Select>
          )}
        </Field>
      )}
      <p className="text-[12px] text-muted">{t("maskedDomains.policy.existing")}</p>
      {changed && (
        <Button type="submit" variant="primary" className="self-start" busy={save.isPending}>
          {t("common.save")}
        </Button>
      )}
    </form>
  );
}

/** A mail domain: its masked address policy, and turning it masked-only. */
export function MaskedPolicyCard({ domain }: { domain: DomainDetail }) {
  const { t } = useT();
  if (!domain.maskedPolicy) return null;
  const policy = domain.maskedPolicy;
  return (
    <Card title={t("maskedDomains.policy.title")}>
      {/* A fresh form whenever the saved policy changes, e.g. after a masked domain went away. */}
      <PolicyForm key={JSON.stringify(policy)} domain={domain} policy={policy} />
      <ConvertToMasked domain={domain} />
    </Card>
  );
}

/** A masked-only domain: what it is, who may use it, and turning it back into a mail domain. */
export function MaskedOnlyCard({ domain }: { domain: DomainDetail }) {
  const { t } = useT();
  const back = useSetDomainKind(domain.name, t("maskedDomains.only.backDone", { domain: domain.name }));
  const usedBy = domain.maskedUsedBy ?? { domains: [], accounts: [] };
  const names = [...usedBy.domains, ...usedBy.accounts];
  return (
    <Card title={t("maskedDomains.only.title")}>
      <div className="flex flex-col items-start gap-3">
        <p className="text-[13px] text-muted">{t("maskedDomains.only.body")}</p>
        <p className="text-sm font-semibold">{t("maskedDomains.only.count", { count: domain.maskedInUse ?? 0 })}</p>
        {names.length === 0 ? (
          <p className="rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">{t("maskedDomains.only.unused")}</p>
        ) : (
          <div className="text-[13px] text-muted">
            {usedBy.domains.length > 0 && (
              <p>{t("maskedDomains.only.usedByDomains", { domains: usedBy.domains.join(", ") })}</p>
            )}
            {usedBy.accounts.length > 0 && (
              <p>{t("maskedDomains.only.usedByAccounts", { accounts: usedBy.accounts.join(", ") })}</p>
            )}
          </div>
        )}
        <div className="mt-1 flex w-full flex-col items-start gap-2 border-t border-hairline pt-4">
          <p className="text-[13px] text-muted">{t("maskedDomains.only.backHint")}</p>
          <Button
            icon={ArrowLeftRight}
            busy={back.isPending}
            onClick={() => {
              const text = [
                t("maskedDomains.only.backConfirm", { domain: domain.name }),
                names.length > 0 ? t("maskedDomains.only.backConfirmUsed", { names: names.join(", ") }) : "",
              ].join(" ");
              if (window.confirm(text.trim())) back.mutate("mail");
            }}
          >
            {t("maskedDomains.only.back")}
          </Button>
        </div>
      </div>
    </Card>
  );
}

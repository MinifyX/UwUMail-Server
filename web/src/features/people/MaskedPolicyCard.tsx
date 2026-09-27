import { useState, type FormEvent } from "react";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Field, Select } from "@/components/ui/Field";
import { useT } from "@/i18n";
import type { MaskedMode, Person, PersonMaskedPolicy } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";
import { MASKED_MODES, MaskedDomainToggles, allowedDomains, usesDedicated } from "@/features/domains/MaskedCards";
import { useSetPersonMaskedPolicy } from "./queries";

/** "" in a select means "as the domain". */
const AS_DOMAIN = "";

function PolicyForm({ login, policy }: { login: string; policy: PersonMaskedPolicy }) {
  const { t } = useT();
  const errorText = useErrorText();
  const save = useSetPersonMaskedPolicy(login);
  const own = login.slice(login.lastIndexOf("@") + 1);
  const domain = policy.domain ?? { mode: "off" as MaskedMode, maskedDomains: [], defaultDomain: null };
  const [mode, setMode] = useState<MaskedMode | typeof AS_DOMAIN>(policy.custom.mode ?? AS_DOMAIN);
  const [custom, setCustom] = useState<string[] | null>(policy.custom.maskedDomains);
  const [fallback, setFallback] = useState(policy.custom.defaultDomain ?? AS_DOMAIN);

  // What the choices here come to, worked out the way the server does it.
  const effectiveMode = mode || domain.mode;
  const effectiveList = custom ?? domain.maskedDomains;
  const allowed = allowedDomains(effectiveMode, policy.domain ? own : null, effectiveList);
  const defaultDomain = allowed.includes(fallback) ? fallback : AS_DOMAIN;
  const changed =
    (mode || null) !== policy.custom.mode ||
    JSON.stringify(custom) !== JSON.stringify(policy.custom.maskedDomains) ||
    (defaultDomain || null) !== policy.custom.defaultDomain;
  const stored = defaultDomain || domain.defaultDomain;
  const result = stored && allowed.includes(stored) ? stored : allowed.includes(own) ? own : (allowed[0] ?? null);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    save.mutate(
      { mode: mode || null, maskedDomains: custom, defaultDomain: defaultDomain || null },
      {
        onSuccess: () => toast(t("people.toasts.saved"), "success"),
        onError: (error) => toast(errorText(error), "error"),
      },
    );
  };

  return (
    <form className="flex flex-col gap-4" onSubmit={submit}>
      <p className="text-[13px] text-muted">{t("maskedDomains.person.intro")}</p>
      <Field label={t("maskedDomains.policy.mode")}>
        {(id) => (
          <Select id={id} value={mode} onChange={(event) => setMode(event.target.value as MaskedMode | "")}>
            <option value={AS_DOMAIN}>
              {t("maskedDomains.person.asDomain", { value: t(`maskedDomains.modes.${domain.mode}`) })}
            </option>
            {MASKED_MODES.map((value) => (
              <option key={value} value={value}>
                {t(`maskedDomains.modes.${value}`)}
              </option>
            ))}
          </Select>
        )}
      </Field>
      {usesDedicated(effectiveMode) && (
        <div className="flex flex-col gap-2">
          <Field label={t("maskedDomains.policy.domains")} hint={t("maskedDomains.policy.domainsHint")}>
            {(id) => (
              <Select
                id={id}
                value={custom === null ? AS_DOMAIN : "custom"}
                onChange={(event) => setCustom(event.target.value === AS_DOMAIN ? null : [...domain.maskedDomains])}
              >
                <option value={AS_DOMAIN}>
                  {t("maskedDomains.person.asDomain", {
                    value: domain.maskedDomains.join(", ") || t("maskedDomains.person.none"),
                  })}
                </option>
                <option value="custom">{t("maskedDomains.person.custom")}</option>
              </Select>
            )}
          </Field>
          {custom !== null && <MaskedDomainToggles choices={policy.choices} chosen={custom} onChange={setCustom} />}
        </div>
      )}
      {allowed.length > 1 && (
        <Field label={t("maskedDomains.policy.default")} hint={t("maskedDomains.policy.defaultHint")}>
          {(id) => (
            <Select id={id} value={defaultDomain} onChange={(event) => setFallback(event.target.value)}>
              <option value={AS_DOMAIN}>
                {t("maskedDomains.person.asDomain", {
                  value: domain.defaultDomain ?? t("maskedDomains.policy.automatic"),
                })}
              </option>
              {allowed.map((name) => (
                <option key={name} value={name}>
                  {name}
                </option>
              ))}
            </Select>
          )}
        </Field>
      )}
      <p className="rounded-control bg-canvas px-3 py-2 text-[13px] text-muted">
        {allowed.length === 0
          ? t("maskedDomains.person.resultNone")
          : t("maskedDomains.person.result", { domains: allowed.join(", "), domain: result ?? "" })}
      </p>
      {changed && (
        <Button type="submit" variant="primary" className="self-start" busy={save.isPending}>
          {t("common.save")}
        </Button>
      )}
    </form>
  );
}

/** Admin → a person: where they may make masked addresses, part by part as their domain or their own. */
export function PersonMaskedPolicyCard({ person }: { person: Person }) {
  const { t } = useT();
  const policy = person.maskedPolicy;
  if (!policy) return null;
  return (
    <Card title={t("maskedDomains.person.title")}>
      <PolicyForm
        key={JSON.stringify(policy.custom) + JSON.stringify(policy.domain)}
        login={person.login}
        policy={policy}
      />
    </Card>
  );
}

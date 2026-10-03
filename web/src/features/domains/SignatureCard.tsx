import { useMutation, useQueryClient } from "@tanstack/react-query";
import { PenLine } from "lucide-react";
import { useState, type FormEvent } from "react";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Field, Select } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type DomainDetail } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";
import {
  PLACEHOLDERS,
  tooLarge,
  type CompanySignature,
  type CompanySignatureMode,
} from "@/features/mailbox/signatures";

const textareaClass =
  "w-full rounded-control border border-line bg-surface px-3.5 py-2.5 text-sm focus:border-pink focus:shadow-focus focus:outline-none";

const MODES: CompanySignatureMode[] = ["off", "template", "footer"];

/** The company signature of a domain: a template for its people, or a footer the server appends. */
export function SignatureCard({ domain }: { domain: DomainDetail }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const current: CompanySignature = domain.signature ?? { mode: "off", text: "", html: "" };
  const [value, setValue] = useState<CompanySignature>(current);
  const save = useMutation({
    mutationFn: (next: CompanySignature) =>
      api<CompanySignature>(`/api/admin/domains/${encodeURIComponent(domain.name)}/signature`, {
        method: "PUT",
        body: next,
      }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["admin", "domains", domain.name] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
      toast(t("domains.signature.saved"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const dirty = value.mode !== current.mode || value.text !== current.text || value.html !== current.html;
  const emptyFooter = value.mode === "footer" && !value.text.trim() && !value.html.trim();
  const big = tooLarge(value);

  return (
    <Card
      title={
        <span className="flex items-center gap-2">
          <PenLine className="size-4 text-muted" aria-hidden />
          {t("domains.signature.title")}
        </span>
      }
    >
      <form
        className="flex flex-col gap-4"
        onSubmit={(event: FormEvent) => {
          event.preventDefault();
          save.mutate(value);
        }}
      >
        <p className="-mt-1 text-[13px] text-muted">{t("domains.signature.explain")}</p>
        <Field label={t("domains.signature.mode")} hint={t(`domains.signature.modeHint.${value.mode}`)}>
          {(id) => (
            <Select
              id={id}
              value={value.mode}
              onChange={(event) => setValue({ ...value, mode: event.target.value as CompanySignatureMode })}
            >
              {MODES.map((mode) => (
                <option key={mode} value={mode}>
                  {t(`domains.signature.modes.${mode}`)}
                </option>
              ))}
            </Select>
          )}
        </Field>
        {value.mode !== "off" && (
          <>
            <Field
              label={t("domains.signature.text")}
              hint={t("domains.signature.placeholders", { placeholders: PLACEHOLDERS.join(" ") })}
            >
              {(id) => (
                <textarea
                  id={id}
                  rows={4}
                  className={textareaClass}
                  value={value.text}
                  onChange={(event) => setValue({ ...value, text: event.target.value })}
                />
              )}
            </Field>
            <Field label={t("domains.signature.html")} hint={t("domains.signature.htmlHint")}>
              {(id) => (
                <textarea
                  id={id}
                  rows={3}
                  spellCheck={false}
                  className={`${textareaClass} font-mono text-[13px]`}
                  value={value.html}
                  onChange={(event) => setValue({ ...value, html: event.target.value })}
                />
              )}
            </Field>
          </>
        )}
        {value.mode === "footer" && <p className="text-[12px] text-muted">{t("domains.signature.footerNote")}</p>}
        {emptyFooter && <p className="text-[13px] text-danger">{t("domains.signature.footerEmpty")}</p>}
        {big && <p className="text-[13px] text-danger">{t("mailbox.signatures.tooLarge")}</p>}
        <div className="flex justify-end">
          <Button
            type="submit"
            size="sm"
            variant="primary"
            busy={save.isPending}
            disabled={!dirty || emptyFooter || big}
          >
            {t("domains.signature.save")}
          </Button>
        </div>
      </form>
    </Card>
  );
}

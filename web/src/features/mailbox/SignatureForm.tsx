import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { Button } from "@/components/ui/Button";
import { Field } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type IdentityInfo } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";

const textareaClass =
  "w-full rounded-control border border-line bg-surface px-3.5 py-2.5 text-sm focus:border-pink focus:shadow-focus focus:outline-none";

/** The signature of one sending address: plain text, and optionally HTML for formatted mail. */
export function SignatureForm({ identity }: { identity: IdentityInfo }) {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [text, setText] = useState(identity.textSignature);
  const [html, setHtml] = useState(identity.htmlSignature);
  const dirty = text !== identity.textSignature || html !== identity.htmlSignature;
  const save = useMutation({
    mutationFn: () =>
      api<void>(`/api/account/identities/${identity.id}`, {
        method: "PATCH",
        body: { textSignature: text, htmlSignature: html },
      }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["account", "identities"] });
      toast(t("mailbox.signatures.saved"), "success");
    },
  });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    save.mutate();
  };

  return (
    <form className="flex flex-col gap-3 rounded-control border border-hairline p-4" onSubmit={submit}>
      <p className="truncate text-sm font-semibold">
        {identity.name ? `${identity.name} <${identity.email}>` : identity.email}
      </p>
      <Field label={t("mailbox.signatures.text")}>
        {(id) => (
          <textarea
            id={id}
            rows={4}
            className={textareaClass}
            value={text}
            onChange={(event) => setText(event.target.value)}
          />
        )}
      </Field>
      <Field
        label={t("mailbox.signatures.html")}
        hint={t("mailbox.signatures.htmlHint")}
        error={save.isError ? errorText(save.error) : undefined}
      >
        {(id) => (
          <textarea
            id={id}
            rows={3}
            spellCheck={false}
            className={`${textareaClass} font-mono text-[13px]`}
            value={html}
            onChange={(event) => setHtml(event.target.value)}
          />
        )}
      </Field>
      <div className="flex justify-end">
        <Button type="submit" variant="primary" size="sm" busy={save.isPending} disabled={!dirty}>
          {t("mailbox.signatures.save")}
        </Button>
      </div>
    </form>
  );
}

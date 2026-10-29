import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CircleCheck, ExternalLink, Link2, Unlink } from "lucide-react";
import { useState, type FormEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card, CopyButton } from "@/components/ui/Card";
import { Dialog } from "@/components/ui/Dialog";
import { Field, TextInput, Toggle } from "@/components/ui/Field";
import { Cancelled, usePasswordConfirmation } from "@/features/security/ConfirmPassword";
import { useT } from "@/i18n";
import { api, type UwuauthView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatRelative } from "@/lib/format";
import { toast } from "@/state/toasts";

const DOCS = "https://github.com/MinifyX/UwUMail-Server/blob/main/docs/uwuauth.md";
const uwuauthKey = ["admin", "uwuauth"] as const;

/** Server → Settings → UwUAuth: pair with UwUAuth for login and SCIM, or unpair. */
export function UwuauthPage() {
  const { t } = useT();
  const view = useQuery({ queryKey: uwuauthKey, queryFn: () => api<UwuauthView>("/api/admin/uwuauth") });
  if (view.isPending) return <Loading />;
  if (view.isError) return <LoadError error={view.error} onRetry={() => void view.refetch()} />;
  return (
    <div className="flex flex-col gap-5">
      {view.data.paired ? <Paired view={view.data} /> : <PairForm />}
      <Card title={t("uwuauth.addresses")}>
        <div className="flex flex-col gap-3">
          <Address label={t("uwuauth.scimUrl")} value={view.data.scimUrl} />
          <Address label={t("externalLogin.oidc.redirectUri")} value={view.data.redirectUri} />
          <a href={DOCS} target="_blank" rel="noreferrer" className="text-[13px] font-semibold text-pink-ink">
            {t("uwuauth.docs")}
          </a>
        </div>
      </Card>
    </div>
  );
}

function Address({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex flex-col gap-1 rounded-control bg-canvas px-3 py-2.5">
      <span className="text-[12px] font-semibold text-muted">{label}</span>
      <span className="flex items-center justify-between gap-2">
        <code className="min-w-0 font-mono text-[13px] break-all select-all">{value}</code>
        <CopyButton value={value} />
      </span>
    </div>
  );
}

function PairForm() {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [url, setUrl] = useState("");
  const [code, setCode] = useState("");
  const [privateNetwork, setPrivateNetwork] = useState(false);
  const { confirmed, dialog } = usePasswordConfirmation();
  // The whole text of the QR code carries the address: then the address field is not needed.
  const fromQr = code.includes("#pair=");
  const pair = useMutation({
    // Pairing changes who may log in: an older login confirms with the password first.
    mutationFn: () =>
      confirmed((password) =>
        api<UwuauthView>("/api/admin/uwuauth", {
          method: "POST",
          body: { url: fromQr ? null : url.trim(), code: code.trim(), privateNetwork, password },
        }),
      ),
    onSuccess: (view) => {
      queryClient.setQueryData(uwuauthKey, view);
      void queryClient.invalidateQueries({ queryKey: ["admin", "settings"] });
      toast(t("uwuauth.paired"), "success");
    },
    onError: (error) => {
      if (!(error instanceof Cancelled)) toast(errorText(error), "error");
    },
  });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    pair.mutate();
  };
  return (
    <Card title={t("uwuauth.pairTitle")}>
      <form className="flex flex-col gap-4" onSubmit={submit}>
        <p className="text-[13px] text-muted">{t("uwuauth.pairIntro")}</p>
        <Field label={t("uwuauth.code")} hint={t("uwuauth.codeHint")}>
          {(id) => (
            <TextInput
              id={id}
              value={code}
              onChange={(event) => setCode(event.target.value)}
              placeholder="7KQ4-M2XD-9HFT"
              autoComplete="off"
              spellCheck={false}
              required
            />
          )}
        </Field>
        {!fromQr && (
          <Field label={t("uwuauth.url")}>
            {(id) => (
              <TextInput
                id={id}
                type="url"
                value={url}
                onChange={(event) => setUrl(event.target.value)}
                placeholder="https://auth.example.com"
                required
              />
            )}
          </Field>
        )}
        <Toggle
          checked={privateNetwork}
          onChange={setPrivateNetwork}
          label={t("externalLogin.oidc.privateNetwork")}
          description={t("externalLogin.oidc.privateNetworkHint")}
        />
        <div>
          <Button type="submit" variant="primary" icon={Link2} busy={pair.isPending}>
            {t("uwuauth.pair")}
          </Button>
        </div>
      </form>
      {dialog}
    </Card>
  );
}

function Paired({ view }: { view: UwuauthView }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const [asking, setAsking] = useState(false);
  const unpair = useMutation({
    mutationFn: () => api<void>("/api/admin/uwuauth", { method: "DELETE" }),
    onSuccess: () => {
      setAsking(false);
      void queryClient.invalidateQueries({ queryKey: uwuauthKey });
      void queryClient.invalidateQueries({ queryKey: ["admin", "settings"] });
      toast(t("uwuauth.unpaired"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const pairing = view.pairing!;
  return (
    <Card
      title={t("uwuauth.pairedTitle")}
      action={
        <Button variant="danger" size="sm" icon={Unlink} onClick={() => setAsking(true)}>
          {t("uwuauth.unpair")}
        </Button>
      }
    >
      <div className="flex flex-col gap-3 text-sm">
        <span className="inline-flex items-center gap-1.5 font-semibold text-success">
          <CircleCheck className="size-4" aria-hidden />
          {t("uwuauth.pairedWith", { name: pairing.name || pairing.issuer, version: pairing.version })}
        </span>
        <dl className="grid gap-x-4 gap-y-1.5 sm:grid-cols-[max-content_1fr]">
          <dt className="text-muted">{t("uwuauth.issuer")}</dt>
          <dd className="font-mono text-[13px] break-all">{pairing.issuer}</dd>
          <dt className="text-muted">{t("uwuauth.pairedAt")}</dt>
          <dd>
            {t("uwuauth.pairedBy", { when: formatRelative(pairing.pairedAt, i18n.language), who: pairing.pairedBy })}
          </dd>
          <dt className="text-muted">{t("uwuauth.login")}</dt>
          <dd>{view.loginActive ? t("uwuauth.loginActive") : t("uwuauth.loginInactive")}</dd>
        </dl>
        {view.locked.length > 0 && <p className="text-[13px] text-warning">{t("uwuauth.locked")}</p>}
        {pairing.manageUrl && (
          <a
            href={pairing.manageUrl}
            target="_blank"
            rel="noreferrer"
            className="inline-flex items-center gap-1 text-[13px] font-semibold text-pink-ink"
          >
            <ExternalLink className="size-3.5" aria-hidden />
            {t("uwuauth.manage")}
          </a>
        )}
      </div>
      <Dialog open={asking} onClose={() => setAsking(false)} title={t("uwuauth.unpairTitle")} width="sm">
        <div className="flex flex-col gap-4">
          <p className="text-sm">{t("uwuauth.unpairBody")}</p>
          <div className="flex justify-end gap-2">
            <Button onClick={() => setAsking(false)}>{t("common.cancel")}</Button>
            <Button variant="danger" icon={Unlink} busy={unpair.isPending} onClick={() => unpair.mutate()}>
              {t("uwuauth.unpair")}
            </Button>
          </div>
        </div>
      </Dialog>
    </Card>
  );
}

import { AppWindow, LogOut } from "lucide-react";
import { useState } from "react";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Dialog } from "@/components/ui/Dialog";
import { useT } from "@/i18n";
import { api, type OAuthGrantInfo, type SecurityView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDate, formatRelative } from "@/lib/format";
import { toast } from "@/state/toasts";
import { useSecurityAction } from "./queries";

/** The scopes worth a pill; who someone is (openid, profile, email) goes without saying. */
const SHOWN_SCOPES = ["mail", "smtp", "dav", "maskedemail"];

/** One app signed in with OAuth, in the look of an app password. */
export function OAuthGrantRow({ grant, onRevoke }: { grant: OAuthGrantInfo; onRevoke: () => void }) {
  const { t, i18n } = useT();
  const language = i18n.language;
  const used = grant.lastUsedAt
    ? t("security.appPasswords.lastUsed", {
        time: formatRelative(grant.lastUsedAt, language),
        protocol: (grant.lastUsedProtocol ?? "").toUpperCase(),
        ip: grant.lastUsedIp ?? "",
      })
    : t("security.appPasswords.neverUsed");
  return (
    <li className="flex items-start gap-3 border-b border-hairline py-3 last:border-b-0">
      <AppWindow className="mt-0.5 size-4 shrink-0 text-muted" aria-hidden />
      <span className="min-w-0 flex-1">
        <span className="flex flex-wrap items-center gap-1.5">
          <span className="truncate text-sm font-semibold">{grant.clientName}</span>
          {grant.scopes
            .filter((scope) => SHOWN_SCOPES.includes(scope))
            .map((scope) => (
              <span
                key={scope}
                className="rounded-full bg-pink-tint px-2 text-[11px] leading-5 font-semibold text-pink-ink"
              >
                {t(`security.appPasswords.scopeShort.${scope}`)}
              </span>
            ))}
          {grant.scopes.includes("offline_access") && (
            <span className="rounded-full bg-canvas px-2 text-[11px] leading-5 font-semibold text-muted">
              {t("security.oauthApps.staysSignedIn")}
            </span>
          )}
        </span>
        <span className="block text-[12px] text-muted">{used}</span>
        <span className="block text-[12px] text-faint">
          {t("security.oauthApps.since", { date: formatDate(grant.createdAt, language) })}
        </span>
      </span>
      <Button size="sm" variant="danger" icon={LogOut} onClick={onRevoke}>
        {t("security.oauthApps.signOut")}
      </Button>
    </li>
  );
}

/** Asks before an app is signed out; `busy` while the server does it. */
export function SignOutAppDialog({
  grant,
  busy,
  onClose,
  onConfirm,
}: {
  grant: OAuthGrantInfo | null;
  busy: boolean;
  onClose: () => void;
  onConfirm: (grant: OAuthGrantInfo) => void;
}) {
  const { t } = useT();
  return (
    <Dialog
      open={grant !== null}
      onClose={onClose}
      title={t("security.oauthApps.signOutTitle", { name: grant?.clientName ?? "" })}
      width="sm"
    >
      <div className="flex flex-col gap-4 px-6 pt-1 pb-6">
        <p className="text-sm text-muted">{t("security.oauthApps.signOutBody")}</p>
        <div className="flex justify-end gap-2">
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="danger" icon={LogOut} busy={busy} onClick={() => grant && onConfirm(grant)}>
            {t("security.oauthApps.signOut")}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

/** Apps that signed in in the browser (OAuth) instead of with an app password (docs/oauth.md). */
export function OAuthAppsCard({ security }: { security: SecurityView }) {
  const { t } = useT();
  const errorText = useErrorText();
  const [revoking, setRevoking] = useState<OAuthGrantInfo | null>(null);
  const revoke = useSecurityAction((id: number) => api<void>(`/api/account/oauth-grants/${id}`, { method: "DELETE" }));
  const grants = security.oauthGrants;

  return (
    <Card title={t("security.oauthApps.title")}>
      {grants.length === 0 ? (
        <p className="text-sm text-muted">{t("security.oauthApps.none")}</p>
      ) : (
        <ul className="flex flex-col">
          {grants.map((grant) => (
            <OAuthGrantRow key={grant.id} grant={grant} onRevoke={() => setRevoking(grant)} />
          ))}
        </ul>
      )}
      <SignOutAppDialog
        grant={revoking}
        busy={revoke.isPending}
        onClose={() => setRevoking(null)}
        onConfirm={(grant) =>
          revoke.mutate(grant.id, {
            onSuccess: () => {
              toast(t("security.oauthApps.signedOut", { name: grant.clientName }), "success");
              setRevoking(null);
            },
            onError: (error) => toast(errorText(error), "error"),
          })
        }
      />
    </Card>
  );
}

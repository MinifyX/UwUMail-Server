import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ExternalLink, RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/Button";
import { Card, CopyButton } from "@/components/ui/Card";
import { Segmented, Toggle } from "@/components/ui/Field";
import { LoadError, Loading } from "@/components/StatusViews";
import { useT } from "@/i18n";
import { api, type UpdateChannel, type UpdatesView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDateTime } from "@/lib/format";
import { toast } from "@/state/toasts";

const key = ["admin", "updates"] as const;

function Command({ label, command }: { label: string; command: string }) {
  return (
    <div className="flex flex-col gap-1.5">
      <p className="text-[13px] font-semibold text-muted">{label}</p>
      <div className="flex items-start gap-2 rounded-control bg-canvas px-3 py-2">
        <code className="flex-1 font-mono text-[12px] break-all">{command}</code>
        <CopyButton value={command} />
      </div>
    </div>
  );
}

/** The running version, what is newer on the chosen channel, and how to update. */
export function UpdatesCard() {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const query = useQuery({ queryKey: key, queryFn: () => api<UpdatesView>("/api/admin/updates") });
  const save = useMutation({
    mutationFn: (settings: UpdatesView["settings"]) =>
      api<UpdatesView>("/api/admin/updates", { method: "PUT", body: settings }),
    onSuccess: (next) => queryClient.setQueryData(key, next),
    onError: (error) => toast(errorText(error), "error"),
  });
  const check = useMutation({
    mutationFn: () => api<UpdatesView>("/api/admin/updates/check", { method: "POST", body: {} }),
    onSuccess: (next) => queryClient.setQueryData(key, next),
    onError: (error) => toast(errorText(error), "error"),
  });

  if (query.isPending) {
    return (
      <Card title={t("updates.title")}>
        <Loading />
      </Card>
    );
  }
  if (query.isError) {
    return (
      <Card title={t("updates.title")}>
        <LoadError error={query.error} onRetry={() => void query.refetch()} />
      </Card>
    );
  }
  const view = query.data;
  const { build, settings, info } = view;
  const newest = info.releases[0];
  const edge = !build.release;
  const behind = info.behind ?? 0;
  const available = edge ? behind > 0 : Boolean(newest);

  return (
    <Card title={t("updates.title")}>
      <div className="flex flex-col gap-4">
        <p className="text-sm">
          {t(edge ? "updates.runningEdge" : "updates.running", {
            version: build.version,
            commit: build.commit?.slice(0, 7) ?? "–",
          })}
        </p>

        {info.error ? (
          <p className="rounded-control bg-warning-tint px-3 py-2 text-[13px] text-warning">
            {t("updates.checkFailed", { error: info.error })}
          </p>
        ) : !info.checkedAt ? (
          <p className="text-[13px] text-muted">{t("updates.notChecked")}</p>
        ) : !available ? (
          <p className="text-[13px] text-muted">
            {t("updates.upToDate", { time: formatDateTime(info.checkedAt, i18n.language) })}
          </p>
        ) : edge ? (
          <div className="flex flex-col gap-2">
            <p className="text-sm font-semibold">{t("updates.edgeBehind", { count: behind })}</p>
            <ul className="flex flex-col gap-1 text-[13px]">
              {info.commits.map((commit) => (
                <li key={commit.sha} className="flex gap-2">
                  <code className="font-mono text-faint">{commit.sha}</code>
                  <span className="min-w-0 break-words">{commit.message}</span>
                </li>
              ))}
            </ul>
          </div>
        ) : (
          <div className="flex flex-col gap-3">
            {info.releases.map((release) => (
              <section key={release.version} className="flex flex-col gap-1">
                <h3 className="flex flex-wrap items-center gap-2 text-sm font-semibold">
                  {release.name}
                  {release.prerelease && (
                    <span className="rounded-full bg-pink-tint px-2 text-[11px] text-pink-ink">
                      {t("updates.beta")}
                    </span>
                  )}
                  <a
                    href={release.url}
                    target="_blank"
                    rel="noreferrer"
                    className="inline-flex items-center gap-1 text-[12px] font-normal text-pink-ink hover:underline"
                  >
                    {t("updates.onGitHub")}
                    <ExternalLink className="size-3" aria-hidden />
                  </a>
                </h3>
                <pre className="max-h-56 overflow-y-auto font-sans text-[13px] whitespace-pre-wrap text-muted">
                  {release.notes}
                </pre>
              </section>
            ))}
          </div>
        )}

        {available && (
          <div className="flex flex-col gap-3 border-t border-hairline pt-4">
            <p className="text-[13px] text-muted">{t("updates.howTo", { image: view.image })}</p>
            <Command label={t("updates.serverCommand")} command={view.serverCommand} />
            {view.gatewayCommand && <Command label={t("updates.gatewayCommand")} command={view.gatewayCommand} />}
          </div>
        )}
        {view.gateway?.software && (
          <p className="text-[12px] text-muted">{t("updates.gatewayRunning", { software: view.gateway.software })}</p>
        )}

        <div className="flex flex-col gap-3 border-t border-hairline pt-4">
          <Toggle
            checked={settings.check}
            onChange={(on) => save.mutate({ ...settings, check: on })}
            label={t("updates.check")}
            description={t("updates.checkHint")}
          />
          {!edge && (
            <Segmented<UpdateChannel>
              label={t("updates.channel")}
              value={settings.channel}
              onChange={(channel) => save.mutate({ ...settings, channel })}
              options={[
                { value: "stable", label: t("updates.stable") },
                { value: "beta", label: t("updates.beta") },
              ]}
            />
          )}
          <div>
            <Button variant="ghost" icon={RefreshCw} busy={check.isPending} onClick={() => check.mutate()}>
              {t("updates.checkNow")}
            </Button>
          </div>
        </div>
      </div>
    </Card>
  );
}

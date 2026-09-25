import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Folder, FolderInput, Share2, UserMinus } from "lucide-react";
import { useState, type FormEvent } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { Dialog } from "@/components/ui/Dialog";
import { Field, Select } from "@/components/ui/Field";
import { useT } from "@/i18n";
import { api, type ShareLevel, type SharingView } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";

const sharingKey = ["account", "sharing"] as const;
const LEVELS: ShareLevel[] = ["read", "write", "all"];

type Folder = SharingView["folders"][number];

/** Sharing one's folders with people on the server, and what others share (docs/sharing.md). */
export function SharingCard() {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const sharing = useQuery({ queryKey: sharingKey, queryFn: () => api<SharingView>("/api/account/sharing") });
  const [sharingFolder, setSharingFolder] = useState<Folder | null>(null);
  const [person, setPerson] = useState("");
  const [level, setLevel] = useState<ShareLevel>("read");

  const folderName = (folder: { path: string; role: Folder["role"] }) =>
    folder.role ? t(`storage.roles.${folder.role}`) : folder.path;
  const personName = (login: string) => {
    const found = sharing.data?.people.find((entry) => entry.login === login);
    return found?.name ? `${found.name} (${login})` : login;
  };

  const share = useMutation({
    mutationFn: (input: { folder: Folder; login: string; level: ShareLevel }) =>
      api<SharingView>(`/api/account/sharing/${input.folder.id}`, {
        method: "PUT",
        body: { login: input.login, level: input.level },
      }),
    onSuccess: (next, input) => {
      queryClient.setQueryData(sharingKey, next);
      setSharingFolder(null);
      toast(t("sharing.saved", { folder: folderName(input.folder), person: personName(input.login) }), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const unshare = useMutation({
    mutationFn: (input: { folder: Folder; login: string }) =>
      api<SharingView>(`/api/account/sharing/${input.folder.id}/${encodeURIComponent(input.login)}`, {
        method: "DELETE",
      }),
    onSuccess: (next, input) => {
      queryClient.setQueryData(sharingKey, next);
      toast(t("sharing.removed", { folder: folderName(input.folder), person: personName(input.login) }), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  if (sharing.isPending) return <Loading />;
  if (sharing.isError) return <LoadError error={sharing.error} onRetry={() => void sharing.refetch()} />;
  const data = sharing.data;

  const open = (folder: Folder) => {
    const taken = new Set(folder.shares.map((entry) => entry.login));
    setPerson(data.people.find((entry) => !taken.has(entry.login))?.login ?? "");
    setLevel("read");
    setSharingFolder(folder);
  };
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (sharingFolder && person) share.mutate({ folder: sharingFolder, login: person, level });
  };

  return (
    <Card title={t("sharing.title")}>
      <div className="flex flex-col gap-4">
        <p className="text-sm text-muted">{t("sharing.intro")}</p>
        {data.people.length === 0 && (
          <p className="rounded-control bg-canvas px-3 py-2.5 text-[13px] text-muted">{t("sharing.noPeople")}</p>
        )}
        <ul className="flex flex-col">
          {data.folders.map((folder) => (
            <li key={folder.id} className="flex flex-col gap-1.5 border-b border-hairline py-2 last:border-b-0">
              <div className="flex min-h-9 items-center gap-2 text-sm">
                <Folder className="size-4 shrink-0 text-muted" aria-hidden />
                <span className="min-w-0 flex-1">
                  <span className="block truncate font-semibold">{folderName(folder)}</span>
                  <span className="block text-[12px] text-muted">
                    {folder.shares.length === 0
                      ? t("sharing.none")
                      : t("sharing.sharedWith", { count: folder.shares.length })}
                  </span>
                </span>
                <Button size="sm" icon={Share2} disabled={data.people.length === 0} onClick={() => open(folder)}>
                  {t("sharing.share")}
                </Button>
              </div>
              {folder.shares.length > 0 && (
                <ul className="ml-6 flex flex-col gap-1">
                  {folder.shares.map((entry) => (
                    <li key={entry.login} className="flex flex-wrap items-center gap-2 text-[13px]">
                      <span className="min-w-0 flex-1 truncate">{personName(entry.login)}</span>
                      <Select
                        aria-label={t("sharing.level")}
                        className="w-44"
                        value={entry.level}
                        disabled={share.isPending}
                        onChange={(event) =>
                          share.mutate({ folder, login: entry.login, level: event.target.value as ShareLevel })
                        }
                      >
                        {LEVELS.map((option) => (
                          <option key={option} value={option}>
                            {t(`sharing.levels.${option}`)}
                          </option>
                        ))}
                      </Select>
                      <Button
                        size="sm"
                        variant="danger"
                        icon={UserMinus}
                        busy={
                          unshare.isPending &&
                          unshare.variables?.folder.id === folder.id &&
                          unshare.variables.login === entry.login
                        }
                        onClick={() => unshare.mutate({ folder, login: entry.login })}
                      >
                        {t("sharing.remove")}
                      </Button>
                    </li>
                  ))}
                </ul>
              )}
            </li>
          ))}
        </ul>

        <section className="flex flex-col gap-1.5 border-t border-hairline pt-3">
          <h3 className="text-[13px] font-bold">{t("sharing.withMeTitle")}</h3>
          {data.sharedWithMe.length === 0 ? (
            <p className="text-[13px] text-muted">{t("sharing.withMeEmpty")}</p>
          ) : (
            <>
              <p className="text-[12px] text-muted">{t("sharing.withMeHint")}</p>
              <ul className="flex flex-col">
                {data.sharedWithMe.map((entry) => (
                  <li key={`${entry.owner}-${entry.id}`} className="flex min-h-10 items-center gap-2 text-sm">
                    <FolderInput className="size-4 shrink-0 text-muted" aria-hidden />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate font-semibold">{folderName(entry)}</span>
                      <span className="block truncate text-[12px] text-muted">
                        {t("sharing.withMeFrom", {
                          owner: entry.ownerName ? `${entry.ownerName} (${entry.owner})` : entry.owner,
                        })}
                      </span>
                    </span>
                    <span className="text-[12px] text-muted">{t(`sharing.levels.${entry.level}`)}</span>
                  </li>
                ))}
              </ul>
            </>
          )}
        </section>
      </div>

      <Dialog
        open={sharingFolder !== null}
        onClose={() => setSharingFolder(null)}
        title={sharingFolder ? t("sharing.shareTitle", { folder: folderName(sharingFolder) }) : ""}
        width="sm"
      >
        <form className="flex flex-col gap-4 px-6 pt-1 pb-6" onSubmit={submit}>
          <Field label={t("sharing.person")}>
            {(id) => (
              <Select id={id} required value={person} onChange={(event) => setPerson(event.target.value)}>
                <option value="" disabled>
                  {t("sharing.personPlaceholder")}
                </option>
                {data.people.map((entry) => (
                  <option key={entry.login} value={entry.login}>
                    {entry.name ? `${entry.name} (${entry.login})` : entry.login}
                  </option>
                ))}
              </Select>
            )}
          </Field>
          <Field label={t("sharing.level")} hint={t(`sharing.levelHints.${level}`)}>
            {(id) => (
              <Select id={id} value={level} onChange={(event) => setLevel(event.target.value as ShareLevel)}>
                {LEVELS.map((option) => (
                  <option key={option} value={option}>
                    {t(`sharing.levels.${option}`)}
                  </option>
                ))}
              </Select>
            )}
          </Field>
          <div className="flex justify-end gap-2">
            <Button onClick={() => setSharingFolder(null)}>{t("common.cancel")}</Button>
            <Button type="submit" icon={Share2} busy={share.isPending} disabled={!person}>
              {t("sharing.share")}
            </Button>
          </div>
        </form>
      </Dialog>
    </Card>
  );
}

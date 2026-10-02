import { useMutation } from "@tanstack/react-query";
import { Download, KeyRound, Printer } from "lucide-react";
import { Button } from "@/components/ui/Button";
import { Card, CopyButton } from "@/components/ui/Card";
import { useT } from "@/i18n";
import { api, type MoveLinks, type MoveMailboxInfo } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDateTime } from "@/lib/format";
import { toast } from "@/state/toasts";
import { escapeHtml, linksCsv } from "./moves";

/** The table of links, also for printing. */
export function LinksTable({ links, origin }: { links: MoveLinks; origin: string }) {
  const { t, i18n } = useT();
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-left text-[13px]">
        <thead className="text-[12px] text-muted">
          <tr>
            <th scope="col" className="py-1 pr-3 font-semibold">
              {t("moves.links.mailbox")}
            </th>
            <th scope="col" className="py-1 pr-3 font-semibold">
              {t("moves.links.link")}
            </th>
            <th scope="col" className="py-1 font-semibold">
              {t("moves.links.expires")}
            </th>
          </tr>
        </thead>
        <tbody>
          {links.links.map((link) => {
            const url = `${origin}${link.path}`;
            return (
              <tr key={link.mailboxId} className="border-t border-hairline">
                <td className="py-1.5 pr-3">
                  <span className="font-semibold">{link.address}</span>
                  {link.name && <span className="block text-[12px] text-muted">{link.name}</span>}
                </td>
                <td className="py-1.5 pr-3">
                  <span className="flex items-center gap-1">
                    <code className="max-w-[22rem] truncate text-[12px]">{url}</code>
                    <CopyButton value={url} label={t("moves.links.copy", { address: link.address })} />
                  </span>
                </td>
                <td className="py-1.5 whitespace-nowrap text-muted">{formatDateTime(link.expiresAt, i18n.language)}</td>
              </tr>
            );
          })}
          {links.skipped.map((skipped) => (
            <tr key={skipped.mailboxId} className="border-t border-hairline text-muted">
              <td className="py-1.5 pr-3">{skipped.address}</td>
              <td className="py-1.5" colSpan={2}>
                {t(`moves.links.skipped.${skipped.reason}`)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/**
 * Links to choose a password, once the people have moved: one per mailbox the move made, shown
 * here once (the server keeps only their fingerprints), as a CSV file and as a page to print.
 * Making them again replaces the ones before.
 */
export function LinksCard({
  moveId,
  domain,
  mailboxes,
  links,
  onLinks,
}: {
  moveId: number;
  domain: string;
  mailboxes: MoveMailboxInfo[];
  links: MoveLinks | null;
  onLinks: (links: MoveLinks) => void;
}) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const origin = window.location.origin;
  const waiting = mailboxes.filter((mailbox) => !mailbox.hasPortalPassword).length;
  const make = useMutation({
    mutationFn: () => api<MoveLinks>(`/api/admin/moves/${moveId}/links`, { method: "POST", body: {} }),
    onSuccess: (made) => {
      onLinks(made);
      toast(t("moves.links.made", { count: made.links.length }), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const download = () => {
    if (!links) return;
    const headers = [
      t("moves.links.mailbox"),
      t("moves.links.name"),
      t("moves.links.oldAddress"),
      t("moves.links.link"),
      t("moves.links.expires"),
    ];
    const csv = linksCsv(links.links, origin, headers, (at) => formatDateTime(at, i18n.language));
    const url = URL.createObjectURL(new Blob([csv], { type: "text/csv;charset=utf-8" }));
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `${domain}-links.csv`;
    anchor.click();
    URL.revokeObjectURL(url);
  };
  const print = () => {
    if (!links) return;
    const page = window.open("", "_blank");
    if (!page) return;
    const rows = links.links
      .map(
        (link) =>
          `<tr><td><b>${escapeHtml(link.address)}</b><br>${escapeHtml(link.name)}</td><td><code>${escapeHtml(
            `${origin}${link.path}`,
          )}</code></td><td>${escapeHtml(formatDateTime(link.expiresAt, i18n.language))}</td></tr>`,
      )
      .join("");
    page.document.write(
      `<!doctype html><meta charset="utf-8"><title>${escapeHtml(t("moves.links.printTitle", { domain }))}</title>` +
        `<style>body{font:14px system-ui,sans-serif;margin:24px}table{border-collapse:collapse;width:100%}` +
        `td,th{border:1px solid #ccc;padding:6px 8px;text-align:left;vertical-align:top}code{word-break:break-all}</style>` +
        `<h1>${escapeHtml(t("moves.links.printTitle", { domain }))}</h1><p>${escapeHtml(t("moves.links.printIntro"))}</p>` +
        `<table><tr><th>${escapeHtml(t("moves.links.mailbox"))}</th><th>${escapeHtml(t("moves.links.link"))}</th>` +
        `<th>${escapeHtml(t("moves.links.expires"))}</th></tr>${rows}</table>`,
    );
    page.document.close();
    page.focus();
    page.print();
  };
  return (
    <Card title={t("moves.links.title")}>
      <div className="flex flex-col gap-3">
        <p className="text-[13px] text-muted">
          {waiting > 0 ? t("moves.links.explain", { count: waiting }) : t("moves.links.none")}
        </p>
        <div className="flex flex-wrap gap-2">
          <Button icon={KeyRound} busy={make.isPending} disabled={waiting === 0} onClick={() => make.mutate()}>
            {links ? t("moves.links.again") : t("moves.links.make")}
          </Button>
          {links && links.links.length > 0 && (
            <>
              <Button icon={Download} onClick={download}>
                {t("moves.links.csv")}
              </Button>
              <Button icon={Printer} onClick={print}>
                {t("moves.links.print")}
              </Button>
            </>
          )}
        </div>
        {links && (
          <>
            <p className="text-[12px] text-muted">{t("moves.links.once")}</p>
            <LinksTable links={links} origin={origin} />
          </>
        )}
      </div>
    </Card>
  );
}

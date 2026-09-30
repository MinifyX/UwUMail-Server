import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import clsx from "clsx";
import {
  BookOpen,
  Check,
  ChevronRight,
  CircleCheck,
  CircleHelp,
  CircleX,
  ExternalLink,
  ListChecks,
  RotateCw,
  TriangleAlert,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import { LoadError, Loading } from "@/components/StatusViews";
import { Button } from "@/components/ui/Button";
import { Card } from "@/components/ui/Card";
import { useT } from "@/i18n";
import {
  api,
  type MicrosoftAddress,
  type MicrosoftChecklist,
  type MicrosoftDomainCheck,
  type MicrosoftIssue,
  type MicrosoftIssues,
  type MsStatus,
} from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { formatDateTime, formatRelative } from "@/lib/format";
import { Link } from "@/lib/router";
import { toast } from "@/state/toasts";
import { actionOf, dmarcNote, isOpen, issueDomain, issueIp, ptrStatus, worstStatus } from "./microsoft";

export const SNDS_URL = "https://sendersupport.olc.protection.outlook.com/snds/";
export const DELIST_URL = "https://sender.office.com";

const ISSUES_KEY = ["admin", "microsoft", "issues"];
const CHECKLIST_KEY = ["admin", "microsoft", "checklist"];

export function useMicrosoftIssues() {
  return useQuery({
    queryKey: ISSUES_KEY,
    queryFn: () => api<MicrosoftIssues>("/api/admin/microsoft/issues"),
    refetchInterval: 60_000,
  });
}

function useChecklist() {
  return useQuery({
    queryKey: CHECKLIST_KEY,
    queryFn: () => api<MicrosoftChecklist>("/api/admin/microsoft/checklist"),
  });
}

const STATUS: Record<MsStatus, { icon: LucideIcon; className: string }> = {
  ok: { icon: CircleCheck, className: "bg-success-tint text-success" },
  warning: { icon: TriangleAlert, className: "bg-warning-tint text-warning" },
  problem: { icon: CircleX, className: "bg-danger-tint text-danger" },
  unknown: { icon: CircleHelp, className: "bg-elevated text-muted" },
};

/** A small coloured label: what is checked, and how it stands. */
function StatusChip({ status, label }: { status: MsStatus; label?: ReactNode }) {
  const { t } = useT();
  const { icon: Icon, className } = STATUS[status];
  return (
    <span
      className={clsx(
        "inline-flex h-6 shrink-0 items-center gap-1 rounded-full px-2.5 text-[12px] font-semibold",
        className,
      )}
    >
      <Icon className="size-3.5" aria-hidden />
      {label ?? t(`microsoft.status.${status}`)}
      {label && <span className="sr-only">: {t(`microsoft.status.${status}`)}</span>}
    </span>
  );
}

/** A link to one of Microsoft's pages, in a new tab. */
export function ExternalAnchor({ href, children }: { href: string; children: ReactNode }) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noopener noreferrer"
      className="inline-flex items-center gap-1 font-semibold text-pink-ink hover:underline"
    >
      {children}
      <ExternalLink className="size-3.5 shrink-0" aria-hidden />
    </a>
  );
}

function Steps({ children }: { children: ReactNode }) {
  return <ol className="flex list-decimal flex-col gap-1.5 pl-5 text-[13px] marker:font-semibold">{children}</ol>;
}

/** What to do about an issue, by what it means. */
function WhatToDo({ issue, delistUrl }: { issue: MicrosoftIssue; delistUrl: string }) {
  const { t } = useT();
  const ip = issueIp(issue) || t("microsoft.issues.theIp");
  const domain = issueDomain(issue);
  switch (actionOf(issue.group)) {
    case "delist":
      return (
        <div className="flex flex-col gap-2">
          <Steps>
            <li>
              {t("microsoft.delist.step1")}{" "}
              <ExternalAnchor href={delistUrl}>{delistUrl.replace(/^https:\/\//, "")}</ExternalAnchor>
            </li>
            <li>{t("microsoft.delist.step2", { ip })}</li>
            <li>{t("microsoft.delist.step3")}</li>
            <li>{t("microsoft.delist.step4")}</li>
          </Steps>
          {issue.group === "banned" && <p className="text-[13px] text-muted">{t("microsoft.delist.banned")}</p>}
        </div>
      );
    case "wait":
      return (
        <ul className="flex list-disc flex-col gap-1.5 pl-5 text-[13px]">
          <li>{t("microsoft.wait.retry")}</li>
          <li>{t("microsoft.wait.steady")}</li>
          <li>{t("microsoft.wait.snds")}</li>
          <li>{t("microsoft.wait.relay")}</li>
        </ul>
      );
    case "authenticate":
      return (
        <div className="flex flex-col gap-1.5 text-[13px]">
          <p>{domain ? t("microsoft.authenticate.text", { domain }) : t("microsoft.authenticate.textAny")}</p>
          {domain && (
            <Link
              to={`/admin/domains/${encodeURIComponent(domain)}`}
              className="inline-flex items-center gap-0.5 self-start font-semibold text-pink-ink hover:underline"
            >
              {t("microsoft.authenticate.open", { domain })}
              <ChevronRight className="size-3.5" aria-hidden />
            </Link>
          )}
        </div>
      );
  }
}

function IssueSubject({ issue }: { issue: MicrosoftIssue }) {
  const { t } = useT();
  if (!issue.subject) return <>{t("microsoft.issues.subjectUnknown")}</>;
  return (
    <>
      {issue.scope === "ip"
        ? t("microsoft.issues.subjectIp", { ip: issue.subject })
        : t("microsoft.issues.subjectDomain", { domain: issue.subject })}
    </>
  );
}

function IssueCard({ issue, delistUrl }: { issue: MicrosoftIssue; delistUrl: string }) {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const resolve = useMutation({
    mutationFn: () => api<MicrosoftIssues>(`/api/admin/microsoft/issues/${issue.id}/resolve`, { method: "POST" }),
    onSuccess: (data) => {
      queryClient.setQueryData(ISSUES_KEY, data);
      void queryClient.invalidateQueries({ queryKey: ["admin", "health"] });
      void queryClient.invalidateQueries({ queryKey: ["admin", "alerts"] });
      toast(t("microsoft.issues.resolvedToast"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });
  const language = i18n.language;
  const tone = issue.kind === "throttled" ? "warning" : "problem";

  return (
    <li className="flex flex-col gap-3 rounded-card border border-hairline bg-surface p-5">
      <header className="flex flex-wrap items-start gap-3">
        <StatusChip status={tone} label={t(`microsoft.kinds.${issue.kind}`)} />
        <div className="min-w-0 flex-1 basis-60">
          <h3 className="text-[15px] font-bold">{t(`microsoft.groups.${issue.group}.title`)}</h3>
          <p className="text-[13px] text-muted">
            <IssueSubject issue={issue} />
            {" · "}
            <code className="font-mono text-[12px]">{issue.code}</code>
            {" · "}
            {t("microsoft.issues.count", { count: issue.count })}
          </p>
          <p className="text-[12px] text-faint">
            {t("microsoft.issues.seen", {
              first: formatDateTime(issue.firstSeen, language),
              last: formatRelative(issue.lastSeen, language),
            })}
          </p>
        </div>
      </header>
      <section>
        <h4 className="text-[13px] font-bold">{t("microsoft.issues.meaning")}</h4>
        <p className="mt-0.5 text-[13px] text-muted">{t(`microsoft.groups.${issue.group}.meaning`)}</p>
      </section>
      <section className="flex flex-col gap-1.5">
        <h4 className="text-[13px] font-bold">{t("microsoft.issues.todo")}</h4>
        <WhatToDo issue={issue} delistUrl={delistUrl} />
      </section>
      {issue.reply && (
        <details>
          <summary className="cursor-pointer text-[13px] font-semibold text-muted hover:text-ink">
            {t("microsoft.issues.reply")}
          </summary>
          <pre className="mt-1.5 rounded-control bg-canvas px-3 py-2 font-mono text-[12px] break-all whitespace-pre-wrap text-muted">
            {issue.reply}
          </pre>
        </details>
      )}
      <div className="flex flex-wrap items-center gap-3 border-t border-hairline pt-3">
        <Button icon={Check} size="sm" busy={resolve.isPending} onClick={() => resolve.mutate()}>
          {t("microsoft.issues.resolve")}
        </Button>
        <span className="min-w-0 flex-1 basis-52 text-[12px] text-muted">{t("microsoft.issues.resolveHint")}</span>
      </div>
    </li>
  );
}

function ResolvedRow({ issue }: { issue: MicrosoftIssue }) {
  const { t, i18n } = useT();
  const time = formatRelative(issue.resolvedAt ?? 0, i18n.language);
  return (
    <li className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 border-b border-hairline py-2.5 text-[13px] last:border-b-0">
      <span className="font-semibold">{t(`microsoft.groups.${issue.group}.title`)}</span>
      <span className="text-muted">
        <IssueSubject issue={issue} /> · <code className="font-mono text-[12px]">{issue.code}</code>
      </span>
      <span className="w-full text-[12px] text-faint">
        {issue.resolvedBy === "auto" || !issue.resolvedBy
          ? t("microsoft.issues.resolvedAuto", { time })
          : t("microsoft.issues.resolvedBy", { time, login: issue.resolvedBy })}
      </span>
    </li>
  );
}

function Issues() {
  const { t } = useT();
  const query = useMicrosoftIssues();
  if (query.isPending) return <Loading />;
  if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
  const { issues, delistUrl } = query.data;
  const open = issues.filter(isOpen);
  const resolved = issues.filter((issue) => !isOpen(issue));
  return (
    <section className="flex flex-col gap-3" aria-labelledby="microsoft-issues">
      <h2 id="microsoft-issues" className="text-[15px] font-bold">
        {t("microsoft.issues.title")}
      </h2>
      {open.length === 0 ? (
        <div className="flex items-start gap-3 rounded-card border border-hairline bg-surface p-5">
          <span className="flex size-9 shrink-0 items-center justify-center rounded-full bg-success-tint text-success">
            <CircleCheck className="size-[18px]" aria-hidden />
          </span>
          <div>
            <p className="text-sm font-semibold">{t("microsoft.issues.none")}</p>
            <p className="text-[13px] text-muted">{t("microsoft.issues.noneHint")}</p>
          </div>
        </div>
      ) : (
        <ul className="flex flex-col gap-3">
          {open.map((issue) => (
            <IssueCard key={issue.id} issue={issue} delistUrl={delistUrl || DELIST_URL} />
          ))}
        </ul>
      )}
      {resolved.length > 0 && (
        <details className="rounded-card border border-hairline bg-surface px-5 py-3">
          <summary className="cursor-pointer text-[13px] font-semibold text-muted hover:text-ink">
            {t("microsoft.issues.earlier", { count: resolved.length })}
          </summary>
          <ul className="mt-1">
            {resolved.map((issue) => (
              <ResolvedRow key={issue.id} issue={issue} />
            ))}
          </ul>
        </details>
      )}
    </section>
  );
}

/** One rule of the checklist: a status, a title and what it means here. */
function Rule({ status, title, children }: { status: MsStatus; title: ReactNode; children: ReactNode }) {
  const { t } = useT();
  const { icon: Icon, className } = STATUS[status];
  return (
    <li className="flex items-start gap-3 border-b border-hairline py-4 first:pt-0 last:border-b-0 last:pb-0">
      <span className={clsx("mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-full", className)}>
        <Icon className="size-3.5" aria-hidden />
      </span>
      <div className="flex min-w-0 flex-1 flex-col gap-1.5">
        <h3 className="text-sm font-bold">
          {title}
          <span className="sr-only">: {t(`microsoft.status.${status}`)}</span>
        </h3>
        {children}
      </div>
    </li>
  );
}

function DomainRow({ domain }: { domain: MicrosoftDomainCheck }) {
  const { t } = useT();
  const note = dmarcNote(domain);
  const checked = domain.checkedAt !== null;
  return (
    <li className="flex flex-col gap-1.5 border-b border-hairline py-3 first:pt-0 last:border-b-0 last:pb-0">
      <div className="flex flex-wrap items-center gap-2">
        <Link
          to={`/admin/domains/${encodeURIComponent(domain.domain)}`}
          className="inline-flex items-center gap-0.5 text-sm font-semibold hover:text-pink-ink hover:underline"
        >
          {domain.domain}
          <ChevronRight className="size-3.5" aria-hidden />
        </Link>
      </div>
      {checked ? (
        <>
          <div className="flex flex-wrap gap-1.5">
            <StatusChip status={domain.spf} label="SPF" />
            <StatusChip status={domain.dkim} label="DKIM" />
            <StatusChip
              status={domain.dmarc}
              label={
                <>
                  DMARC
                  {domain.dmarcPolicy && ` p=${domain.dmarcPolicy}`}
                  {domain.dmarcPct !== null && ` pct=${domain.dmarcPct}`}
                </>
              }
            />
            <StatusChip status={domain.aligned} label={t("microsoft.checklist.aligned")} />
          </div>
          <p className="text-[12px] text-muted">
            {t(`microsoft.checklist.dmarc.${note}`, { policy: domain.dmarcPolicy ?? "", pct: domain.dmarcPct ?? 100 })}
          </p>
        </>
      ) : (
        <p className="text-[12px] text-muted">{t("microsoft.checklist.dmarc.notChecked")}</p>
      )}
    </li>
  );
}

function AddressRow({ address, checklist }: { address: MicrosoftAddress; checklist: MicrosoftChecklist }) {
  const { t } = useT();
  const status = ptrStatus(address);
  const ptr = address.ptr.join(", ");
  const text = address.private
    ? t("microsoft.checklist.ptr.private")
    : status === "problem"
      ? t("microsoft.checklist.ptr.missing", { hostname: checklist.hostname })
      : status === "warning"
        ? t("microsoft.checklist.ptr.unconfirmed", { ptr })
        : t("microsoft.checklist.ptr.ok", { ptr });
  const otherName =
    checklist.route === "direct" && status === "ok" && !address.ptrIsHostname
      ? t("microsoft.checklist.ptr.notHostname", { hostname: checklist.hostname })
      : null;
  return (
    <li className="flex flex-wrap items-start gap-2">
      <StatusChip status={status} label={<code className="font-mono">{address.ip}</code>} />
      <p className="min-w-0 flex-1 basis-60 pt-0.5 text-[12px] text-muted">
        {text}
        {otherName && ` ${otherName}`}
      </p>
    </li>
  );
}

function Checklist() {
  const { t, i18n } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const query = useChecklist();
  const check = useMutation({
    mutationFn: () => api<MicrosoftChecklist>("/api/admin/microsoft/checklist", { method: "POST" }),
    onSuccess: (data) => {
      queryClient.setQueryData(CHECKLIST_KEY, data);
      // The check looks at every domain's DNS again.
      void queryClient.invalidateQueries({ queryKey: ["admin", "domains"] });
      toast(t("microsoft.checklist.checkedToast"), "success");
    },
    onError: (error) => toast(errorText(error), "error"),
  });

  const content = () => {
    if (query.isPending) return <Loading />;
    if (query.isError) return <LoadError error={query.error} onRetry={() => void query.refetch()} />;
    const data = query.data;
    const domainsStatus = worstStatus(
      data.domains.map((domain) =>
        domain.checkedAt === null ? "unknown" : worstStatus([domain.spf, domain.dkim, domain.dmarc, domain.aligned]),
      ),
    );
    const ptrOverall =
      data.addresses.length === 0 ? "unknown" : worstStatus(data.addresses.map((address) => ptrStatus(address)));
    return (
      <div className="flex flex-col gap-4">
        <p className="-mt-1 text-[13px] text-muted">{t("microsoft.checklist.intro")}</p>
        <p className="text-[12px] text-faint">
          {t("microsoft.checklist.checkedAt", { time: formatRelative(data.checkedAt, i18n.language) })}
        </p>
        <ul className="flex flex-col">
          <Rule status={domainsStatus} title={t("microsoft.checklist.domains")}>
            <p className="text-[13px] text-muted">{t("microsoft.checklist.domainsIntro")}</p>
            {data.domains.length === 0 ? (
              <p className="text-[13px] text-faint">{t("microsoft.checklist.noDomains")}</p>
            ) : (
              <ul className="mt-1 flex flex-col">
                {data.domains.map((domain) => (
                  <DomainRow key={domain.domain} domain={domain} />
                ))}
              </ul>
            )}
          </Rule>
          <Rule status={ptrOverall} title={t("microsoft.checklist.ptr.title")}>
            <p className="text-[13px] text-muted">{t("microsoft.checklist.ptr.intro")}</p>
            <p className="text-[13px] text-muted">
              {t(`microsoft.checklist.route.${data.route}`, { relay: data.relayHost ?? "" })}
            </p>
            {data.addresses.length === 0 ? (
              <p className="text-[13px] text-faint">{t("microsoft.checklist.ptr.none")}</p>
            ) : (
              <ul className="mt-1 flex flex-col gap-2">
                {data.addresses.map((address) => (
                  <AddressRow key={address.ip} address={address} checklist={data} />
                ))}
              </ul>
            )}
          </Rule>
          <Rule status={data.tls} title={t("microsoft.checklist.tls.title")}>
            <p className="text-[13px] text-muted">{t(`microsoft.checklist.tls.${data.tls}`)}</p>
          </Rule>
          <Rule status="unknown" title={t("microsoft.checklist.unsubscribe.title")}>
            <p className="text-[13px] text-muted">{t("microsoft.checklist.unsubscribe.text")}</p>
            <pre className="rounded-control bg-canvas px-3 py-2 font-mono text-[12px] break-all whitespace-pre-wrap text-muted">
              {"List-Unsubscribe: <https://…>, <mailto:…>\nList-Unsubscribe-Post: List-Unsubscribe=One-Click"}
            </pre>
          </Rule>
        </ul>
      </div>
    );
  };

  return (
    <Card
      title={
        <span className="inline-flex items-center gap-2">
          <ListChecks className="size-4 text-pink-ink" aria-hidden />
          {t("microsoft.checklist.title")}
        </span>
      }
      action={
        <Button size="sm" icon={RotateCw} busy={check.isPending} onClick={() => check.mutate()}>
          {check.isPending ? t("microsoft.checklist.checking") : t("microsoft.checklist.check")}
        </Button>
      }
    >
      {content()}
    </Card>
  );
}

function Guides() {
  const { t } = useT();
  const checklist = useChecklist();
  const ip = checklist.data?.addresses.find((address) => !address.private)?.ip ?? t("microsoft.issues.theIp");
  return (
    <Card
      title={
        <span className="inline-flex items-center gap-2">
          <BookOpen className="size-4 text-pink-ink" aria-hidden />
          {t("microsoft.guides.title")}
        </span>
      }
    >
      <div className="flex flex-col gap-5">
        <p className="-mt-1 text-[13px] text-muted">{t("microsoft.guides.intro")}</p>
        <section className="flex flex-col gap-2">
          <h3 className="text-sm font-bold">{t("microsoft.guides.snds.title")}</h3>
          <p className="text-[13px] text-muted">{t("microsoft.guides.snds.intro")}</p>
          <Steps>
            <li>
              {t("microsoft.guides.snds.step1")} <ExternalAnchor href={SNDS_URL}>SNDS</ExternalAnchor>
            </li>
            <li>{t("microsoft.guides.snds.step2", { ip })}</li>
            <li>{t("microsoft.guides.snds.step3")}</li>
            <li>{t("microsoft.guides.snds.step4")}</li>
            <li>{t("microsoft.guides.snds.step5")}</li>
          </Steps>
        </section>
        <section className="flex flex-col gap-2 border-t border-hairline pt-4">
          <h3 className="text-sm font-bold">{t("microsoft.guides.jmrp.title")}</h3>
          <p className="text-[13px] text-muted">{t("microsoft.guides.jmrp.intro")}</p>
          <Steps>
            <li>{t("microsoft.guides.jmrp.step1")}</li>
            <li>{t("microsoft.guides.jmrp.step2")}</li>
            <li>{t("microsoft.guides.jmrp.step3")}</li>
            <li>{t("microsoft.guides.jmrp.step4")}</li>
          </Steps>
        </section>
        <section className="flex flex-col gap-2 border-t border-hairline pt-4">
          <h3 className="text-sm font-bold">{t("microsoft.guides.delist.title")}</h3>
          <p className="text-[13px] text-muted">
            {t("microsoft.guides.delist.text")} <ExternalAnchor href={DELIST_URL}>sender.office.com</ExternalAnchor>
          </p>
        </section>
      </div>
    </Card>
  );
}

/** Server → Microsoft: refusals by Outlook.com and friends, their rules for senders, and their tools. */
export function MicrosoftPage() {
  return (
    <div className="flex flex-col gap-5">
      <Issues />
      <Checklist />
      <Guides />
    </div>
  );
}

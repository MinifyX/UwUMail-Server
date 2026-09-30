import type { MicrosoftAddress, MicrosoftDomainCheck, MicrosoftIssue, MsStatus } from "@/lib/api";

export type IssueKind = MicrosoftIssue["kind"];
export type IssueGroup = MicrosoftIssue["group"];

/** What the admin can do about an issue: ask for delisting, wait, or fix the domain's records. */
export type IssueAction = "delist" | "wait" | "authenticate";

const ACTIONS: Record<IssueGroup, IssueAction> = {
  blockList: "delist",
  banned: "delist",
  ipRefused: "delist",
  throttled: "wait",
  authentication: "authenticate",
  dmarc: "authenticate",
};

export function actionOf(group: IssueGroup): IssueAction {
  return ACTIONS[group];
}

/** Worse first: a block stops all mail, an authentication refusal some of it, throttling only slows it down. */
const SEVERITY: IssueKind[] = ["blocked", "authentication", "throttled"];

export function isOpen(issue: MicrosoftIssue): boolean {
  return issue.resolvedAt === null;
}

/** The worst kind among the open issues; null when none is open. */
export function worstKind(issues: MicrosoftIssue[]): IssueKind | null {
  const open = issues.filter(isOpen);
  return SEVERITY.find((kind) => open.some((issue) => issue.kind === kind)) ?? null;
}

/** The IP address an issue is about: its subject for IP issues, otherwise the last one seen. */
export function issueIp(issue: MicrosoftIssue): string {
  return (issue.scope === "ip" && issue.subject) || issue.ip;
}

/** The sender domain an issue is about: its subject for domain issues, otherwise the last one seen. */
export function issueDomain(issue: MicrosoftIssue): string {
  return (issue.scope === "domain" && issue.subject) || issue.domain;
}

/** What the overview banner names: the codes and IP addresses of the open issues, each once. */
export function bannerFacts(issues: MicrosoftIssue[]): { codes: string[]; ips: string[]; delist: boolean } {
  const open = issues.filter(isOpen);
  const unique = (values: string[]) => [...new Set(values.filter(Boolean))];
  return {
    codes: unique(open.map((issue) => issue.code)),
    ips: unique(open.map(issueIp)),
    delist: open.some((issue) => actionOf(issue.group) === "delist"),
  };
}

/**
 * Reverse DNS of a sending address as Microsoft looks at it: a name that points back to the address
 * is fine, a name that does not is weak, no name at all is a problem. A private address is not what
 * Microsoft sees, so it cannot be judged here.
 */
export function ptrStatus(address: MicrosoftAddress): MsStatus {
  if (address.private) return "unknown";
  if (address.ptr.length === 0) return "problem";
  return address.ptrConfirmed ? "ok" : "warning";
}

/** Which explanation a domain's DMARC gets in the checklist. */
export type DmarcNote = "notChecked" | "missing" | "none" | "partial" | "strict";

export function dmarcNote(domain: MicrosoftDomainCheck): DmarcNote {
  if (domain.checkedAt === null) return "notChecked";
  if (domain.dmarcPolicy === null) return "missing";
  if (domain.dmarcPolicy === "none") return "none";
  // No pct means all of it.
  if (domain.dmarcPct !== null && domain.dmarcPct < 100) return "partial";
  return "strict";
}

const ORDER: MsStatus[] = ["ok", "unknown", "warning", "problem"];

/** The worst of a few statuses, e.g. for a domain row. */
export function worstStatus(statuses: MsStatus[]): MsStatus {
  return statuses.reduce<MsStatus>((worst, s) => (ORDER.indexOf(s) > ORDER.indexOf(worst) ? s : worst), "ok");
}

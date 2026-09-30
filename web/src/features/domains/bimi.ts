import type { BimiCertificate, BimiView } from "@/lib/api";

/** Why a domain's DMARC is not enough for BIMI, each reason with its own sentence. */
export type DmarcReason = "notChecked" | "missing" | "policy" | "pct" | "subdomains" | "weak";

/**
 * BIMI needs DMARC with p=quarantine or p=reject for all mail (no pct, or pct=100) and not sp=none.
 * An empty list means DMARC is good enough.
 */
export function dmarcReasons(dmarc: BimiView["dmarc"]): DmarcReason[] {
  if (dmarc.status === "ok") return [];
  if (dmarc.status === "unknown") return ["notChecked"];
  if (dmarc.status === "missing" || !dmarc.record) return ["missing"];
  const reasons: DmarcReason[] = [];
  const policy = dmarc.policy?.toLowerCase();
  if (policy !== "quarantine" && policy !== "reject") reasons.push("policy");
  if (dmarc.pct !== null && dmarc.pct < 100) reasons.push("pct");
  if (dmarc.subdomainPolicy?.toLowerCase() === "none") reasons.push("subdomains");
  return reasons.length > 0 ? reasons : ["weak"];
}

/** What is wrong with a stored certificate, if anything. */
export type CertificateWarning = "expired" | "notYetValid" | "otherDomain" | "noLogo" | "unknownKind";

export function certificateWarnings(certificate: BimiCertificate, now = Date.now() / 1000): CertificateWarning[] {
  const warnings: CertificateWarning[] = [];
  if (certificate.expired || certificate.notAfter < now) warnings.push("expired");
  else if (certificate.notBefore > now) warnings.push("notYetValid");
  if (!certificate.coversDomain) warnings.push("otherDomain");
  if (!certificate.hasLogotype) warnings.push("noLogo");
  if (certificate.kind === "unknown") warnings.push("unknownKind");
  return warnings;
}

/** The logo preview's address, new whenever the logo changes so the browser does not show an old one. */
export function logoPreviewPath(domain: string, updatedAt: number | null): string {
  return `/api/admin/domains/${encodeURIComponent(domain)}/bimi/logo.svg?v=${updatedAt ?? 0}`;
}

/** "#rrggbb", as the server takes it for the square behind the logo. */
export function isHexColor(value: string): boolean {
  return /^#[0-9a-fA-F]{6}$/.test(value);
}

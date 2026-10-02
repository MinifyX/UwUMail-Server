/**
 * Signatures per domain (the server's docs/signatures.md): one signature for all addresses of a
 * domain, or one for every domain (`*`); a single address may have its own. An admin may give a
 * domain a company signature, as a template or as a footer the server appends on sending.
 *
 * Kept free of React so the rules are easy to test and to carry over to the webmail and the apps.
 */

/** The key of the signature for every domain. */
export const ALL_DOMAINS = "*";

/** The placeholders a signature may use; `{address}` and `{email}` work as well. */
export const PLACEHOLDERS = ["{name}", "{adresse}", "{domain}"] as const;

export interface SignatureText {
  text: string;
  html: string;
}

export type SignatureSource = "identity" | "domain" | "allDomains" | "company" | "none";

export type CompanySignatureMode = "off" | "template" | "footer";

export interface CompanySignature {
  mode: CompanySignatureMode;
  text: string;
  html: string;
}

export interface DomainSignatureInfo {
  domain: string;
  addressCount: number;
  signature: SignatureText | null;
  company: CompanySignature | null;
  source: SignatureSource;
}

export interface IdentitySignatureInfo {
  id: number;
  name: string;
  email: string;
  domain: string;
  signature: SignatureText | null;
  effective: SignatureText;
  source: SignatureSource;
}

export interface SignatureOverview {
  state: string;
  allDomains: SignatureText | null;
  domains: DomainSignatureInfo[];
  identities: IdentitySignatureInfo[];
  limits?: { maxSize: number; maxChanges: number; placeholders: string[] };
}

/** What a change sends: per domain (or `*`) and per identity id; null removes. */
export interface SignatureChange {
  domains?: Record<string, SignatureText | null>;
  identities?: Record<string, SignatureText | null>;
}

/** Up to what the server keeps, text and HTML each (256 KiB). */
export const SIGNATURE_MAX_BYTES = 262_144;

export const EMPTY_SIGNATURE: SignatureText = { text: "", html: "" };

export function escapeHtml(text: string): string {
  return text.replace(
    /[&<>"']/g,
    (char) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[char]!,
  );
}

/**
 * `template` with `{name}`, `{adresse}` (`{address}`, `{email}`) and `{domain}` filled for one
 * address, as the server does. In HTML the values are escaped: a name is whatever its owner typed.
 */
export function fillPlaceholders(template: string, name: string, email: string, html: boolean): string {
  const domain = email.includes("@") ? email.slice(email.lastIndexOf("@") + 1) : "";
  return template.replace(/\{([^{}]{1,16})\}/g, (whole, key: string) => {
    let value: string;
    switch (key.trim().toLowerCase()) {
      case "name":
        value = name;
        break;
      case "adresse":
      case "address":
      case "email":
      case "e-mail":
        value = email;
        break;
      case "domain":
        value = domain;
        break;
      default:
        return whole;
    }
    return html ? escapeHtml(value) : value;
  });
}

export function isEmptySignature(signature: SignatureText | null | undefined): boolean {
  return !signature || (!signature.text.trim() && !signature.html.trim());
}

export function sameSignature(a: SignatureText | null | undefined, b: SignatureText | null | undefined): boolean {
  return (a?.text ?? "") === (b?.text ?? "") && (a?.html ?? "") === (b?.html ?? "");
}

/** The UTF-8 size the server counts. */
export function byteLength(text: string): number {
  return new TextEncoder().encode(text).length;
}

export function tooLarge(signature: SignatureText, max = SIGNATURE_MAX_BYTES): boolean {
  return byteLength(signature.text) > max || byteLength(signature.html) > max;
}

/** What the editor of a domain starts with, and which domains it applies to at first. */
export interface DomainEditorStart {
  signature: SignatureText;
  /** Where it comes from: the domain's own, every domain's, the company template, or nothing. */
  origin: "domain" | "allDomains" | "template" | "empty";
  targets: string[];
}

export function editorStart(overview: SignatureOverview, domain: string): DomainEditorStart {
  const info = overview.domains.find((entry) => entry.domain === domain);
  if (info?.signature) return { signature: info.signature, origin: "domain", targets: [domain] };
  if (overview.allDomains) return { signature: overview.allDomains, origin: "allDomains", targets: [ALL_DOMAINS] };
  if (info?.company?.mode === "template") {
    return { signature: { text: info.company.text, html: info.company.html }, origin: "template", targets: [domain] };
  }
  return { signature: EMPTY_SIGNATURE, origin: "empty", targets: [domain] };
}

/**
 * The change that gives `targets` the signature (null removes it). "All domains" stores it once
 * for every domain and drops the domains' own ones, so it really applies everywhere; other targets
 * next to it are covered by it.
 */
export function domainChange(
  overview: SignatureOverview,
  targets: string[],
  signature: SignatureText | null,
): SignatureChange {
  const domains: Record<string, SignatureText | null> = {};
  if (targets.includes(ALL_DOMAINS)) {
    domains[ALL_DOMAINS] = signature;
    if (signature) {
      for (const info of overview.domains) if (info.signature) domains[info.domain] = null;
    }
  } else for (const target of targets) domains[target] = signature;
  return { domains };
}

/** The domains whose own signature "all domains" would replace, other than `current`. */
export function replacedByAllDomains(overview: SignatureOverview, current: string): string[] {
  return overview.domains.filter((info) => info.signature && info.domain !== current).map((info) => info.domain);
}

/** The addresses of a domain, in the order the server lists them. */
export function identitiesOf(overview: SignatureOverview, domain: string): IdentitySignatureInfo[] {
  return overview.identities.filter((identity) => identity.domain === domain);
}

/** The signature as one address sends it, placeholders filled. */
export function previewFor(signature: SignatureText, identity: { name: string; email: string }, fallbackName = "") {
  const name = identity.name.trim() || fallbackName;
  return {
    text: fillPlaceholders(signature.text, name, identity.email, false),
    html: fillPlaceholders(signature.html, name, identity.email, true),
  };
}

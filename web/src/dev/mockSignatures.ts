import {
  ALL_DOMAINS,
  fillPlaceholders,
  type SignatureChange,
  type SignatureOverview,
  type SignatureSource,
  type SignatureText,
} from "@/features/mailbox/signatures";

/** The signatures of the mock account: per domain, for every domain, per address. */
const identities = [
  { id: 1, name: "Lorin", email: "lorin@uwu.example" },
  { id: 2, name: "UwU Verein", email: "verein@uwu.example" },
  { id: 3, name: "Lorin", email: "lorin@nyan.example" },
];
const domainSignatures: Record<string, SignatureText> = {
  "uwu.example": { text: "{name}\n{adresse}", html: "" },
};
const overrides: Record<number, SignatureText> = {};
const company = { "nyan.example": { mode: "template" as const, text: "Nyan e. V. · {name}", html: "" } };

const domainOf = (email: string) => email.slice(email.lastIndexOf("@") + 1);

export function mockSignatureOverview(): SignatureOverview {
  const forDomain = (domain: string): [SignatureText, SignatureSource] => {
    if (domainSignatures[domain]) return [domainSignatures[domain], "domain"];
    if (domainSignatures[ALL_DOMAINS]) return [domainSignatures[ALL_DOMAINS], "allDomains"];
    const template = company[domain as keyof typeof company];
    if (template) return [{ text: template.text, html: template.html }, "company"];
    return [{ text: "", html: "" }, "none"];
  };
  const domains = [...new Set(identities.map((identity) => domainOf(identity.email)))].sort();
  return {
    state: String(Date.now()),
    allDomains: domainSignatures[ALL_DOMAINS] ?? null,
    domains: domains.map((domain) => ({
      domain,
      addressCount: identities.filter((identity) => domainOf(identity.email) === domain).length,
      signature: domainSignatures[domain] ?? null,
      company: company[domain as keyof typeof company] ?? null,
      source: forDomain(domain)[1],
    })),
    identities: identities.map((identity) => {
      const own = overrides[identity.id] ?? null;
      const [signature, source] = own ? [own, "identity" as const] : forDomain(domainOf(identity.email));
      return {
        ...identity,
        domain: domainOf(identity.email),
        signature: own,
        effective: {
          text: fillPlaceholders(signature.text, identity.name, identity.email, false),
          html: fillPlaceholders(signature.html, identity.name, identity.email, true),
        },
        source,
      };
    }),
  };
}

export function mockChangeSignatures(change: SignatureChange): SignatureOverview {
  for (const [domain, signature] of Object.entries(change.domains ?? {})) {
    if (signature) domainSignatures[domain] = signature;
    else delete domainSignatures[domain];
  }
  for (const [id, signature] of Object.entries(change.identities ?? {})) {
    if (signature) overrides[Number(id)] = signature;
    else delete overrides[Number(id)];
  }
  return mockSignatureOverview();
}

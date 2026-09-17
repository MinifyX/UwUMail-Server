import type { SenderKind } from "@/lib/api";

function looksLikeNetwork(value: string): boolean {
  const [address = "", prefix, ...rest] = value.split("/");
  if (rest.length > 0 || (prefix !== undefined && !/^\d{1,3}$/.test(prefix))) return false;
  return /^\d{1,3}(\.\d{1,3}){3}$/.test(address) || (address.includes(":") && /^[0-9a-f:.]+$/i.test(address));
}

/**
 * What the server will take a value for when no kind is chosen, so the form can say it before
 * saving: an address or network, a `*.` host name, any other value with `*` a pattern, a full email
 * address, otherwise a domain.
 */
export function guessSenderKind(value: string): SenderKind {
  const trimmed = value.trim();
  if (looksLikeNetwork(trimmed)) return "ip";
  if (trimmed.startsWith("*.")) {
    const rest = trimmed.slice(2);
    if (rest.includes(".") && !rest.includes("*") && !rest.includes("@")) return "host";
  }
  if (trimmed.includes("*")) return "pattern";
  if (trimmed.includes("@") && !trimmed.startsWith("@")) return "address";
  return "domain";
}

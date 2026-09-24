/** What a WireGuard client file (.conf) says, as the VPN form needs it. */
export interface WireguardConf {
  privateKey: string;
  presharedKey: string | undefined;
  /** Comma-separated, IPv4 only when there is one: gluetun's WireGuard does not need the IPv6 one. */
  addresses: string;
  publicKey: string;
  endpointIp: string;
  endpointPort: number | null;
}

/**
 * Reads a provider's WireGuard file: [Interface] with PrivateKey and Address, [Peer] with PublicKey,
 * PresharedKey and Endpoint. Returns null when there is no private key in it.
 */
export function parseWireguardConf(text: string): WireguardConf | null {
  const values: Record<string, string> = {};
  let section = "";
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.replace(/[#;].*$/, "").trim();
    if (!line) continue;
    const header = /^\[(\w+)\]$/.exec(line);
    if (header) {
      section = header[1]!.toLowerCase();
      continue;
    }
    const [key, ...rest] = line.split("=");
    if (!key || rest.length === 0) continue;
    const name = `${section}.${key.trim().toLowerCase()}`;
    // A key's own "=" padding belongs to the value.
    if (!(name in values)) values[name] = rest.join("=").trim();
  }
  const privateKey = values["interface.privatekey"];
  if (!privateKey) return null;
  const addresses = (values["interface.address"] ?? "")
    .split(",")
    .map((address) => address.trim())
    .filter(Boolean);
  const ipv4 = addresses.filter((address) => !address.includes(":"));
  const endpoint = values["peer.endpoint"] ?? "";
  const match = /^\[?([^\]]+?)\]?:(\d+)$/.exec(endpoint);
  return {
    privateKey,
    presharedKey: values["peer.presharedkey"] || undefined,
    addresses: (ipv4.length ? ipv4 : addresses).join(","),
    publicKey: values["peer.publickey"] ?? "",
    endpointIp: match?.[1] ?? endpoint,
    endpointPort: match ? Number(match[2]) : null,
  };
}

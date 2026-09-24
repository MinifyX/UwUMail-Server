/** What a WireGuard client file (.conf) says, as the VPN form needs it. */
export interface WireguardConf {
  privateKey: string;
  presharedKey: string | undefined;
  /** Comma-separated, IPv4 only when there is one: gluetun's WireGuard does not need the IPv6 one. */
  addresses: string;
  publicKey: string;
  /** The server's address or name, as the file writes it. */
  endpointIp: string;
  endpointPort: number | null;
  /** The DNS servers of [Interface], which tell some providers apart. */
  dns: string[];
}

/** What a provider's file says about where it comes from. */
export interface Detected {
  /** gluetun's name of the provider, or "custom" for a server nobody knows. */
  provider: string;
  /** gluetun's English country name, when the file says it. */
  country?: string;
  /** The provider's name for the server, e.g. de1380, when the file or its name says it. */
  server?: string;
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
    dns: (values["interface.dns"] ?? "")
      .split(",")
      .map((server) => server.trim())
      .filter(Boolean),
  };
}

/** Countries as gluetun names them, by the two letters providers put into server names. */
const COUNTRIES: Record<string, string> = {
  al: "Albania",
  ar: "Argentina",
  at: "Austria",
  au: "Australia",
  be: "Belgium",
  bg: "Bulgaria",
  br: "Brazil",
  ca: "Canada",
  ch: "Switzerland",
  cl: "Chile",
  cy: "Cyprus",
  cz: "Czech Republic",
  de: "Germany",
  dk: "Denmark",
  ee: "Estonia",
  es: "Spain",
  fi: "Finland",
  fr: "France",
  gb: "United Kingdom",
  gr: "Greece",
  hk: "Hong Kong",
  hr: "Croatia",
  hu: "Hungary",
  ie: "Ireland",
  il: "Israel",
  is: "Iceland",
  it: "Italy",
  jp: "Japan",
  lt: "Lithuania",
  lu: "Luxembourg",
  lv: "Latvia",
  md: "Moldova",
  mx: "Mexico",
  nl: "Netherlands",
  no: "Norway",
  nz: "New Zealand",
  pl: "Poland",
  pt: "Portugal",
  ro: "Romania",
  rs: "Serbia",
  se: "Sweden",
  sg: "Singapore",
  si: "Slovenia",
  sk: "Slovakia",
  tr: "Turkey",
  ua: "Ukraine",
  uk: "United Kingdom",
  us: "United States",
  za: "South Africa",
};

/** Endpoint names and DNS servers the providers gluetun knows use in their WireGuard files. */
const PROVIDERS: { id: string; hosts: RegExp; dns?: string[] }[] = [
  { id: "nordvpn", hosts: /\.(nordhold\.net|nordvpn\.com)$/, dns: ["103.86.96.100", "103.86.99.100"] },
  { id: "mullvad", hosts: /\.mullvad\.net$/ },
  { id: "protonvpn", hosts: /\.(protonvpn\.(net|com)|proton\.me)$/, dns: ["10.2.0.1"] },
  { id: "surfshark", hosts: /\.surfshark\.com$/ },
  { id: "ivpn", hosts: /\.ivpn\.net$/ },
  { id: "airvpn", hosts: /\.(airdns\.org|airvpn\.org)$/ },
  { id: "windscribe", hosts: /\.(windscribe\.com|whiskergalaxy\.com)$/ },
];

/**
 * Which provider a WireGuard file belongs to, and where its server is: NordVPN writes
 * `Endpoint = frankfurt.de.wg.nordhold.net` and names the file after the server (de1380-nordvpn.conf).
 */
export function detectProvider(conf: WireguardConf, fileName = ""): Detected {
  const host = conf.endpointIp.toLowerCase();
  const found = PROVIDERS.find(
    (provider) => provider.hosts.test(host) || provider.dns?.some((server) => conf.dns.includes(server)),
  );
  if (!found) return { provider: "custom" };
  const detected: Detected = { provider: found.id };
  // Two letters in front of a server number, in the file's name or the endpoint: de1380, ch-zrh-wg-001.
  const server = /(?:^|[^a-z])([a-z]{2}(?:-[a-z]{3}-wg-)?\d{2,4})(?:[^0-9]|$)/i.exec(fileName.toLowerCase());
  if (server) detected.server = server[1];
  const code = server?.[1]?.slice(0, 2) ?? host.split(".").find((label) => label.length === 2 && label in COUNTRIES);
  if (code && COUNTRIES[code]) detected.country = COUNTRIES[code];
  return detected;
}

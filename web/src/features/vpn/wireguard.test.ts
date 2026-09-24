import { describe, expect, it } from "vitest";
import { detectProvider, parseWireguardConf } from "./wireguard";

describe("parseWireguardConf", () => {
  it("reads a provider's file", () => {
    const conf = `[Interface]
# Device: Happy Cat
PrivateKey = aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa=
Address = 10.64.0.2/32, fc00:bbbb:bbbb:bb01::1:2/128
DNS = 10.64.0.1

[Peer]
PublicKey = ccccccccccccccccccccccccccccccccccccccccccc=
PresharedKey = bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb=
AllowedIPs = 0.0.0.0/0, ::/0
Endpoint = 203.0.113.10:51820
`;
    expect(parseWireguardConf(conf)).toEqual({
      privateKey: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa=",
      presharedKey: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb=",
      addresses: "10.64.0.2/32",
      publicKey: "ccccccccccccccccccccccccccccccccccccccccccc=",
      endpointIp: "203.0.113.10",
      endpointPort: 51820,
      dns: ["10.64.0.1"],
    });
  });

  it("takes an IPv6 endpoint and refuses a file without a key", () => {
    const conf = "[Interface]\nPrivateKey=abc=\n[Peer]\nEndpoint=[2001:db8::1]:443";
    expect(parseWireguardConf(conf)?.endpointIp).toBe("2001:db8::1");
    expect(parseWireguardConf(conf)?.endpointPort).toBe(443);
    expect(parseWireguardConf("[Peer]\nPublicKey=x")).toBeNull();
  });

  it("recognises a NordVPN file with its country and server", () => {
    const conf = parseWireguardConf(`[Interface]
PrivateKey = ${"a".repeat(43)}=
Address = 10.5.0.2/16
DNS = 103.86.96.100

[Peer]
PublicKey = ${"c".repeat(43)}=
AllowedIPs = 0.0.0.0/0
Endpoint = frankfurt.de.wg.nordhold.net:51820
PersistentKeepalive = 25
`)!;
    expect(conf.addresses).toBe("10.5.0.2/16");
    expect(detectProvider(conf, "de1380-nordvpn.conf")).toEqual({
      provider: "nordvpn",
      server: "de1380",
      country: "Germany",
    });
    // The endpoint alone is enough.
    expect(detectProvider(conf)).toEqual({ provider: "nordvpn", country: "Germany" });
  });

  it("takes an unknown server for an own one", () => {
    const conf = parseWireguardConf(
      `[Interface]\nPrivateKey=${"a".repeat(43)}=\n[Peer]\nEndpoint=vpn.example.net:51820`,
    )!;
    expect(detectProvider(conf, "home.conf")).toEqual({ provider: "custom" });
  });
});

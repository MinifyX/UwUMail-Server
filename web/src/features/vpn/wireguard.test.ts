import { describe, expect, it } from "vitest";
import { parseWireguardConf } from "./wireguard";

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
    });
  });

  it("takes an IPv6 endpoint and refuses a file without a key", () => {
    const conf = "[Interface]\nPrivateKey=abc=\n[Peer]\nEndpoint=[2001:db8::1]:443";
    expect(parseWireguardConf(conf)?.endpointIp).toBe("2001:db8::1");
    expect(parseWireguardConf(conf)?.endpointPort).toBe(443);
    expect(parseWireguardConf("[Peer]\nPublicKey=x")).toBeNull();
  });
});

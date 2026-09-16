import { describe, expect, it } from "vitest";
import { guessSenderKind } from "./senders";

describe("guessSenderKind", () => {
  it("tells addresses, networks, host patterns and domains apart like the server", () => {
    expect(guessSenderKind("192.0.2.10")).toBe("ip");
    expect(guessSenderKind(" 198.51.100.0/24 ")).toBe("ip");
    expect(guessSenderKind("2001:db8::/48")).toBe("ip");
    expect(guessSenderKind("news@example.com")).toBe("address");
    expect(guessSenderKind("@example.com")).toBe("domain");
    expect(guessSenderKind("*.mail.example.com")).toBe("host");
    expect(guessSenderKind("example.com")).toBe("domain");
    expect(guessSenderKind("192.0.2.10/abc")).toBe("domain");
  });
});

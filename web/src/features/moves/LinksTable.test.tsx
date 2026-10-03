import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { i18n } from "@/i18n";
import { LinksTable } from "./LinksCard";

afterEach(cleanup);
beforeAll(async () => {
  await i18n.changeLanguage("de");
});

describe("LinksTable", () => {
  it("lists a link per mailbox and says why the others have none", () => {
    render(
      <LinksTable
        origin="https://mail.example.com"
        links={{
          hostname: "mail.example.com",
          links: [
            {
              mailboxId: 1,
              address: "nyu@example.com",
              name: "Nyu",
              oldAddress: "nyu@example.net",
              path: "/password/abc",
              expiresAt: 1_900_000_000,
            },
          ],
          skipped: [{ mailboxId: 2, address: "mini@example.com", reason: "hasPassword" }],
        }}
      />,
    );
    expect(screen.getByText("https://mail.example.com/password/abc")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Link für nyu@example.com kopieren" })).toBeTruthy();
    expect(screen.getByText("hat schon ein Passwort, kein Link nötig")).toBeTruthy();
  });
});

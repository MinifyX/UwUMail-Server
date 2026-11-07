import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { i18n } from "@/i18n";
import { RefusedList } from "./RefusedCard";

afterEach(cleanup);
beforeAll(async () => {
  await i18n.changeLanguage("de");
});

describe("RefusedList", () => {
  it("shows who a mail was for, the other server's answer, and when sending was given up", () => {
    render(
      <RefusedList
        refused={[
          {
            id: 2,
            at: 1_900_000_000,
            from: "mini@example.com",
            to: "opa@example.org",
            error: "451 4.7.1 try again later",
            gaveUp: true,
            canResend: true,
          },
          {
            id: 1,
            at: 1_899_990_000,
            from: "",
            to: "oma@example.net",
            error: "550 5.7.1 rejected by policy",
            gaveUp: false,
            canResend: false,
          },
        ]}
      />,
    );
    expect(screen.getByText("opa@example.org")).toBeTruthy();
    expect(screen.getByText("mini@example.com", { exact: false })).toBeTruthy();
    expect(screen.getByText("550 5.7.1 rejected by policy")).toBeTruthy();
    // Only the mail that was retried until it expired carries the badge.
    expect(screen.getAllByText("nach Wiederholungen aufgegeben")).toHaveLength(1);
    // A mail without a sender is the server's own notice.
    expect(screen.getByText("Rückmeldung an einen Absender", { exact: false })).toBeTruthy();
  });

  it("marks a mail fixed, one at a time", () => {
    const onResend = vi.fn();
    const mail = {
      id: 7,
      at: 1_900_000_000,
      from: "mini@example.com",
      to: "opa@example.org",
      error: "550 5.7.1 (S3150)",
    };
    const { rerender } = render(
      <RefusedList refused={[{ ...mail, gaveUp: false, canResend: true }]} onResend={onResend} />,
    );
    fireEvent.click(screen.getByRole("button", { name: /Behoben/ }));
    expect(onResend).toHaveBeenCalledWith(7);
    rerender(<RefusedList refused={[{ ...mail, gaveUp: false, canResend: true }]} onResend={onResend} resending={7} />);
    expect((screen.getByRole("button", { name: /Behoben/ }) as HTMLButtonElement).disabled).toBe(true);
  });
});

import { describe, expect, it } from "vitest";
import type { MoveInfo } from "@/lib/api";
import {
  emptyRow,
  escapeHtml,
  isBlank,
  isBusy,
  linksCsv,
  mergeCsv,
  problemsByRow,
  quotaTooSmall,
  rowsToBody,
  share,
  splitAliases,
  tableIndexes,
} from "./moves";

const MB = 1024 * 1024;

describe("rows", () => {
  it("leaves blank rows out and turns the rest into what the API takes", () => {
    const rows = [
      {
        ...emptyRow(),
        oldAddress: " mini@example.com ",
        password: " geheim ",
        quotaMb: "1,5",
        aliases: "Info@example.com, sales@example.com",
      },
      emptyRow(),
      { ...emptyRow(), oldAddress: "nyu@example.com", quotaMb: "viel" },
    ];
    const body = rowsToBody(rows);
    expect(body).toHaveLength(2);
    expect(body[0]).toMatchObject({
      oldAddress: "mini@example.com",
      // Passwords are sent as typed: spaces can be part of them.
      password: " geheim ",
      quotaBytes: Math.round(1.5 * MB),
      aliases: ["info@example.com", "sales@example.com"],
      imapPort: null,
    });
    expect(body[1]!.quotaBytes).toBeNull();
    expect(tableIndexes(rows)).toEqual([0, 2]);
    expect(isBlank({ ...emptyRow(), name: "  " })).toBe(true);
  });

  it("splits aliases at the usual separators", () => {
    expect(splitAliases(" a@example.com;b@example.com | c@example.com\nd@example.com ")).toEqual([
      "a@example.com",
      "b@example.com",
      "c@example.com",
      "d@example.com",
    ]);
    expect(splitAliases("   ")).toEqual([]);
  });

  it("puts CSV rows after the typed ones, replacing blank rows", () => {
    const typed = { ...emptyRow(), oldAddress: "mini@example.com" };
    const merged = mergeCsv([typed, emptyRow()], {
      rows: [
        {
          line: 2,
          oldAddress: "nyu@example.com",
          login: "",
          password: "pw",
          name: "Nyu",
          target: "",
          quotaBytes: 2048 * MB,
          aliases: ["info@example.com"],
        },
      ],
      problems: [],
      delimiter: ";",
      header: true,
    });
    expect(merged.map((row) => row.oldAddress)).toEqual(["mini@example.com", "nyu@example.com"]);
    expect(merged[1]).toMatchObject({ quotaMb: "2048", aliases: "info@example.com" });
    expect(mergeCsv([emptyRow()], { rows: [], problems: [], delimiter: ",", header: false })).toEqual([emptyRow()]);
  });

  it("groups the problems of a check by row", () => {
    const byRow = problemsByRow([
      { row: 0, field: "password", code: "passwordMissing" },
      { row: 2, field: "target", code: "targetTaken" },
      { row: 0, field: "aliases", code: "aliasTaken" },
    ]);
    expect(byRow.get(0)?.map((problem) => problem.code)).toEqual(["passwordMissing", "aliasTaken"]);
    expect(byRow.has(1)).toBe(false);
  });
});

describe("progress", () => {
  it("stays between 0 and 1", () => {
    expect(share(5, 10)).toBe(0.5);
    expect(share(0, 0)).toBe(0);
    expect(share(12, 10)).toBe(1);
  });

  it("warns when the old mailbox does not fit", () => {
    expect(quotaTooSmall({ quotaBytes: 100 * MB, sourceBytes: 90 * MB, usedBytes: 20 * MB })).toBe(true);
    expect(quotaTooSmall({ quotaBytes: 100 * MB, sourceBytes: 50 * MB, usedBytes: 20 * MB })).toBe(false);
    expect(quotaTooSmall({ quotaBytes: 0, sourceBytes: 900 * MB, usedBytes: 0 })).toBe(false);
    expect(quotaTooSmall({ quotaBytes: 100 * MB, sourceBytes: null, usedBytes: 0 })).toBe(false);
  });

  it("follows a move only while it has work", () => {
    const move = (state: MoveInfo["state"], queued: number) =>
      ({ state, summary: { queued, running: 0 } }) as unknown as MoveInfo;
    expect(isBusy(move("active", 1))).toBe(true);
    expect(isBusy(move("active", 0))).toBe(false);
    expect(isBusy(move("paused", 3))).toBe(false);
    expect(isBusy(move("finishing", 2))).toBe(true);
  });
});

describe("links", () => {
  it("writes a CSV a spreadsheet opens, quoting what needs it", () => {
    const csv = linksCsv(
      [
        {
          mailboxId: 1,
          address: "mini@example.com",
          name: 'Mini "die Kleine"; Muster',
          oldAddress: "mini@example.net",
          path: "/password/abc",
          expiresAt: 0,
        },
      ],
      "https://mail.example.com",
      ["Postfach", "Name", "Alte Adresse", "Link", "Gültig bis"],
      () => "9. Oktober",
    );
    expect(csv.startsWith("﻿")).toBe(true);
    expect(csv.slice(1).split("\r\n")).toEqual([
      "Postfach;Name;Alte Adresse;Link;Gültig bis",
      'mini@example.com;"Mini ""die Kleine""; Muster";mini@example.net;https://mail.example.com/password/abc;9. Oktober',
      "",
    ]);
  });

  it("escapes text for the printed page", () => {
    expect(escapeHtml(`<b class="x">Tom & Jerry's</b>`)).toBe(
      "&lt;b class=&quot;x&quot;&gt;Tom &amp; Jerry&#39;s&lt;/b&gt;",
    );
  });
});

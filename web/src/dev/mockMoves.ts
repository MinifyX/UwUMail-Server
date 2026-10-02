/**
 * Moves for the pretend server: one domain move halfway through with a few people (one of them
 * stuck on a wrong password) and a finished single mailbox move.
 */

import type {
  MoveDetail,
  MoveInfo,
  MoveLimits,
  MoveLinks,
  MoveMailboxInfo,
  MoveMailboxState,
  MovePlan,
  MoveRow,
  MovesView,
} from "@/lib/api";

type Handler = (body: unknown, params: string[], search: URLSearchParams) => [number, unknown];

const now = Math.floor(Date.now() / 1000);
const MB = 1024 ** 2;
const problem = (status: number, code: string): [number, unknown] => [status, { code, detail: code }];

const limits: MoveLimits = {
  maxMailboxes: 2000,
  maxParallel: 8,
  defaultParallel: 2,
  minSyncMinutes: 5,
  maxSyncMinutes: 1440,
  defaultSyncMinutes: 60,
  maxUploadBytes: 20 * MB,
};

function mailbox(
  id: number,
  moveId: number,
  local: string,
  domain: string,
  name: string,
  state: MoveMailboxState,
  extra: Partial<MoveMailboxInfo> = {},
): MoveMailboxInfo {
  return {
    id,
    moveId,
    accountId: 100 + id,
    address: `${local}@${domain}`,
    displayName: name,
    quotaBytes: 0,
    usedBytes: 120 * MB,
    oldAddress: `${local}@${domain}`,
    login: `${local}@${domain}`,
    imapHost: "",
    imapPort: 993,
    davUrl: "",
    createdAccount: true,
    hasPassword: true,
    hasPortalPassword: false,
    aliases: [],
    state,
    finalRound: false,
    error: "",
    errorDetail: "",
    foldersDone: 8,
    foldersTotal: 8,
    messagesDone: 2140,
    messagesTotal: 2140,
    messagesSkipped: 0,
    bytesDone: 120 * MB,
    sourceBytes: 120 * MB,
    contactsDone: 84,
    eventsDone: 213,
    davError: "",
    rounds: 3,
    createdAt: now - 2 * 86_400,
    lastRunAt: now - 1200,
    lastSyncedAt: now - 1200,
    nextSyncAt: now + 2400,
    finishedAt: null,
    ...extra,
  };
}

const moves: MoveInfo[] = [];
const boxes: MoveMailboxInfo[] = [];
let nextMove = 1;
let nextBox = 1;

function newMove(extra: Partial<MoveInfo>): MoveInfo {
  const move: MoveInfo = {
    id: nextMove++,
    kind: "domain",
    domain: "",
    imapHost: "imap.example.net",
    imapPort: 993,
    davMode: "sogo",
    davHost: "",
    davUrl: "",
    contacts: true,
    calendars: true,
    parallel: 2,
    syncMinutes: 60,
    state: "active",
    createdAt: now - 2 * 86_400,
    finishRequestedAt: null,
    finishedAt: null,
    summary: emptySummary(),
    ...extra,
  };
  moves.push(move);
  return move;
}

function emptySummary(): MoveInfo["summary"] {
  return {
    mailboxes: 0,
    queued: 0,
    running: 0,
    paused: 0,
    synced: 0,
    done: 0,
    messagesDone: 0,
    messagesTotal: 0,
    messagesSkipped: 0,
    bytesDone: 0,
    contactsDone: 0,
    eventsDone: 0,
  };
}

const firm = newMove({ domain: "kanzlei.example" });
for (const [local, name, state, extra] of [
  ["mini", "Mini Muster", "synced", { hasPortalPassword: true, aliases: ["info@kanzlei.example"] }],
  ["nyu", "Nyu Neko", "running", { messagesDone: 900, foldersDone: 3, nextSyncAt: null, lastSyncedAt: null }],
  [
    "kiki",
    "Kiki Kurz",
    "paused",
    { error: "loginRefused", errorDetail: "NO [AUTHENTICATIONFAILED]", messagesDone: 0, foldersDone: 0, rounds: 0 },
  ],
  ["lou", "Lou Lang", "queued", { messagesDone: 0, foldersDone: 0, rounds: 0, lastRunAt: null, lastSyncedAt: null }],
  [
    "pia",
    "Pia Platt",
    "synced",
    { quotaBytes: 100 * MB, sourceBytes: 160 * MB, davError: "providerNotFound", contactsDone: 0, eventsDone: 0 },
  ],
] as [string, string, MoveMailboxState, Partial<MoveMailboxInfo>][]) {
  boxes.push(mailbox(nextBox++, firm.id, local, firm.domain, name, state, extra));
}

const single = newMove({
  kind: "mailbox",
  domain: "uwu.example",
  imapHost: "imap.example.org",
  davMode: "auto",
  state: "done",
  createdAt: now - 20 * 86_400,
  finishRequestedAt: now - 18 * 86_400,
  finishedAt: now - 18 * 86_400,
});
boxes.push(
  mailbox(nextBox++, single.id, "mini", single.domain, "Mini", "done", {
    oldAddress: "mini@example.org",
    login: "mini@example.org",
    createdAccount: false,
    hasPassword: false,
    hasPortalPassword: true,
    finishedAt: single.finishedAt,
    nextSyncAt: null,
  }),
);

function summed(move: MoveInfo): MoveInfo {
  const summary = emptySummary();
  for (const box of boxes.filter((entry) => entry.moveId === move.id)) {
    summary.mailboxes += 1;
    summary[box.state] += 1;
    summary.messagesDone += box.messagesDone;
    summary.messagesTotal += box.messagesTotal;
    summary.messagesSkipped += box.messagesSkipped;
    summary.bytesDone += box.bytesDone;
    summary.contactsDone += box.contactsDone;
    summary.eventsDone += box.eventsDone;
  }
  return { ...move, summary };
}

const find = (id: string) => moves.find((move) => move.id === Number(id));
const view = (): MovesView => ({ moves: moves.map(summed).reverse(), limits });
const detail = (move: MoveInfo): MoveDetail => ({
  move: summed(move),
  mailboxes: boxes.filter((box) => box.moveId === move.id),
  limits,
  hostname: "mail.uwu.example",
});

/** Runs a change on a move that is still open, answering with the move. */
function open(id: string, change: (move: MoveInfo) => [number, unknown] | void): [number, unknown] {
  const move = find(id);
  if (!move) return problem(404, "notFound");
  if (move.state === "done") return problem(409, "moveFinished");
  return change(move) ?? [200, detail(move)];
}

function addRows(move: MoveInfo, rows: MoveRow[]) {
  for (const row of rows) {
    const local = row.oldAddress.split("@")[0] ?? "neu";
    const target = row.target || `${local}@${move.domain}`;
    const [targetLocal = local] = target.split("@");
    boxes.push(
      mailbox(nextBox++, move.id, targetLocal, move.domain, row.name, "queued", {
        oldAddress: row.oldAddress,
        login: row.login || row.oldAddress,
        quotaBytes: row.quotaBytes ?? 0,
        aliases: row.aliases,
        messagesDone: 0,
        messagesTotal: 0,
        foldersDone: 0,
        foldersTotal: 0,
        bytesDone: 0,
        sourceBytes: null,
        contactsDone: 0,
        eventsDone: 0,
        rounds: 0,
        lastRunAt: null,
        lastSyncedAt: null,
        nextSyncAt: null,
        createdAt: Math.floor(Date.now() / 1000),
      }),
    );
  }
}

function plan(body: { domain: string; rows: MoveRow[] }): MovePlan {
  const problems: MovePlan["problems"] = [];
  body.rows.forEach((row, index) => {
    if (!row.oldAddress.includes("@")) problems.push({ row: index, field: "oldAddress", code: "addressInvalid" });
    if (!row.password) problems.push({ row: index, field: "password", code: "passwordMissing" });
  });
  return {
    domainExists: ["uwu.example", "kanzlei.example"].includes(body.domain),
    rows: body.rows.map((row, index) => ({
      row: index,
      target: row.target || `${row.oldAddress.split("@")[0]}@${body.domain}`,
      exists: false,
      hasPassword: false,
    })),
    problems,
  };
}

export const moveMockRoutes: [string, RegExp, Handler][] = [
  ["GET", /^\/api\/admin\/moves$/, () => [200, view()]],
  [
    "POST",
    /^\/api\/admin\/moves\/discover$/,
    () => [
      200,
      { imap: { host: "imap.example.net", port: 993, source: "autoconfig" }, davMode: "sogo", domainHere: true },
    ],
  ],
  [
    "POST",
    /^\/api\/admin\/moves\/csv$/,
    (body) => {
      const lines = (body as { text: string }).text.split(/\r?\n/).filter((line) => line.trim());
      const delimiter = lines[0]?.includes(";") ? ";" : ",";
      const header = /adress|address/i.test(lines[0] ?? "") && !(lines[0] ?? "").includes("@");
      const rows = lines.slice(header ? 1 : 0).map((line, index) => {
        const [oldAddress = "", password = "", name = "", target = ""] = line
          .split(delimiter)
          .map((cell) => cell.trim());
        return {
          line: index + (header ? 2 : 1),
          oldAddress,
          login: "",
          password,
          name,
          target,
          quotaBytes: null,
          aliases: [],
        };
      });
      const problems = rows
        .filter((row) => !row.oldAddress.includes("@"))
        .map((row) => ({ line: row.line, field: "oldAddress", code: "addressInvalid" }));
      return [200, { rows: rows.filter((row) => row.oldAddress.includes("@")), problems, delimiter, header }];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/moves$/,
    (body) => {
      const request = body as { kind: MoveInfo["kind"]; domain: string; dryRun: boolean; rows: MoveRow[] } & MoveInfo;
      const result = plan(request);
      if (request.dryRun) return [200, result];
      if (result.problems.length > 0) return [409, { code: "moveRows", detail: "moveRows", blockers: result.problems }];
      const move = newMove({
        kind: request.kind,
        domain: request.domain,
        imapHost: request.imapHost,
        imapPort: request.imapPort,
        davMode: request.davMode,
        contacts: request.contacts,
        calendars: request.calendars,
        parallel: request.parallel,
        syncMinutes: request.syncMinutes,
        createdAt: Math.floor(Date.now() / 1000),
      });
      addRows(move, request.rows);
      return [200, detail(move)];
    },
  ],
  [
    "GET",
    /^\/api\/admin\/moves\/(\d+)$/,
    (_body, [id]) => {
      const move = find(id!);
      return move ? [200, detail(move)] : problem(404, "notFound");
    },
  ],
  [
    "PATCH",
    /^\/api\/admin\/moves\/(\d+)$/,
    (body, [id]) =>
      open(id!, (move) => {
        Object.assign(move, body as Partial<MoveInfo>);
      }),
  ],
  [
    "DELETE",
    /^\/api\/admin\/moves\/(\d+)$/,
    (_body, [id]) => {
      const at = moves.findIndex((move) => move.id === Number(id));
      if (at < 0) return problem(404, "notFound");
      moves.splice(at, 1);
      return [204, null];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/moves\/(\d+)\/pause$/,
    (_body, [id]) =>
      open(id!, (move) => {
        if (move.state !== "active") return problem(409, "moveNotActive");
        move.state = "paused";
      }),
  ],
  [
    "POST",
    /^\/api\/admin\/moves\/(\d+)\/resume$/,
    (_body, [id]) =>
      open(id!, (move) => {
        if (move.state !== "paused") return problem(409, "moveNotPaused");
        move.state = "active";
      }),
  ],
  [
    "POST",
    /^\/api\/admin\/moves\/(\d+)\/finish$/,
    (body, [id]) =>
      open(id!, (move) => {
        const at = Math.floor(Date.now() / 1000);
        move.finishRequestedAt = at;
        // The pretend server is quick: the last round is over at once.
        move.state = "done";
        move.finishedAt = at;
        for (const box of boxes.filter((entry) => entry.moveId === move.id)) {
          Object.assign(box, { state: "done", hasPassword: false, finishedAt: at, nextSyncAt: null });
          if (!(body as { skipLastRound?: boolean }).skipLastRound) box.rounds += 1;
        }
      }),
  ],
  [
    "POST",
    /^\/api\/admin\/moves\/(\d+)\/mx$/,
    () => [
      200,
      {
        check: { status: "wrong", found: ["mx.example.net"], expected: "mail.uwu.example", note: null },
        hostname: "mail.uwu.example",
      },
    ],
  ],
  [
    "POST",
    /^\/api\/admin\/moves\/(\d+)\/links$/,
    (_body, [id]) => {
      const move = find(id!);
      if (!move) return problem(404, "notFound");
      const own = boxes.filter((box) => box.moveId === move.id);
      const links: MoveLinks = {
        links: own
          .filter((box) => !box.hasPortalPassword)
          .map((box) => ({
            mailboxId: box.id,
            address: box.address,
            name: box.displayName,
            oldAddress: box.oldAddress,
            path: `/password/mock-${Math.random().toString(36).slice(2)}`,
            expiresAt: Math.floor(Date.now() / 1000) + 7 * 86_400,
          })),
        skipped: own
          .filter((box) => box.hasPortalPassword)
          .map((box) => ({ mailboxId: box.id, address: box.address, reason: "hasPassword" as const })),
        hostname: "mail.uwu.example",
      };
      return [200, links];
    },
  ],
  [
    "POST",
    /^\/api\/admin\/moves\/(\d+)\/mailboxes$/,
    (body, [id]) =>
      open(id!, (move) => {
        if (move.kind === "mailbox") return problem(409, "moveSingle");
        addRows(move, (body as { rows: MoveRow[] }).rows);
      }),
  ],
  [
    "POST",
    /^\/api\/admin\/moves\/(\d+)\/mailboxes\/(\d+)\/retry$/,
    (body, [id, boxId]) =>
      open(id!, () => {
        const box = boxes.find((entry) => entry.id === Number(boxId));
        if (!box) return problem(404, "notFound");
        const change = body as { login?: string | null };
        Object.assign(box, { state: "queued", error: "", errorDetail: "", login: change.login || box.login });
      }),
  ],
  [
    "POST",
    /^\/api\/admin\/moves\/(\d+)\/mailboxes\/(\d+)\/pause$/,
    (_body, [id, boxId]) =>
      open(id!, () => {
        const box = boxes.find((entry) => entry.id === Number(boxId));
        if (!box) return problem(404, "notFound");
        Object.assign(box, { state: "paused", error: "stopped" });
      }),
  ],
  [
    "DELETE",
    /^\/api\/admin\/moves\/(\d+)\/mailboxes\/(\d+)$/,
    (_body, [id, boxId]) =>
      open(id!, () => {
        const at = boxes.findIndex((entry) => entry.id === Number(boxId));
        if (at < 0) return problem(404, "notFound");
        boxes.splice(at, 1);
      }),
  ],
  [
    "POST",
    /^\/api\/admin\/moves\/(\d+)\/mailboxes\/(\d+)\/import$/,
    () => [200, { report: { created: 42, updated: 0, unchanged: 3 } }],
  ],
];

import type { MoveCsvRead, MoveInfo, MoveLink, MoveMailboxInfo, MoveRow, MoveRowProblem } from "@/lib/api";

/** Where the moves live in the portal. */
export const movesPath = "/admin/moves";
export const newMovePath = "/admin/moves/new";
export const moveUrl = (id: number) => `/admin/moves/${id}`;

/** A row of the table as the admin types it: quota in megabytes, aliases as one line. */
export interface EditRow {
  oldAddress: string;
  login: string;
  password: string;
  name: string;
  target: string;
  quotaMb: string;
  aliases: string;
  imapHost: string;
  davUrl: string;
}

export function emptyRow(): EditRow {
  return {
    oldAddress: "",
    login: "",
    password: "",
    name: "",
    target: "",
    quotaMb: "",
    aliases: "",
    imapHost: "",
    davUrl: "",
  };
}

/** Whether a row was left untouched, so it is not sent. */
export function isBlank(row: EditRow): boolean {
  return Object.values(row).every((value) => value.trim() === "");
}

const MB = 1024 * 1024;

/** Splits an alias line at spaces, commas, semicolons and bars. */
export function splitAliases(text: string): string[] {
  return text
    .split(/[\s,;|]+/)
    .map((alias) => alias.trim().toLowerCase())
    .filter(Boolean);
}

/** The table's rows as the API takes them. A target left empty is made by the server. */
export function rowsToBody(rows: EditRow[]): MoveRow[] {
  return rows.filter((row) => !isBlank(row)).map(rowToBody);
}

export function rowToBody(row: EditRow): MoveRow {
  const quota = row.quotaMb.trim().replace(",", ".");
  const megabytes = quota === "" ? null : Number(quota);
  return {
    oldAddress: row.oldAddress.trim(),
    login: row.login.trim(),
    password: row.password,
    name: row.name.trim(),
    target: row.target.trim(),
    quotaBytes: megabytes === null || !Number.isFinite(megabytes) ? null : Math.round(megabytes * MB),
    aliases: splitAliases(row.aliases),
    imapHost: row.imapHost.trim(),
    imapPort: null,
    davUrl: row.davUrl.trim(),
  };
}

/** Rows read from a CSV list, for the table: replacing the blank rows, after the ones typed in. */
export function mergeCsv(rows: EditRow[], read: MoveCsvRead): EditRow[] {
  const typed = rows.filter((row) => !isBlank(row));
  const added = read.rows.map((row) => ({
    ...emptyRow(),
    oldAddress: row.oldAddress,
    login: row.login,
    password: row.password,
    name: row.name,
    target: row.target,
    quotaMb: row.quotaBytes === null ? "" : String(Math.round((row.quotaBytes / MB) * 100) / 100),
    aliases: row.aliases.join(" "),
  }));
  const merged = [...typed, ...added];
  return merged.length > 0 ? merged : [emptyRow()];
}

/** The problems of a check, by the row (of the sent list) they are about. */
export function problemsByRow(problems: MoveRowProblem[]): Map<number, MoveRowProblem[]> {
  const byRow = new Map<number, MoveRowProblem[]>();
  for (const problem of problems) {
    byRow.set(problem.row, [...(byRow.get(problem.row) ?? []), problem]);
  }
  return byRow;
}

/** The sent list leaves blank rows out; this maps a sent row back to the table's. */
export function tableIndexes(rows: EditRow[]): number[] {
  return rows.flatMap((row, index) => (isBlank(row) ? [] : [index]));
}

/** How far a move or a mailbox got, from 0 to 1. */
export function share(done: number, total: number): number {
  return total > 0 ? Math.min(1, Math.max(0, done / total)) : 0;
}

/** Whether a mailbox here will be too small for what the old one holds. */
export function quotaTooSmall(mailbox: Pick<MoveMailboxInfo, "quotaBytes" | "sourceBytes" | "usedBytes">): boolean {
  if (mailbox.quotaBytes <= 0 || mailbox.sourceBytes === null) return false;
  return mailbox.sourceBytes + mailbox.usedBytes > mailbox.quotaBytes;
}

/** Whether the move still does something on its own, so the page follows it more closely. */
export function isBusy(move: MoveInfo): boolean {
  return (move.state === "active" || move.state === "finishing") && move.summary.queued + move.summary.running > 0;
}

/**
 * A cell for CSV: quoted when it holds the delimiter, a quote or a line break. A cell a spreadsheet
 * would read as a formula (starting with `=`, `+`, `-`, `@`, a tab or a carriage return) gets a
 * leading `'` and is quoted, so a name from the customer's list cannot run one next to the live
 * password links (security review 0.22 MOV-2).
 */
export function csvCell(value: string): string {
  const safe = /^[=+\-@\t\r]/.test(value) ? `'${value}` : value;
  return safe !== value || /[;"\n\r]/.test(safe) ? `"${safe.replace(/"/g, '""')}"` : safe;
}

/** The password links as a CSV file a spreadsheet opens: one line per mailbox. */
export function linksCsv(
  links: MoveLink[],
  origin: string,
  headers: string[],
  expires: (at: number) => string,
): string {
  const lines = [headers.map(csvCell).join(";")];
  for (const link of links) {
    lines.push(
      [link.address, link.name, link.oldAddress, `${origin}${link.path}`, expires(link.expiresAt)].map(csvCell).join(";"),
    );
  }
  // A byte order mark, so spreadsheets read umlauts right.
  return `\uFEFF${lines.join("\r\n")}\r\n`;
}

/** Text for an HTML page; the printable overview builds one by hand. */
export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

// Checks a running UwUMail server from the outside, the way the app uses it:
// web portal, JMAP session, mailboxes, push, sending through JMAP and, optionally, a login
// on the submission port and the reply of an outside mail server.
//
//   UWUMAIL_URL=https://mail.example.com UWUMAIL_LOGIN=test@example.com \
//   UWUMAIL_PASSWORD_FILE=test.password node scripts/live-check.mjs \
//     [--to someone@example.org ...] [--smtp host:587] [--imap host:993] [--wait-reply-from example.org] [--minutes 10]
//
// --to               send a check mail to these addresses (outside ones leave like any other mail:
//                    through the relay, the gateway or directly)
// --smtp             also log in on this submission port with STARTTLS; the certificate is checked
//                    against the host name in UWUMAIL_URL, so host may be an address, e.g. the
//                    gateway's public one when the name resolves to something else at home
// --imap             also use IMAP with TLS like a mail app: capabilities, folders, QRESYNC, and IDLE
//                    noticing a draft made through JMAP, which IMAP then deletes again
// --wait-reply-from  wait for a mail from this address or domain and print its Authentication-Results,
//                    e.g. with --to check-auth@verifier.port25.com --wait-reply-from port25.com

import { readFileSync } from "node:fs";
import net from "node:net";
import tls from "node:tls";

const args = { to: [], smtp: null, imap: null, waitReplyFrom: null, minutes: 10 };
for (let i = 2; i < process.argv.length; i++) {
  const flag = process.argv[i];
  const value = process.argv[++i];
  if (value === undefined) throw new Error(`${flag} needs a value`);
  if (flag === "--to") args.to.push(value);
  else if (flag === "--smtp") args.smtp = value;
  else if (flag === "--imap") args.imap = value;
  else if (flag === "--wait-reply-from") args.waitReplyFrom = value.toLowerCase();
  else if (flag === "--minutes") args.minutes = Number(value);
  else throw new Error(`unknown option ${flag}`);
}

const base = (process.env.UWUMAIL_URL ?? "").replace(/\/+$/, "");
const login = process.env.UWUMAIL_LOGIN;
const password = process.env.UWUMAIL_PASSWORD_FILE
  ? readFileSync(process.env.UWUMAIL_PASSWORD_FILE, "utf8").trim()
  : process.env.UWUMAIL_PASSWORD;
if (!base || !login || !password) {
  throw new Error("set UWUMAIL_URL, UWUMAIL_LOGIN and UWUMAIL_PASSWORD (or UWUMAIL_PASSWORD_FILE)");
}

const authorization = `Basic ${Buffer.from(`${login}:${password}`).toString("base64")}`;
const NUL = String.fromCharCode(0);
const runId = new Date().toISOString().replace(/[-:]/g, "").slice(0, 15);
const started = new Date();
const ok = (text) => console.log(`  ✓ ${text}`);

async function getSession() {
  const response = await fetch(`${base}/.well-known/jmap`, { headers: { Authorization: authorization } });
  if (response.status !== 200) throw new Error(`JMAP session: HTTP ${response.status} ${await response.text()}`);
  const session = await response.json();
  if (!session.capabilities?.["urn:ietf:params:jmap:mail"]) throw new Error("the session has no mail capability");
  if (base.startsWith("https:")) {
    for (const key of ["apiUrl", "uploadUrl", "downloadUrl", "eventSourceUrl"]) {
      if (!session[key].startsWith("https://")) throw new Error(`${key} is not https: ${session[key]}`);
    }
  }
  return session;
}

async function api(session, methodCalls) {
  const response = await fetch(session.apiUrl, {
    method: "POST",
    headers: { Authorization: authorization, "Content-Type": "application/json" },
    body: JSON.stringify({
      using: ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail", "urn:ietf:params:jmap:submission"],
      methodCalls,
    }),
  });
  if (response.status !== 200) throw new Error(`JMAP API: HTTP ${response.status} ${await response.text()}`);
  const { methodResponses } = await response.json();
  for (const [name, result] of methodResponses) {
    if (name === "error") throw new Error(`JMAP error: ${JSON.stringify(result)}`);
    for (const failure of ["notCreated", "notUpdated", "notDestroyed"]) {
      if (result[failure] && Object.keys(result[failure]).length > 0) {
        throw new Error(`${name} ${failure}: ${JSON.stringify(result[failure])}`);
      }
    }
  }
  return methodResponses;
}

/** Collects push events in the background. */
function listen(session) {
  const events = [];
  const controller = new AbortController();
  const url = session.eventSourceUrl.replace("{types}", "*").replace("{closeafter}", "no").replace("{ping}", "0");
  const done = fetch(url, { headers: { Authorization: authorization, Accept: "text/event-stream" }, signal: controller.signal })
    .then(async (response) => {
      if (response.status !== 200) throw new Error(`EventSource: HTTP ${response.status}`);
      const decoder = new TextDecoder();
      let buffer = "";
      for await (const chunk of response.body) {
        buffer += decoder.decode(chunk, { stream: true });
        let end;
        while ((end = buffer.indexOf("\n\n")) >= 0) {
          const block = buffer.slice(0, end);
          buffer = buffer.slice(end + 2);
          const data = block
            .split("\n")
            .filter((line) => line.startsWith("data:"))
            .map((line) => line.slice(5).trim())
            .join("\n");
          if (data) events.push(JSON.parse(data));
        }
      }
    })
    .catch((error) => {
      if (error.name !== "AbortError") events.push({ error: String(error) });
    });
  return { events, stop: () => (controller.abort(), done) };
}

async function waitUntil(description, seconds, check) {
  const deadline = Date.now() + seconds * 1000;
  while (Date.now() < deadline) {
    const result = await check();
    if (result) return result;
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
  throw new Error(`timed out waiting until ${description}`);
}

class Smtp {
  constructor(socket) {
    this.attach(socket);
  }

  attach(socket) {
    this.socket = socket;
    this.buffer = "";
    this.waiting = [];
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => {
      this.buffer += chunk;
      this.flush();
    });
  }

  flush() {
    while (this.waiting.length > 0) {
      const lines = this.buffer.split("\r\n");
      const end = lines.findIndex((line) => /^\d{3}( |$)/.test(line));
      if (end < 0) return;
      this.buffer = lines.slice(end + 1).join("\r\n");
      this.waiting.shift()(lines.slice(0, end + 1).join("\n"));
    }
  }

  async command(line, expected) {
    if (line !== null) this.socket.write(`${line}\r\n`);
    const reply = await new Promise((resolve) => {
      this.waiting.push(resolve);
      this.flush();
    });
    if (!reply.startsWith(expected)) throw new Error(`SMTP "${(line ?? "greeting").slice(0, 12)}…" got "${reply}"`);
    return reply;
  }
}

async function smtpLogin(target, servername) {
  const [host, port] = target.split(":");
  const socket = net.connect(Number(port ?? 587), host);
  await new Promise((resolve, reject) => socket.once("connect", resolve).once("error", reject));
  const smtp = new Smtp(socket);
  await smtp.command(null, "220");
  await smtp.command("EHLO live-check.invalid", "250");
  await smtp.command("STARTTLS", "220");
  socket.removeAllListeners("data");
  const secure = tls.connect({ socket, servername });
  await new Promise((resolve, reject) => secure.once("secureConnect", resolve).once("error", reject));
  smtp.attach(secure);
  const certificate = secure.getPeerCertificate();
  await smtp.command("EHLO live-check.invalid", "250");
  const token = Buffer.from(`${NUL}${login}${NUL}${password}`).toString("base64");
  await smtp.command(`AUTH PLAIN ${token}`, "235");
  await smtp.command("QUIT", "221");
  secure.end();
  return `${certificate.issuer?.O ?? "?"}, valid until ${certificate.valid_to}`;
}

class Imap {
  constructor(socket) {
    this.socket = socket;
    this.buffer = Buffer.alloc(0);
    this.lines = [];
    this.waiters = [];
    this.tag = 0;
    socket.on("data", (chunk) => {
      this.buffer = Buffer.concat([this.buffer, chunk]);
      this.split();
    });
  }

  /** Complete response lines, with the data of literals kept inline. */
  split() {
    for (;;) {
      let end = this.buffer.indexOf("\r\n");
      let consumed = 0;
      while (end >= 0) {
        const head = this.buffer.subarray(consumed, end).toString("latin1");
        const literal = /\{(\d+)\}$/.exec(head);
        if (!literal) break;
        consumed = end + 2 + Number(literal[1]);
        if (this.buffer.length < consumed) return;
        end = this.buffer.indexOf("\r\n", consumed);
      }
      if (end < 0) return;
      this.lines.push(this.buffer.subarray(0, end).toString("utf8"));
      this.buffer = this.buffer.subarray(end + 2);
      while (this.waiters.length > 0 && this.lines.length > 0) this.waiters.shift()(this.lines.shift());
    }
  }

  next(seconds = 20) {
    if (this.lines.length > 0) return Promise.resolve(this.lines.shift());
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("IMAP: no answer in time")), seconds * 1000);
      this.waiters.push((line) => {
        clearTimeout(timer);
        resolve(line);
      });
    });
  }

  async command(line) {
    const tag = `c${++this.tag}`;
    this.socket.write(`${tag} ${line}\r\n`);
    const untagged = [];
    for (;;) {
      const answer = await this.next();
      if (!answer.startsWith(`${tag} `)) {
        untagged.push(answer);
        continue;
      }
      if (!answer.startsWith(`${tag} OK`)) throw new Error(`IMAP "${line.split(" ")[0]}" got "${answer}"`);
      return untagged;
    }
  }
}

/** IMAP like a mail app: login, folders, QRESYNC, and IDLE hearing about a draft made through JMAP. */
async function checkImap(target, servername, session, accountId, mailboxes) {
  const [host, port] = target.split(":");
  const socket = tls.connect({ host, port: Number(port ?? 993), servername });
  await new Promise((resolve, reject) => socket.once("secureConnect", resolve).once("error", reject));
  const imap = new Imap(socket);
  const greeting = await imap.next();
  if (!greeting.startsWith("* OK")) throw new Error(`IMAP greeting: ${greeting}`);
  const token = Buffer.from(`${NUL}${login}${NUL}${password}`).toString("base64");
  await imap.command(`AUTHENTICATE PLAIN ${token}`);
  const capabilities = (await imap.command("CAPABILITY")).join(" ");
  for (const needed of ["IDLE", "UIDPLUS", "MOVE", "SPECIAL-USE", "CONDSTORE", "QRESYNC"]) {
    if (!capabilities.includes(` ${needed}`)) throw new Error(`IMAP lacks ${needed}`);
  }
  const folders = await imap.command('LIST "" "*"');
  const sent = folders.find((line) => line.includes("\\Sent"));
  if (!folders.some((line) => line.endsWith('"INBOX"')) || !sent) throw new Error(`IMAP folders: ${folders.join(" | ")}`);
  await imap.command("ENABLE QRESYNC");
  const inbox = await imap.command("SELECT INBOX");
  const exists = inbox.find((line) => line.endsWith(" EXISTS"));
  if (!inbox.some((line) => line.includes("[HIGHESTMODSEQ "))) throw new Error("IMAP SELECT has no HIGHESTMODSEQ");
  if (exists && !exists.startsWith("* 0 ")) {
    const fetched = await imap.command("FETCH * (UID FLAGS ENVELOPE BODYSTRUCTURE BODY.PEEK[HEADER.FIELDS (SUBJECT)])");
    if (!fetched.some((line) => line.includes("BODYSTRUCTURE ("))) throw new Error("IMAP FETCH has no BODYSTRUCTURE");
  }

  const drafts = mailboxes.list.find((mailbox) => mailbox.role === "drafts");
  const draftsName = folders.find((line) => line.includes("\\Drafts"))?.match(/"([^"]+)"$/)?.[1];
  if (!draftsName) throw new Error("IMAP shows no Drafts folder");
  await imap.command(`SELECT "${draftsName}"`);
  imap.socket.write("idle IDLE\r\n");
  if (!(await imap.next()).startsWith("+")) throw new Error("IMAP IDLE was not accepted");
  const [[, created]] = await api(session, [
    [
      "Email/set",
      {
        accountId,
        create: {
          draft: {
            mailboxIds: { [drafts.id]: true },
            keywords: { $draft: true, $seen: true },
            subject: `UwUMail IMAP check ${runId}`,
            bodyValues: { text: { value: "IMAP check, deleted again right away" } },
            textBody: [{ partId: "text", type: "text/plain" }],
          },
        },
      },
      "0",
    ],
  ]);
  let line;
  do line = await imap.next(20);
  while (!line.endsWith(" EXISTS"));
  imap.socket.write("DONE\r\n");
  do line = await imap.next();
  while (line.startsWith("* "));
  if (!line.startsWith("idle OK")) throw new Error(`IMAP IDLE did not end: ${line}`);
  const found = await imap.command(`UID SEARCH SUBJECT "UwUMail IMAP check ${runId}"`);
  const uid = found.find((entry) => entry.startsWith("* SEARCH "))?.split(" ")[2];
  if (!uid) throw new Error(`IMAP did not find the draft: ${found.join(" | ")}`);
  await imap.command(`UID STORE ${uid} +FLAGS.SILENT (\\Deleted)`);
  const vanished = await imap.command(`UID EXPUNGE ${uid}`);
  if (!vanished.some((entry) => entry === `* VANISHED ${uid}`)) throw new Error(`IMAP expunge: ${vanished.join(" | ")}`);
  const [[, gone]] = await api(session, [["Email/get", { accountId, ids: [created.created.draft.id], properties: ["id"] }, "0"]]);
  if (gone.notFound?.length !== 1) throw new Error("the draft deleted through IMAP is still there in JMAP");
  await imap.command("LOGOUT");
  socket.end();
  return folders.length;
}

/** The web portal: its page and headers, then a login, the account page's data and a logout. */
async function checkPortal() {
  const page = await fetch(`${base}/account`);
  const html = await page.text();
  if (page.status !== 200 || !html.includes('<div id="root">')) throw new Error(`portal page: HTTP ${page.status}`);
  if (!page.headers.get("content-security-policy")?.includes("frame-ancestors 'none'")) {
    throw new Error("the portal page has no Content-Security-Policy");
  }
  const loggedIn = await fetch(`${base}/api/auth/login`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ login, password }),
  });
  if (loggedIn.status !== 200) throw new Error(`portal login: HTTP ${loggedIn.status} ${await loggedIn.text()}`);
  const setCookie = loggedIn.headers.get("set-cookie") ?? "";
  if (base.startsWith("https:") && !(setCookie.startsWith("__Host-uwumail=") && setCookie.includes("Secure"))) {
    throw new Error(`over HTTPS the session cookie must be __Host- and Secure: ${setCookie.split("=")[0]}`);
  }
  const cookie = setCookie.split(";")[0];
  const { csrfToken } = await loggedIn.json();
  const profile = await fetch(`${base}/api/account`, { headers: { Cookie: cookie } });
  if (profile.status !== 200) throw new Error(`portal account: HTTP ${profile.status}`);
  const { role } = (await (await fetch(`${base}/api/session`, { headers: { Cookie: cookie } })).json()).account;
  const health = await fetch(`${base}/api/admin/health`, { headers: { Cookie: cookie } });
  if (role !== "admin" && health.status !== 403) throw new Error(`a non-admin got HTTP ${health.status} for the health overview`);
  if (role === "admin" && health.status !== 200) throw new Error(`health overview: HTTP ${health.status}`);
  const logout = await fetch(`${base}/api/auth/logout`, {
    method: "POST",
    headers: { Cookie: cookie, "X-CSRF-Token": csrfToken, "Content-Type": "application/json" },
    body: "{}",
  });
  if (logout.status !== 204) throw new Error(`portal logout: HTTP ${logout.status}`);
  return (await profile.json()).addresses.length;
}

console.log(`UwUMail live check against ${base} as ${login}`);
ok(`web portal: page with CSP, login with a secure cookie, ${await checkPortal()} address(es), logout`);
const session = await getSession();
const accountId = session.primaryAccounts["urn:ietf:params:jmap:mail"];
ok(`JMAP session, endpoints ${new URL(session.apiUrl).protocol}//`);

const push = listen(session);
const [[, mailboxes], [, identities]] = await api(session, [
  ["Mailbox/get", { accountId, properties: ["name", "role", "totalEmails"] }, "0"],
  ["Identity/get", { accountId }, "1"],
]);
const role = (name) => mailboxes.list.find((mailbox) => mailbox.role === name);
for (const name of ["inbox", "drafts", "sent", "junk", "trash"]) {
  if (!role(name)) throw new Error(`no ${name} mailbox`);
}
const identity = identities.list.find((entry) => entry.email === login) ?? identities.list[0];
ok(`${mailboxes.list.length} mailboxes, identity ${identity.email}`);

if (args.smtp) ok(`SMTP login on ${args.smtp}, certificate by ${await smtpLogin(args.smtp, new URL(base).hostname)}`);
if (args.imap) {
  const folders = await checkImap(args.imap, new URL(base).hostname, session, accountId, mailboxes);
  ok(`IMAP on ${args.imap}: ${folders} folders, QRESYNC, IDLE saw a JMAP draft, UID EXPUNGE removed it`);
}

if (args.to.length > 0) {
  const subject = `UwUMail live check ${runId}`;
  const [, [, submission]] = await api(session, [
    [
      "Email/set",
      {
        accountId,
        create: {
          check: {
            mailboxIds: { [role("drafts").id]: true },
            keywords: { $draft: true, $seen: true },
            from: [{ name: identity.name, email: identity.email }],
            to: args.to.map((email) => ({ email })),
            subject,
            bodyValues: { text: { value: `Hallo! Das ist eine automatische Testmail von UwUMail (=^･ω･^=)\n\nLauf ${runId}\n` } },
            textBody: [{ partId: "text", type: "text/plain" }],
          },
        },
      },
      "0",
    ],
    [
      "EmailSubmission/set",
      {
        accountId,
        create: { send: { emailId: "#check", identityId: identity.id } },
        onSuccessUpdateEmail: {
          "#send": { [`mailboxIds/${role("drafts").id}`]: null, [`mailboxIds/${role("sent").id}`]: true, "keywords/$draft": null },
        },
      },
      "1",
    ],
  ]);
  ok(`sent "${subject}" to ${args.to.join(", ")} (submission ${submission.created.send.id})`);
  await waitUntil("push reports the sent mail", 15, () => push.events.some((event) => event["@type"] === "StateChange"));
  ok("push reported the change");
}

if (args.waitReplyFrom) {
  console.log(`  … waiting up to ${args.minutes} minutes for a mail from ${args.waitReplyFrom}`);
  const reply = await waitUntil(`a mail from ${args.waitReplyFrom} arrives`, args.minutes * 60, async () => {
    const [[, query]] = await api(session, [
      ["Email/query", { accountId, filter: { from: args.waitReplyFrom, after: started.toISOString() }, sort: [{ property: "receivedAt", isAscending: false }], limit: 1 }, "0"],
    ]);
    if (query.ids.length === 0) {
      await new Promise((resolve) => setTimeout(resolve, 9000));
      return null;
    }
    const [[, emails]] = await api(session, [
      [
        "Email/get",
        {
          accountId,
          ids: query.ids,
          properties: ["subject", "from", "mailboxIds", "header:Authentication-Results:asText:all", "bodyValues", "textBody"],
          fetchTextBodyValues: true,
        },
        "0",
      ],
    ]);
    return emails.list[0];
  });
  const folder = mailboxes.list.find((mailbox) => reply.mailboxIds[mailbox.id])?.name ?? "?";
  ok(`reply "${reply.subject}" from ${reply.from?.[0]?.email} landed in ${folder}`);
  for (const header of reply["header:Authentication-Results:asText:all"] ?? []) {
    console.log(`    Authentication-Results: ${header.replace(/\s+/g, " ")}`);
  }
  // A verifier report lists "<name> check details:" blocks, each with a "Result:" line.
  const body = Object.values(reply.bodyValues ?? {})[0]?.value ?? "";
  let check = null;
  for (const line of body.split(/\r?\n/).map((l) => l.trim())) {
    const heading = /^"?([\w-]+)"? check details:$/i.exec(line);
    if (heading) check = heading[1];
    else if (check && line.startsWith("Result:")) {
      console.log(`    ${check}: ${line.slice(7).trim()}`);
      check = null;
    }
  }
}

const pushErrors = push.events.filter((event) => event.error);
await push.stop();
if (pushErrors.length > 0) throw new Error(`push failed: ${pushErrors[0].error}`);
console.log("All good (=^･ω･^=)");

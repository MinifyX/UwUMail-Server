// Checks a running UwUMail server from the outside, the way the app uses it:
// JMAP session, mailboxes, push, sending through JMAP and, optionally, a login
// on the submission port and the reply of an outside mail server.
//
//   UWUMAIL_URL=https://mail.example.com UWUMAIL_LOGIN=test@example.com \
//   UWUMAIL_PASSWORD_FILE=test.password node scripts/live-check.mjs \
//     [--to someone@example.org ...] [--smtp host:587] [--wait-reply-from example.org] [--minutes 10]
//
// --to               send a check mail to these addresses (through the relay for outside ones)
// --smtp             also log in on this submission port with STARTTLS (the certificate must be valid)
// --wait-reply-from  wait for a mail from this address or domain and print its Authentication-Results,
//                    e.g. with --to check-auth@verifier.port25.com --wait-reply-from port25.com

import { readFileSync } from "node:fs";
import net from "node:net";
import tls from "node:tls";

const args = { to: [], smtp: null, waitReplyFrom: null, minutes: 10 };
for (let i = 2; i < process.argv.length; i++) {
  const flag = process.argv[i];
  const value = process.argv[++i];
  if (value === undefined) throw new Error(`${flag} needs a value`);
  if (flag === "--to") args.to.push(value);
  else if (flag === "--smtp") args.smtp = value;
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

console.log(`UwUMail live check against ${base} as ${login}`);
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
  const body = Object.values(reply.bodyValues ?? {})[0]?.value ?? "";
  const summary = body.split(/\r?\n/).filter((line) => /^(SPF|DKIM|DMARC|iprev|Sender-ID|SpamAssassin) check:|^(Summary of Results|=+)/i.test(line.trim()));
  if (summary.length > 0) console.log(summary.map((line) => `    ${line.trim()}`).join("\n"));
}

const pushErrors = push.events.filter((event) => event.error);
await push.stop();
if (pushErrors.length > 0) throw new Error(`push failed: ${pushErrors[0].error}`);
console.log("All good (=^･ω･^=)");

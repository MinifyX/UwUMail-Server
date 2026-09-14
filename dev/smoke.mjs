// Smoke test for the local stack: submits a mail on a.test and checks that it
// arrives locally, on b.test, and that an unknown recipient bounces.
//
//   node dev/smoke.mjs

import { execFileSync } from "node:child_process";
import https from "node:https";
import net from "node:net";
import tls from "node:tls";
import { fileURLToPath } from "node:url";

const password = process.env.UWUMAIL_DEV_PASSWORD ?? "katzenpfote-123";
const composeFile = fileURLToPath(new URL("./compose.yaml", import.meta.url));
const runId = Date.now().toString(36);
const NUL = String.fromCharCode(0);

class SmtpConnection {
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
      const reply = lines.slice(0, end + 1).join("\n");
      this.buffer = lines.slice(end + 1).join("\r\n");
      this.waiting.shift()(reply);
    }
  }

  reply() {
    return new Promise((resolve) => {
      this.waiting.push(resolve);
      this.flush();
    });
  }

  async command(line, expected) {
    this.socket.write(`${line}\r\n`);
    const reply = await this.reply();
    if (expected && !reply.startsWith(expected)) {
      throw new Error(`"${line.slice(0, 40)}" got "${reply}", expected ${expected}`);
    }
    return reply;
  }

  async startTls() {
    await this.command("STARTTLS", "220");
    this.socket.removeAllListeners("data");
    const secure = tls.connect({ socket: this.socket, rejectUnauthorized: false, servername: "localhost" });
    await new Promise((resolve, reject) => secure.once("secureConnect", resolve).once("error", reject));
    this.attach(secure);
  }
}

async function submit() {
  const socket = net.connect(2587, "127.0.0.1");
  await new Promise((resolve, reject) => socket.once("connect", resolve).once("error", reject));
  const smtp = new SmtpConnection(socket);
  const greeting = await smtp.reply();
  if (!greeting.startsWith("220")) throw new Error(`greeting: ${greeting}`);
  await smtp.command("EHLO smoke.test", "250");
  await smtp.startTls();
  await smtp.command("EHLO smoke.test", "250");
  const token = Buffer.from(`${NUL}mini@a.test${NUL}${password}`).toString("base64");
  await smtp.command(`AUTH PLAIN ${token}`, "235");
  await smtp.command("MAIL FROM:<mini@a.test>", "250");
  for (const rcpt of ["ami@a.test", "nyu@b.test", "ghost@b.test"]) {
    await smtp.command(`RCPT TO:<${rcpt}>`, "250");
  }
  await smtp.command("DATA", "354");
  const message = [
    "From: Mini <mini@a.test>",
    "To: ami@a.test, nyu@b.test, ghost@b.test",
    `Subject: Rauchtest ${runId}`,
    "",
    "Hallo aus dem Rauchtest (=^･ω･^=)",
    ".",
  ].join("\r\n");
  const accepted = await smtp.command(message, "250");
  await smtp.command("QUIT", "221");
  return accepted;
}

function logs(service) {
  return execFileSync("docker", ["compose", "-f", composeFile, "logs", "--no-color", "--since", "5m", service], {
    encoding: "utf8",
  });
}

async function waitFor(description, check) {
  const started = Date.now();
  while (Date.now() - started < 30_000) {
    if (check()) {
      console.log(`  ✓ ${description}`);
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  throw new Error(`timed out waiting until ${description}`);
}

function jmapSession(port) {
  const token = Buffer.from(`mini@a.test:${password}`).toString("base64");
  return new Promise((resolve, reject) => {
    https
      .get(
        { host: "127.0.0.1", port, path: "/.well-known/jmap", rejectUnauthorized: false, headers: { Authorization: `Basic ${token}` } },
        (response) => {
          let body = "";
          response.on("data", (chunk) => (body += chunk));
          response.on("end", () => resolve({ status: response.statusCode, body }));
        },
      )
      .on("error", reject);
  });
}

function health(port) {
  return new Promise((resolve, reject) => {
    https
      .get({ host: "127.0.0.1", port, path: "/healthz", rejectUnauthorized: false }, (response) => {
        let body = "";
        response.on("data", (chunk) => (body += chunk));
        response.on("end", () => resolve(JSON.parse(body)));
      })
      .on("error", reject);
  });
}

console.log("UwUMail smoke test");
for (const port of [8443, 9443]) {
  const status = await health(port);
  if (status.status !== "ok") throw new Error(`health on ${port}: ${JSON.stringify(status)}`);
}
console.log("  ✓ both servers answer on HTTPS");
const session = await jmapSession(8443);
if (session.status !== 200 || !JSON.parse(session.body).capabilities["urn:ietf:params:jmap:mail"]) {
  throw new Error(`JMAP session: ${session.status} ${session.body}`);
}
console.log("  ✓ JMAP session for mini@a.test");

console.log(`  ✓ submitted: ${(await submit()).trim()}`);
const logLine = (service, ...parts) => logs(service).split(/\r?\n/).some((line) => parts.every((p) => line.includes(p)));

await waitFor("a.test handed the message to b.test", () => logLine("a", "delivered", "nyu@b.test"));
await waitFor("b.test received the message for nyu", () => logLine("b", "received message"));
await waitFor("the unknown recipient bounced back to mini", () => logLine("a", "delivery failed", "ghost@b.test"));
console.log("All good (=^･ω･^=)");

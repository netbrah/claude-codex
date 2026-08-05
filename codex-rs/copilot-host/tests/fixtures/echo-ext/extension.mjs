// Minimal extension whose wire behavior mirrors what @github/copilot-sdk
// does after `joinSession()`:
//   1. speaks vscode-jsonrpc framing (Content-Length + body) on stdio
//   2. sends `session.resume` with a tool registration
//   3. replies to `tool.call` requests for the tool it registered
//
// This fixture is deliberately free-standing so the test can run without the
// real @github/copilot-sdk (which is proprietary and not redistributable).
import process from "node:process";

// ---------- framing ----------
let inbox = Buffer.alloc(0);
function encode(obj) {
  const body = Buffer.from(JSON.stringify(obj), "utf8");
  const header = Buffer.from(`Content-Length: ${body.length}\r\n\r\n`, "utf8");
  return Buffer.concat([header, body]);
}
function tryParse() {
  const sep = inbox.indexOf("\r\n\r\n");
  if (sep < 0) return null;
  const header = inbox.slice(0, sep).toString("utf8");
  const m = /Content-Length:\s*(\d+)/i.exec(header);
  if (!m) throw new Error("missing Content-Length");
  const len = parseInt(m[1], 10);
  const start = sep + 4;
  if (inbox.length < start + len) return null;
  const body = inbox.slice(start, start + len).toString("utf8");
  inbox = inbox.slice(start + len);
  return JSON.parse(body);
}

// ---------- send helpers ----------
let nextId = 1;
const pending = new Map();
function send(obj) {
  process.stdout.write(encode(obj));
}
function request(method, params) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    send({ jsonrpc: "2.0", id, method, params });
  });
}
function respond(id, result, error) {
  send({ jsonrpc: "2.0", id, ...(error ? { error } : { result }) });
}

// ---------- inbound routing ----------
function handle(msg) {
  if (msg.id !== undefined && (msg.result !== undefined || msg.error !== undefined)) {
    const p = pending.get(msg.id);
    if (p) {
      pending.delete(msg.id);
      msg.error ? p.reject(msg.error) : p.resolve(msg.result);
    }
    return;
  }
  if (msg.method === "tool.call") {
    const { toolName, arguments: args } = msg.params ?? {};
    if (toolName === "echo") {
      respond(msg.id, {
        content: [{ type: "text", text: `echo: ${args?.text ?? ""}` }],
      });
    } else {
      respond(msg.id, null, { code: -32601, message: `unknown tool ${toolName}` });
    }
    return;
  }
  if (msg.method === "ping") {
    respond(msg.id, { ok: true });
    return;
  }
  // Any other host->child request we stub as method-not-found.
  if (msg.id !== undefined) {
    respond(msg.id, null, { code: -32601, message: `Method not found: ${msg.method}` });
  }
}

// ---------- stdin wiring ----------
process.stdin.on("data", (chunk) => {
  inbox = Buffer.concat([inbox, chunk]);
  try {
    let msg;
    while ((msg = tryParse()) !== null) handle(msg);
  } catch (err) {
    process.stderr.write(`[echo-ext] parse error: ${err}\n`);
    process.exit(1);
  }
});
process.stdin.on("end", () => process.exit(0));

// ---------- handshake ----------
const sessionId = process.env.SESSION_ID;
if (!sessionId) {
  process.stderr.write("[echo-ext] SESSION_ID missing\n");
  process.exit(1);
}

await request("session.resume", {
  sessionId,
  clientName: "xli-test-echo",
  tools: [
    {
      name: "echo",
      description: "Echo the text argument back (XLI fixture).",
      parameters: {
        type: "object",
        properties: { text: { type: "string" } },
        required: ["text"],
      },
    },
  ],
  commands: [],
  requestPermission: false,
  requestUserInput: false,
  requestElicitation: false,
  hooks: false,
  envValueMode: "direct",
});

process.stderr.write("[echo-ext] session.resume ack\n");

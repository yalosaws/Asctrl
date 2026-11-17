// Tests the hosted MCP server (mcp-http.ts) without a driver: a local relay, a fake computer that answers calls,
// and MCP clients signing in both ways (an access key as Bearer, and the OAuth flow a connector goes through).
// Ends with the stdio server, to check it still has what only it should have.
// Usage: node test/http.ts

import { createHash, randomBytes } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { WebSocket } from "ws";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { startRelay } from "../src/relay/server.ts";
import { startHttpServer } from "../src/console/mcp-http.ts";
import { AuthStore } from "../src/console/auth.ts";

const RELAY_PORT = 18797;
const HTTP_PORT = 18798;
const CODE = "654321";
const BASE = `http://localhost:${HTTP_PORT}`;

let failed = 0;
function check(name: string, cond: boolean, detail = "") {
  console.log(`${cond ? "✔" : "✘"} ${name}${detail ? " — " + detail : ""}`);
  if (!cond) failed++;
}

const dir = mkdtempSync(path.join(tmpdir(), "asctrl-http-"));
const store = new AuthStore(path.join(dir, "auth.json"));
const alice = store.addKey("alice");
const bob = store.addKey("bob");

const relay = startRelay(RELAY_PORT);
const srv = startHttpServer({ port: HTTP_PORT, host: "127.0.0.1", publicUrl: BASE, relayUrl: `ws://localhost:${RELAY_PORT}`, store });

// A computer showing CODE: allows whoever asks, and answers every call with what it was asked
let consentName = "";
const agent = new WebSocket(`ws://localhost:${RELAY_PORT}`);
agent.on("open", () => agent.send(JSON.stringify({ type: "agent_hello", code: CODE, hostname: "FAKE-PC" })));
agent.on("message", (raw) => {
  const m = JSON.parse(String(raw));
  if (m.type === "consent_request") {
    consentName = m.techName;
    agent.send(JSON.stringify({ type: "consent_reply", allow: true }));
  } else if (m.type === "call") {
    agent.send(JSON.stringify({ type: "result", id: m.id, ok: true, text: `fake ${m.tool} ${JSON.stringify(m.args)}` }));
  }
});

await new Promise((r) => setTimeout(r, 400));

async function mcp(token: string, name = "test-agent") {
  const c = new Client({ name, version: "1.0.0" });
  const t = new StreamableHTTPClientTransport(new URL("/mcp", BASE), { requestInit: { headers: { Authorization: `Bearer ${token}` } } });
  await c.connect(t);
  return { c, t };
}

const text = (r: any) => (r.content ?? []).map((c: any) => c.text ?? "").join("\n");

try {
  // ---- Unauthenticated: 401 pointing at the metadata ----
  const r401 = await fetch(`${BASE}/mcp`, { method: "POST", headers: { "content-type": "application/json" }, body: "{}" });
  const www = r401.headers.get("www-authenticate") ?? "";
  check("no token → 401", r401.status === 401);
  check("401 names the resource metadata", www.includes("/.well-known/oauth-protected-resource/mcp"), www);

  const prm = await (await fetch(`${BASE}/.well-known/oauth-protected-resource/mcp`)).json();
  check("protected resource metadata", prm.resource === `${BASE}/mcp` && prm.authorization_servers?.length === 1, JSON.stringify(prm));
  const asm = await (await fetch(`${BASE}/.well-known/oauth-authorization-server`)).json();
  check("authorization server metadata", !!asm.registration_endpoint && asm.code_challenge_methods_supported?.includes("S256"));

  // ---- The orb, for connector lists ----
  const fav = await fetch(`${BASE}/favicon.ico`);
  check("favicon served", fav.ok && (fav.headers.get("content-type") ?? "").includes("icon"), fav.headers.get("content-type") ?? "");
  const png = await fetch(`${BASE}/icons/128x128.png`);
  check("png icon served", png.ok && png.headers.get("content-type") === "image/png");
  check("other files refused", (await fetch(`${BASE}/icons/..%2F..%2Ftauri.conf.json`)).status === 404);

  // ---- Bearer with an access key ----
  const a = await mcp(alice.key);
  const tools = (await a.c.listTools()).tools;
  const names = tools.map((t) => t.name);
  check("tools listed", names.includes("run") && names.includes("connect"), `${names.length} tools`);
  check("no server-disk tools when hosted", !names.includes("upload_file") && !names.includes("download_file"));
  const connectSchema = tools.find((t) => t.name === "connect")?.inputSchema as any;
  check("connect needs a code when hosted", connectSchema?.required?.includes("code"));
  const icons = (a.c.getServerVersion() as any)?.icons ?? [];
  check("server icons are https URLs", icons.length === 3 && icons.every((i: any) => i.src.startsWith(`${BASE}/icons/`)), icons[0]?.src);
  check("instructions ask for the code", (a.c.getInstructions() ?? "").includes("6-digit code shown in the Asctrl app"));

  const noConn = await a.c.callTool({ name: "observe", arguments: {} });
  check("tools refuse before connect", !!noConn.isError && text(noConn).includes("connect first"), text(noConn));

  const conn = await a.c.callTool({ name: "connect", arguments: { code: CODE } });
  check("connect through the relay", !conn.isError && text(conn).includes("FAKE-PC"), text(conn));
  check("consent card names the client", consentName === "test-agent", consentName);
  const obs = await a.c.callTool({ name: "observe", arguments: { query: "Save" } });
  check("calls reach the computer", text(obs).includes('fake observe {"query":"Save"}'), text(obs));

  // ---- Sessions belong to their key ----
  const hijack = await fetch(`${BASE}/mcp`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${bob.key}`,
      "content-type": "application/json",
      accept: "application/json, text/event-stream",
      "mcp-session-id": a.t.sessionId!,
      "mcp-protocol-version": "2025-06-18",
    },
    body: JSON.stringify({ jsonrpc: "2.0", id: 9, method: "tools/list" }),
  });
  check("another key can't use the session", hijack.status === 403, String(hijack.status));
  const bogus = await fetch(`${BASE}/mcp`, {
    method: "POST",
    headers: { Authorization: `Bearer ${alice.key}`, "content-type": "application/json", accept: "application/json, text/event-stream", "mcp-session-id": "nope" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 9, method: "tools/list" }),
  });
  check("unknown session → 404", bogus.status === 404, String(bogus.status));

  // ---- A client that opens a session per call and ends it (Grok) keeps the computer ----
  await a.t.terminateSession();
  await a.c.close();
  const g = await mcp(alice.key);
  const later = await g.c.callTool({ name: "observe", arguments: { query: "Next" } });
  check("computer still connected in the next session", text(later).includes('fake observe {"query":"Next"}'), text(later));
  await g.t.terminateSession();
  await g.c.close();
  check("ending sessions leaves the connection", srv.links.size === 1 && [...srv.links.values()][0].link.client.connected);
  // Another client with its own sign-in gets its own connection
  const other = await mcp(bob.key);
  const none = await other.c.callTool({ name: "observe", arguments: {} });
  check("another key has no computer until it connects", !!none.isError && text(none).includes("connect first"), text(none));
  await other.c.close();

  // ---- OAuth, the way a connector signs in ----
  const reg = await (
    await fetch(`${BASE}/register`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ client_name: "Claude", redirect_uris: ["https://claude.ai/api/mcp/auth_callback"], token_endpoint_auth_method: "none", grant_types: ["authorization_code", "refresh_token"], response_types: ["code"] }),
    })
  ).json();
  check("dynamic client registration", typeof reg.client_id === "string", JSON.stringify(reg).slice(0, 120));

  const verifier = randomBytes(32).toString("base64url");
  const challenge = createHash("sha256").update(verifier).digest("base64url");
  const authUrl = new URL(`${BASE}/authorize`);
  for (const [k, v] of Object.entries({ response_type: "code", client_id: reg.client_id, redirect_uri: "https://claude.ai/api/mcp/auth_callback", code_challenge: challenge, code_challenge_method: "S256", state: "xyz", resource: `${BASE}/mcp` }))
    authUrl.searchParams.set(k, v);
  const page = await (await fetch(authUrl)).text();
  const reqId = page.match(/name="request" value="([^"]+)"/)?.[1] ?? "";
  check("sign-in page shows who asks and where", page.includes("<b>Claude</b>") && page.includes("claude.ai") && !!reqId);

  const approve = (key: string, action = "allow") =>
    fetch(`${BASE}/oauth/approve`, { method: "POST", redirect: "manual", headers: { "content-type": "application/x-www-form-urlencoded" }, body: new URLSearchParams({ request: reqId, key, action }) });
  const wrong = await approve("asc_wrong");
  check("wrong key re-shows the page", wrong.status === 200 && (await wrong.text()).includes("not valid"));
  const ok = await approve(alice.key);
  const loc = new URL(ok.headers.get("location") ?? "about:blank");
  const code = loc.searchParams.get("code") ?? "";
  check("right key redirects back with a code", ok.status === 302 && loc.host === "claude.ai" && loc.searchParams.get("state") === "xyz" && !!code, loc.href);
  check("sign-in link is single use", (await approve(alice.key)).status === 200);

  const token = (body: Record<string, string>) =>
    fetch(`${BASE}/token`, { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" }, body: new URLSearchParams({ client_id: reg.client_id, ...body }) });
  const badPkce = await token({ grant_type: "authorization_code", code, code_verifier: "x".repeat(43), redirect_uri: "https://claude.ai/api/mcp/auth_callback" });
  check("wrong PKCE verifier refused", badPkce.status === 400);
  const tok = await (await token({ grant_type: "authorization_code", code, code_verifier: verifier, redirect_uri: "https://claude.ai/api/mcp/auth_callback" })).json();
  check("code → tokens", !!tok.access_token && !!tok.refresh_token && tok.expires_in === 3600, Object.keys(tok).join(","));
  const reuse = await token({ grant_type: "authorization_code", code, code_verifier: verifier, redirect_uri: "https://claude.ai/api/mcp/auth_callback" });
  check("code is single use", reuse.status === 400);

  const o = await mcp(tok.access_token, "claude-ai");
  check("OAuth token opens a session", (await o.c.listTools()).tools.length > 10);
  await o.c.close();

  const ref = await (await token({ grant_type: "refresh_token", refresh_token: tok.refresh_token })).json();
  check("refresh gives a new pair", !!ref.access_token && ref.refresh_token !== tok.refresh_token);
  const again = await token({ grant_type: "refresh_token", refresh_token: tok.refresh_token });
  check("old refresh token is spent", again.status === 400);

  // ---- Revoking the key cuts off both ways ----
  store.revokeKey("alice");
  const deadKey = await fetch(`${BASE}/mcp`, { method: "POST", headers: { Authorization: `Bearer ${alice.key}`, "content-type": "application/json" }, body: "{}" });
  const deadTok = await fetch(`${BASE}/mcp`, { method: "POST", headers: { Authorization: `Bearer ${ref.access_token}`, "content-type": "application/json" }, body: "{}" });
  check("revoked key → 401", deadKey.status === 401);
  check("its OAuth tokens → 401", deadTok.status === 401);

  // ---- The stdio server, on the user's own machine, keeps everything ----
  const sc = new Client({ name: "smoke", version: "1.0.0" });
  await sc.connect(new StdioClientTransport({ command: process.execPath, args: [path.resolve(import.meta.dirname, "../src/console/mcp-server.ts")] }));
  const local = (await sc.listTools()).tools.map((t) => t.name);
  check("stdio server keeps file copying and local connect", local.includes("upload_file") && (sc.getInstructions() ?? "").includes("no arguments = this computer"), `${local.length} tools`);
  await sc.close();
} catch (e) {
  console.error(e);
  failed++;
} finally {
  agent.close();
  await srv.close();
  relay.close();
  rmSync(dir, { recursive: true, force: true });
}

console.log(failed ? `\n${failed} failed` : "\nall passed");
process.exit(failed ? 1 : 0);

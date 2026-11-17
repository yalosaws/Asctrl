// The MCP server over Streamable HTTP, for use as a remote connector (claude.ai, Claude Code, the Claude API,
// ChatGPT, Cursor, …). Each MCP session gets its own connection to the relay, so many people can drive their own
// computers through one server; computers are reached by the 6-digit code their app shows.
//
// Run:   node src/console/mcp-http.ts                 (serves PUBLIC_URL/mcp)
// Keys:  node src/console/mcp-http.ts keys add <name> | keys list | keys revoke <name|id>
//
// Env:   PUBLIC_URL  the https address people reach this server at, e.g. https://asctrl.example.com (default http://localhost:PORT)
//        PORT (8788), HOST (127.0.0.1; put a TLS proxy such as Caddy in front), TRUST_PROXY (express setting, default "loopback")
//        RELAY_URL, RELAY_TOKEN  the relay to reach computers through
//        ASCTRL_DATA_DIR  where keys and tokens are kept (default ./data)
//        SESSION_IDLE_MIN (120), SESSIONS_PER_KEY (20), LINK_IDLE_MIN (30)

import { randomUUID } from "node:crypto";
import path from "node:path";
import express, { type Request, type Response } from "express";
import { rateLimit } from "express-rate-limit";
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";
import { mcpAuthRouter, getOAuthProtectedResourceMetadataUrl } from "@modelcontextprotocol/sdk/server/auth/router.js";
import { requireBearerAuth } from "@modelcontextprotocol/sdk/server/auth/middleware/bearerAuth.js";
import { isInitializeRequest } from "@modelcontextprotocol/sdk/types.js";
import { createAsctrlServer, newLink, ICON_DIR, ICON_SIZES, type Link } from "./mcp-server.ts";
import { TechClient } from "./tech-client.ts";
import { AsctrlOAuthProvider, AuthStore, callerOf, defaultStoreFile } from "./auth.ts";
import { DEFAULT_RELAY_URL } from "../shared/protocol.ts";

interface Session {
  transport: StreamableHTTPServerTransport;
  keyId: string;
  /** Which shared link it drives */
  linkKey: string;
  lastSeen: number;
}

/** A computer connection shared by every session of one caller (access key + client) */
interface SharedLink {
  link: Link;
  keyId: string;
  lastUsed: number;
}

export interface HttpOptions {
  port?: number;
  host?: string;
  publicUrl?: string;
  relayUrl?: string;
  relayToken?: string;
  store?: AuthStore;
  idleMs?: number;
  sessionsPerKey?: number;
  /** How long a computer connection no session uses is kept */
  linkIdleMs?: number;
}

export function startHttpServer(opts: HttpOptions = {}) {
  const port = opts.port ?? Number(process.env.PORT ?? 8788);
  const host = opts.host ?? process.env.HOST ?? "127.0.0.1";
  const publicUrl = new URL(opts.publicUrl ?? process.env.PUBLIC_URL ?? `http://localhost:${port}`);
  const mcpUrl = new URL("/mcp", publicUrl);
  const relayUrl = opts.relayUrl ?? process.env.RELAY_URL ?? DEFAULT_RELAY_URL;
  const relayToken = opts.relayToken ?? process.env.RELAY_TOKEN ?? process.env.ASCTRL_RELAY_TOKEN;
  const idleMs = opts.idleMs ?? Number(process.env.SESSION_IDLE_MIN ?? 120) * 60_000;
  const perKey = opts.sessionsPerKey ?? Number(process.env.SESSIONS_PER_KEY ?? 20);
  const linkIdleMs = opts.linkIdleMs ?? Number(process.env.LINK_IDLE_MIN ?? 30) * 60_000;
  const store = opts.store ?? new AuthStore(defaultStoreFile());
  const provider = new AsctrlOAuthProvider(store);

  const sessions = new Map<string, Session>();
  /**
   * Computer connections, by access key and client. They outlive sessions: some clients (Grok) open a session
   * per tool call and close it straight after, which would otherwise drop the computer after every connect.
   */
  const links = new Map<string, SharedLink>();

  function linkFor(keyId: string, clientId: string): [string, SharedLink] {
    const key = `${keyId}|${clientId}`;
    let l = links.get(key);
    if (!l) {
      l = { link: newLink(new TechClient(relayUrl, "AI", relayToken)), keyId, lastUsed: Date.now() };
      links.set(key, l);
    }
    l.lastUsed = Date.now();
    return [key, l];
  }

  function dropLink(key: string) {
    const l = links.get(key);
    if (!l) return;
    links.delete(key);
    l.link.client.disconnect();
  }

  const app = express();
  app.set("trust proxy", process.env.TRUST_PROXY ?? "loopback");
  app.disable("x-powered-by");

  // OAuth: discovery metadata, /authorize, /token, /register, /revoke, and the protected-resource metadata for /mcp
  app.use(mcpAuthRouter({ provider, issuerUrl: publicUrl, resourceServerUrl: mcpUrl, resourceName: "Asctrl" }));

  // The sign-in page posts here; a wrong key re-shows the page, so guessing is rate limited
  app.post(
    provider.approvePath,
    rateLimit({ windowMs: 15 * 60_000, limit: 30, standardHeaders: "draft-7", legacyHeaders: false }),
    express.urlencoded({ extended: false, limit: "8kb" }),
    (req: Request, res: Response) => {
      const b = (req.body ?? {}) as Record<string, string>;
      const r = provider.approve(String(b.request ?? ""), String(b.key ?? ""), b.action === "allow");
      res.set("Cache-Control", "no-store");
      if ("redirect" in r) res.redirect(302, r.redirect);
      else res.status(200).type("html").send(r.page);
    },
  );

  const bearer = requireBearerAuth({ verifier: provider, resourceMetadataUrl: getOAuthProtectedResourceMetadataUrl(mcpUrl) });

  const jsonError = (res: Response, status: number, message: string) =>
    res.status(status).json({ jsonrpc: "2.0", error: { code: -32000, message }, id: null });

  function closeSession(id: string) {
    const s = sessions.get(id);
    if (!s) return;
    sessions.delete(id);
    // The computer stays connected for the caller's next session; the sweep lets it go once unused
    void s.transport.close().catch(() => {});
  }

  async function handle(req: Request, res: Response) {
    const caller = callerOf(req.auth);
    if (!caller) return jsonError(res, 401, "Unauthorized");
    const id = req.header("mcp-session-id");

    if (id) {
      const s = sessions.get(id);
      // Unknown (expired, or the server restarted): 404 tells the client to start a new session
      if (!s) return jsonError(res, 404, "Session not found");
      // A session belongs to whoever opened it
      if (s.keyId !== caller.keyId) return jsonError(res, 403, "This session belongs to someone else");
      s.lastSeen = Date.now();
      const l = links.get(s.linkKey);
      if (l) l.lastUsed = Date.now();
      return s.transport.handleRequest(req, res, req.body);
    }

    if (req.method !== "POST" || !isInitializeRequest(req.body)) return jsonError(res, 400, "Start with an initialize request, without a session id");

    // Keep each key to a bounded number of live sessions: drop its oldest
    const mine = [...sessions.entries()].filter(([, s]) => s.keyId === caller.keyId).sort((a, b) => a[1].lastSeen - b[1].lastSeen);
    for (const [old] of mine.slice(0, Math.max(0, mine.length - perKey + 1))) closeSession(old);

    const [linkKey, shared] = linkFor(caller.keyId, req.auth?.clientId ?? "");
    const { link } = shared;
    const server = createAsctrlServer({ link, hosted: true, iconBaseUrl: publicUrl.href });
    const transport = new StreamableHTTPServerTransport({
      sessionIdGenerator: () => randomUUID(),
      onsessioninitialized: (sid) => {
        sessions.set(sid, { transport, keyId: caller.keyId, linkKey, lastSeen: Date.now() });
      },
    });
    // The consent card on the computer names the app asking; filled in once initialize says who it is
    server.server.oninitialized = () => {
      const v = server.server.getClientVersion();
      const name = v?.title || v?.name;
      if (name && !link.client.connected) link.client.techName = name.slice(0, 48);
    };
    transport.onclose = () => {
      if (transport.sessionId) closeSession(transport.sessionId);
    };
    await server.connect(transport);
    await transport.handleRequest(req, res, req.body);
  }

  const wrap = (req: Request, res: Response) =>
    handle(req, res).catch((e) => {
      console.error("[mcp-http]", e);
      if (!res.headersSent) jsonError(res, 500, "Internal error");
    });

  // Images come back as base64 and set_value forms can be long
  app.post("/mcp", bearer, express.json({ limit: "32mb" }), wrap);
  app.get("/mcp", bearer, wrap);
  app.delete("/mcp", bearer, wrap);

  // The orb: for the server's icons in initialize, and the favicon connector lists fall back on
  const iconFiles = new Set([...ICON_SIZES.map(([f]) => f), "icon.png"]);
  const sendIcon = (file: string) => (_req: Request, res: Response) =>
    res.sendFile(path.join(ICON_DIR, file), { maxAge: "1d" }, (e) => {
      if (e && !res.headersSent) res.sendStatus(404);
    });
  app.get("/favicon.ico", sendIcon("icon.ico"));
  app.get("/apple-touch-icon.png", sendIcon("128x128@2x.png"));
  app.get("/icons/:file", (req: Request, res: Response) => {
    const file = String(req.params.file);
    if (iconFiles.has(file)) sendIcon(file)(req, res);
    else res.sendStatus(404);
  });

  app.get("/", (_req, res) => {
    res.type("html").send(`<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Asctrl</title>
<link rel="icon" href="/favicon.ico" sizes="any"><link rel="icon" type="image/png" href="/icons/32x32.png" sizes="32x32">
<link rel="icon" type="image/png" href="/icons/128x128@2x.png" sizes="256x256"><link rel="apple-touch-icon" href="/apple-touch-icon.png">
</head><body><p>Asctrl MCP server. Add <code>${mcpUrl.href}</code> as a connector.</p></body></html>`);
  });

  const sweep = setInterval(() => {
    const cutoff = Date.now() - idleMs;
    for (const [id, s] of sessions) if (s.lastSeen < cutoff) closeSession(id);
    // A connection is let go once no session uses it and it has sat unused, or its key was revoked
    const inUse = new Set([...sessions.values()].map((s) => s.linkKey));
    const linkCutoff = Date.now() - linkIdleMs;
    const liveKeys = new Set(store.listKeys().map((k) => k.id));
    for (const [key, l] of links) if (!liveKeys.has(l.keyId) || (!inUse.has(key) && l.lastUsed < linkCutoff)) dropLink(key);
  }, 60_000);
  sweep.unref();

  const http = app.listen(port, host, () => {
    log(`listening on http://${host}:${port}, public at ${mcpUrl.href}; relay ${relayUrl}${relayToken ? " (token)" : ""}; keys in ${store.file}`);
  });
  /** Ends every session (and its computer connection), then the listener; open event streams would hold it up */
  const close = () =>
    new Promise<void>((resolve) => {
      clearInterval(sweep);
      for (const id of [...sessions.keys()]) closeSession(id);
      for (const key of [...links.keys()]) dropLink(key);
      http.close(() => resolve());
      http.closeAllConnections();
    });
  return { http, close, sessions, links, store, mcpUrl };
}

function log(s: string) {
  console.log(`[mcp-http ${new Date().toLocaleTimeString()}] ${s}`);
}

function keysCommand(args: string[]) {
  const store = new AuthStore(defaultStoreFile());
  const [cmd, arg] = args;
  if (cmd === "add" && arg) {
    const { id, key } = store.addKey(arg);
    console.log(`Key "${arg}" (id ${id}) created. It is shown only once:\n\n  ${key}\n`);
    console.log("Agents send it as `Authorization: Bearer <key>`; connectors that sign in with OAuth ask for it once.");
  } else if (cmd === "list") {
    const keys = store.listKeys();
    if (!keys.length) console.log("No keys. Create one with: keys add <name>");
    for (const k of keys) console.log(`${k.id}  ${k.name.padEnd(24)} created ${k.created.slice(0, 10)}  ${k.tokens} live OAuth tokens`);
  } else if (cmd === "revoke" && arg) {
    console.log(store.revokeKey(arg) ? `Revoked "${arg}" and its OAuth tokens.` : `No key named or with id "${arg}".`);
  } else {
    console.log("Usage: node src/console/mcp-http.ts keys add <name> | keys list | keys revoke <name|id>");
    process.exitCode = 1;
  }
  console.log(`(store: ${store.file})`);
}

if (import.meta.main) {
  const argv = process.argv.slice(2);
  if (argv[0] === "keys") keysCommand(argv.slice(1));
  else {
    const { close } = startHttpServer();
    const stop = () => void close().then(() => process.exit(0));
    process.on("SIGINT", stop);
    process.on("SIGTERM", stop);
  }
}

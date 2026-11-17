// Who may use the hosted MCP server. One idea underneath both ways in: an access key the operator hands out.
// - Bearer: an agent sends the key itself as `Authorization: Bearer asc_…`.
// - OAuth: a connector (claude.ai, ChatGPT, …) sends the user to /authorize, where they paste the key once;
//   the connector then gets its own short-lived tokens, tied to that key.
// Revoking a key cuts off both at once. Only hashes of keys and tokens are stored.

import { createHash, randomBytes, randomUUID } from "node:crypto";
import { mkdirSync, readFileSync, renameSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import type { Response } from "express";
import type { OAuthServerProvider, AuthorizationParams } from "@modelcontextprotocol/sdk/server/auth/provider.js";
import type { OAuthRegisteredClientsStore } from "@modelcontextprotocol/sdk/server/auth/clients.js";
import type { AuthInfo } from "@modelcontextprotocol/sdk/server/auth/types.js";
import type { OAuthClientInformationFull, OAuthTokenRevocationRequest, OAuthTokens } from "@modelcontextprotocol/sdk/shared/auth.js";
import { InvalidGrantError, InvalidTokenError, InvalidClientMetadataError } from "@modelcontextprotocol/sdk/server/auth/errors.js";

const ACCESS_TTL_SEC = 60 * 60;
const REFRESH_TTL_SEC = 30 * 24 * 60 * 60;
const CODE_TTL_MS = 5 * 60 * 1000;
/** How long the sign-in page stays valid */
const PENDING_TTL_MS = 15 * 60 * 1000;
/** Registration is open to anyone (that is how connectors sign up), so it is capped */
const MAX_CLIENTS = 5000;

const sha256 = (s: string) => createHash("sha256").update(s).digest("hex");
const secret = (prefix: string) => prefix + randomBytes(32).toString("base64url");
const now = () => Math.floor(Date.now() / 1000);

interface KeyRecord {
  id: string;
  name: string;
  created: string;
}

interface TokenRecord {
  kind: "access" | "refresh";
  keyId: string;
  clientId: string;
  scopes: string[];
  /** Seconds since the epoch */
  expiresAt: number;
}

interface StoreData {
  /** By sha256 of the key */
  keys: Record<string, KeyRecord>;
  clients: Record<string, OAuthClientInformationFull>;
  /** By sha256 of the token */
  tokens: Record<string, TokenRecord>;
}

/** The identity a request carries once its token checked out */
export interface Caller {
  keyId: string;
  keyName: string;
}

export const callerOf = (auth: AuthInfo | undefined): Caller | undefined =>
  auth?.extra && typeof auth.extra.keyId === "string" ? { keyId: auth.extra.keyId, keyName: String(auth.extra.keyName ?? "") } : undefined;

/**
 * A JSON file. The keys command edits the same file while the server runs, so it is read again whenever it
 * changed on disk, before anything is looked up or written.
 */
export class AuthStore {
  private data: StoreData = { keys: {}, clients: {}, tokens: {} };
  private mtimeMs = -1;
  readonly file: string;

  constructor(file: string) {
    this.file = file;
    this.refresh();
  }

  private refresh() {
    let m: number;
    try {
      m = statSync(this.file).mtimeMs;
    } catch {
      return;
    }
    if (m === this.mtimeMs) return;
    const v = JSON.parse(readFileSync(this.file, "utf8")) as Partial<StoreData>;
    this.data = { keys: v.keys ?? {}, clients: v.clients ?? {}, tokens: v.tokens ?? {} };
    this.mtimeMs = m;
  }

  private save() {
    const t = now();
    for (const [h, tok] of Object.entries(this.data.tokens)) if (tok.expiresAt <= t) delete this.data.tokens[h];
    mkdirSync(path.dirname(this.file), { recursive: true });
    const tmp = `${this.file}.${process.pid}.tmp`;
    writeFileSync(tmp, JSON.stringify(this.data, null, 2), { mode: 0o600 });
    renameSync(tmp, this.file);
    this.mtimeMs = statSync(this.file).mtimeMs;
  }

  // ---- Access keys ----

  addKey(name: string): { id: string; key: string } {
    this.refresh();
    if (Object.values(this.data.keys).some((k) => k.name === name)) throw new Error(`A key named "${name}" already exists`);
    const key = secret("asc_");
    const id = randomBytes(4).toString("hex");
    this.data.keys[sha256(key)] = { id, name, created: new Date().toISOString() };
    this.save();
    return { id, key };
  }

  listKeys(): (KeyRecord & { tokens: number })[] {
    this.refresh();
    const t = now();
    return Object.values(this.data.keys).map((k) => ({
      ...k,
      tokens: Object.values(this.data.tokens).filter((x) => x.keyId === k.id && x.expiresAt > t).length,
    }));
  }

  /** Removes a key (by name or id) and every OAuth token issued under it */
  revokeKey(nameOrId: string): boolean {
    this.refresh();
    const entry = Object.entries(this.data.keys).find(([, k]) => k.id === nameOrId || k.name === nameOrId);
    if (!entry) return false;
    delete this.data.keys[entry[0]];
    for (const [h, tok] of Object.entries(this.data.tokens)) if (tok.keyId === entry[1].id) delete this.data.tokens[h];
    this.save();
    return true;
  }

  keyByValue(key: string): KeyRecord | undefined {
    this.refresh();
    return this.data.keys[sha256(key.trim())];
  }

  private keyById(id: string): KeyRecord | undefined {
    return Object.values(this.data.keys).find((k) => k.id === id);
  }

  // ---- OAuth clients and tokens ----

  getClient(id: string) {
    this.refresh();
    return this.data.clients[id];
  }

  registerClient(client: OAuthClientInformationFull) {
    this.refresh();
    if (Object.keys(this.data.clients).length >= MAX_CLIENTS) throw new InvalidClientMetadataError("Too many registered clients");
    this.data.clients[client.client_id] = client;
    this.save();
    return client;
  }

  issueTokens(keyId: string, clientId: string, scopes: string[]): OAuthTokens {
    this.refresh();
    const access = secret("asa_");
    const refresh = secret("asr_");
    const t = now();
    this.data.tokens[sha256(access)] = { kind: "access", keyId, clientId, scopes, expiresAt: t + ACCESS_TTL_SEC };
    this.data.tokens[sha256(refresh)] = { kind: "refresh", keyId, clientId, scopes, expiresAt: t + REFRESH_TTL_SEC };
    this.save();
    return { access_token: access, token_type: "Bearer", expires_in: ACCESS_TTL_SEC, refresh_token: refresh, scope: scopes.join(" ") || undefined };
  }

  /** A refresh token is good for one use: it is replaced by the new pair */
  rotate(refreshToken: string, clientId: string): OAuthTokens {
    this.refresh();
    const h = sha256(refreshToken);
    const tok = this.data.tokens[h];
    if (!tok || tok.kind !== "refresh" || tok.clientId !== clientId || tok.expiresAt <= now() || !this.keyById(tok.keyId)) {
      throw new InvalidGrantError("Invalid or expired refresh token");
    }
    delete this.data.tokens[h];
    return this.issueTokens(tok.keyId, tok.clientId, tok.scopes);
  }

  revokeToken(token: string, clientId: string) {
    this.refresh();
    const h = sha256(token);
    if (this.data.tokens[h]?.clientId !== clientId) return;
    delete this.data.tokens[h];
    this.save();
  }

  /** An access key, or an OAuth access token whose key still exists */
  verify(token: string): AuthInfo {
    this.refresh();
    const h = sha256(token);
    const key = this.data.keys[h];
    // Keys don't expire (revoking ends them), but the SDK wants a time on every token: always a day ahead
    if (key) return { token, clientId: `key:${key.id}`, scopes: [], expiresAt: now() + 86_400, extra: { keyId: key.id, keyName: key.name } };
    const tok = this.data.tokens[h];
    const owner = tok && this.keyById(tok.keyId);
    if (!tok || tok.kind !== "access" || tok.expiresAt <= now() || !owner) throw new InvalidTokenError("Invalid or expired token");
    return { token, clientId: tok.clientId, scopes: tok.scopes, expiresAt: tok.expiresAt, extra: { keyId: owner.id, keyName: owner.name } };
  }
}

interface PendingAuthorization {
  client: OAuthClientInformationFull;
  params: AuthorizationParams;
  expires: number;
}

interface IssuedCode {
  clientId: string;
  keyId: string;
  challenge: string;
  redirectUri: string;
  scopes: string[];
  expires: number;
}

const escapeHtml = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

/** The OAuth server, for the SDK's mcpAuthRouter. The sign-in step asks for an access key. */
export class AsctrlOAuthProvider implements OAuthServerProvider {
  /** Sign-in pages shown, by a random id the page posts back (so nothing the browser sends can widen the request) */
  private pending = new Map<string, PendingAuthorization>();
  private codes = new Map<string, IssuedCode>();
  readonly store: AuthStore;
  /** Where the sign-in page posts */
  readonly approvePath: string;

  constructor(store: AuthStore, approvePath = "/oauth/approve") {
    this.store = store;
    this.approvePath = approvePath;
    setInterval(() => this.sweep(), 60_000).unref();
  }

  private sweep() {
    const t = Date.now();
    for (const [k, v] of this.pending) if (v.expires <= t) this.pending.delete(k);
    for (const [k, v] of this.codes) if (v.expires <= t) this.codes.delete(k);
  }

  get clientsStore(): OAuthRegisteredClientsStore {
    return {
      getClient: (id) => this.store.getClient(id),
      // The SDK has already filled in client_id and client_id_issued_at
      registerClient: (client) => this.store.registerClient(client as OAuthClientInformationFull),
    };
  }

  async authorize(client: OAuthClientInformationFull, params: AuthorizationParams, res: Response): Promise<void> {
    const id = randomUUID();
    this.pending.set(id, { client, params, expires: Date.now() + PENDING_TTL_MS });
    res.status(200).type("html").send(this.page(id));
  }

  /** The sign-in page: who is asking, where the answer goes, and a field for the access key */
  page(id: string, error?: string): string {
    const p = this.pending.get(id);
    const who = escapeHtml(p?.client.client_name || "An application");
    let dest = "";
    try {
      dest = escapeHtml(new URL(p?.params.redirectUri ?? "").host);
    } catch {}
    return `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Connect to Asctrl</title><style>
:root{color-scheme:light dark;--bg:#f6f6f4;--card:#fff;--fg:#1c1c1a;--muted:#6b6b66;--line:#d9d9d4;--accent:#2f6fde;--err:#c0392b}
@media (prefers-color-scheme:dark){:root{--bg:#161615;--card:#20201e;--fg:#ececea;--muted:#9a9a94;--line:#3a3a37;--accent:#6c9cf0;--err:#ef7a6c}}
body{margin:0;min-height:100vh;display:grid;place-items:center;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,sans-serif}
main{width:min(420px,100% - 32px);background:var(--card);border:1px solid var(--line);border-radius:12px;padding:28px}
h1{font-size:20px;margin:0 0 8px}p{margin:0 0 16px;color:var(--muted)}b{color:var(--fg)}
input{width:100%;box-sizing:border-box;padding:10px 12px;border:1px solid var(--line);border-radius:8px;background:transparent;color:inherit;font:14px ui-monospace,monospace}
.row{display:flex;gap:8px;margin-top:16px}button{flex:1;padding:10px;border-radius:8px;border:1px solid var(--line);background:transparent;color:inherit;font:inherit;cursor:pointer}
button.primary{background:var(--accent);border-color:var(--accent);color:#fff}.err{color:var(--err);margin:12px 0 0}
</style></head><body><main>
<h1>Connect to Asctrl</h1>
${p ? `<p><b>${who}</b> wants to see and operate computers through Asctrl${dest ? ` and will be signed in at <b>${dest}</b>` : ""}. Each computer still asks its user to allow the connection.</p>
<form method="post" action="${escapeHtml(this.approvePath)}">
<input type="hidden" name="request" value="${escapeHtml(id)}">
<label for="key">Access key</label>
<input id="key" name="key" type="password" autocomplete="off" placeholder="asc_…" autofocus>
${error ? `<p class="err">${escapeHtml(error)}</p>` : ""}
<div class="row"><button name="action" value="deny">Cancel</button><button class="primary" name="action" value="allow">Allow</button></div>
</form>` : `<p class="err">This sign-in link has expired. Start connecting again from your app.</p>`}
</main></body></html>`;
  }

  /**
   * The sign-in page's answer. Returns the URL to send the browser to, or the page again with an error.
   */
  approve(requestId: string, key: string, allow: boolean): { redirect: string } | { page: string } {
    const p = this.pending.get(requestId);
    if (!p || p.expires <= Date.now()) return { page: this.page(requestId) };
    const back = new URL(p.params.redirectUri);
    if (p.params.state !== undefined) back.searchParams.set("state", p.params.state);
    if (!allow) {
      this.pending.delete(requestId);
      back.searchParams.set("error", "access_denied");
      return { redirect: back.href };
    }
    const k = this.store.keyByValue(key);
    if (!k) return { page: this.page(requestId, "That access key is not valid.") };
    this.pending.delete(requestId);
    const code = randomBytes(32).toString("base64url");
    this.codes.set(code, {
      clientId: p.client.client_id,
      keyId: k.id,
      challenge: p.params.codeChallenge,
      redirectUri: p.params.redirectUri,
      scopes: p.params.scopes ?? [],
      expires: Date.now() + CODE_TTL_MS,
    });
    back.searchParams.set("code", code);
    return { redirect: back.href };
  }

  private code(client: OAuthClientInformationFull, code: string): IssuedCode {
    const c = this.codes.get(code);
    if (!c || c.expires <= Date.now() || c.clientId !== client.client_id) throw new InvalidGrantError("Invalid or expired authorization code");
    return c;
  }

  async challengeForAuthorizationCode(client: OAuthClientInformationFull, code: string): Promise<string> {
    return this.code(client, code).challenge;
  }

  async exchangeAuthorizationCode(client: OAuthClientInformationFull, code: string, _verifier?: string, redirectUri?: string): Promise<OAuthTokens> {
    const c = this.code(client, code);
    // Single use, whatever happens next
    this.codes.delete(code);
    if (redirectUri !== undefined && redirectUri !== c.redirectUri) throw new InvalidGrantError("redirect_uri does not match");
    return this.store.issueTokens(c.keyId, c.clientId, c.scopes);
  }

  async exchangeRefreshToken(client: OAuthClientInformationFull, refreshToken: string): Promise<OAuthTokens> {
    return this.store.rotate(refreshToken, client.client_id);
  }

  async verifyAccessToken(token: string): Promise<AuthInfo> {
    return this.store.verify(token);
  }

  async revokeToken(client: OAuthClientInformationFull, request: OAuthTokenRevocationRequest): Promise<void> {
    this.store.revokeToken(request.token, client.client_id);
  }
}

export const defaultStoreFile = () =>
  path.join(process.env.ASCTRL_DATA_DIR ?? path.resolve(import.meta.dirname, "../../data"), "auth.json");

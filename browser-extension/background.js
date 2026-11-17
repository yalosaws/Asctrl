// Asctrl in the person's own browser: Asctrl's driver, running on this computer, works web pages here the
// way it works its own browser, through the DevTools protocol, so it can use the sites the person is signed in
// to without taking the mouse.
//
// The driver talks to this extension through native messaging (no network port): Chrome starts the driver's
// small relay (registered as "com.asctrl.browser"), which passes messages on to the running driver.
// To the driver this looks like a browser's own DevTools connection, cut down to what it uses:
//   Target.getTargets / attachToTarget / createTarget / activateTarget / setDiscoverTargets, and any method on a
//   tab's session ("tab:<id>"), which goes to chrome.debugger. Iframes from other sites run in processes of their
//   own; they are attached as they come (auto-attach) and reached as "tab:<id>:<their session>".
//
// While Asctrl works a tab, the browser shows "Asctrl started debugging this browser". The tab is let go after
// IDLE_MS without work, and the bar goes away. Cancelling the bar stops Asctrl in that tab for REFUSE_MS.

const HOST = "com.asctrl.browser";
const IDLE_MS = 15_000;
const REFUSE_MS = 5 * 60_000;

let port = null;
let retryMs = 2_000;
/** tabId -> last time Asctrl worked it */
const attached = new Map();
/** tabId -> until when the person refused it */
const refused = new Map();

function connect() {
  try {
    port = chrome.runtime.connectNative(HOST);
  } catch (e) {
    port = null;
    setTimeout(connect, retryMs);
    return;
  }
  port.onMessage.addListener((m) => {
    // The driver is there: if it goes, try again soon
    retryMs = 2_000;
    if (m && m.id !== undefined) handle(m);
  });
  port.onDisconnect.addListener(() => {
    port = null;
    // No driver (or no relay installed): try again, less often the longer it stays away
    setTimeout(connect, retryMs);
    retryMs = Math.min(retryMs * 2, 60_000);
  });
  port.postMessage({ hello: { version: chrome.runtime.getManifest().version, agent: navigator.userAgent } });
}

/** "tab:<id>" or "tab:<id>:<child session>" -> the debuggee chrome.debugger takes */
function target(session) {
  const [, id, child] = /^tab:(\d+)(?::(.+))?$/.exec(String(session)) || [];
  return child ? { tabId: Number(id), sessionId: child } : { tabId: Number(id) };
}
const pageLike = (url) => /^(https?|file):|^about:blank/.test(url || "");

async function attach(tabId) {
  const until = refused.get(tabId);
  if (until && until > Date.now()) throw new Error("the person stopped Asctrl in this tab (closed the debugging bar)");
  if (!attached.has(tabId)) {
    await chrome.debugger.attach({ tabId }, "1.3");
    attached.set(tabId, Date.now());
    // Its loads are what an action waits for
    await chrome.debugger.sendCommand({ tabId }, "Page.enable");
    // Iframes from other sites: attached as they come, so their pages can be worked too
    await chrome.debugger.sendCommand({ tabId }, "Target.setAutoAttach", { autoAttach: true, waitForDebuggerOnStart: false, flatten: true }).catch(() => {});
  }
  attached.set(tabId, Date.now());
}

async function handle(m) {
  try {
    let result;
    const p = m.params || {};
    if (m.sessionId) {
      const t = target(m.sessionId);
      await attach(t.tabId);
      result = await chrome.debugger.sendCommand(t, m.method, p);
      attached.set(t.tabId, Date.now());
    } else {
      switch (m.method) {
        case "Target.getTargets": {
          const tabs = await chrome.tabs.query({});
          result = {
            targetInfos: tabs.map((t) => ({
              targetId: String(t.id),
              type: pageLike(t.url || t.pendingUrl) ? "page" : "other",
              title: t.title || "",
              url: t.url || t.pendingUrl || "",
              attached: attached.has(t.id),
              windowId: t.windowId,
              active: t.active,
            })),
          };
          break;
        }
        case "Target.attachToTarget":
          await attach(Number(p.targetId));
          result = { sessionId: `tab:${p.targetId}` };
          break;
        case "Target.createTarget": {
          const t = await chrome.tabs.create({ url: p.url || "about:blank" });
          result = { targetId: String(t.id) };
          break;
        }
        case "Target.activateTarget": {
          const t = await chrome.tabs.update(Number(p.targetId), { active: true });
          await chrome.windows.update(t.windowId, { focused: true });
          result = {};
          break;
        }
        case "Target.setDiscoverTargets":
          result = {};
          break;
        default:
          throw new Error(`${m.method} is not available through the extension`);
      }
    }
    port?.postMessage({ id: m.id, result: result ?? {} });
  } catch (e) {
    port?.postMessage({ id: m.id, error: { message: String((e && e.message) || e) } });
  }
}

chrome.debugger.onEvent.addListener((src, method, params) => {
  if (src.tabId === undefined) return;
  const tab = `tab:${src.tabId}`;
  // A frame attached (or let go): its session, as the driver will name it
  if ((method === "Target.attachedToTarget" || method === "Target.detachedFromTarget") && params && params.sessionId) {
    params = { ...params, sessionId: `${tab}:${params.sessionId}` };
  }
  port?.postMessage({ method, params, sessionId: src.sessionId ? `${tab}:${src.sessionId}` : tab });
});

chrome.debugger.onDetach.addListener((src, reason) => {
  attached.delete(src.tabId);
  if (reason === "canceled_by_user") refused.set(src.tabId, Date.now() + REFUSE_MS);
});

chrome.tabs.onRemoved.addListener((tabId) => {
  attached.delete(tabId);
  refused.delete(tabId);
});

// Let go of tabs Asctrl has stopped working: the debugging bar goes with them
setInterval(() => {
  const now = Date.now();
  for (const [tabId, last] of attached) {
    if (now - last > IDLE_MS) {
      attached.delete(tabId);
      chrome.debugger.detach({ tabId }).catch(() => {});
    }
  }
}, 5_000);

// The service worker starts with the browser (and again whenever it was stopped): connect each time
chrome.runtime.onStartup.addListener(() => {});
connect();

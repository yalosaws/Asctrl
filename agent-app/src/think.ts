// Waiting made visible. The app only sees tool calls, so the quiet stretch between one call ending and
// the next starting is the technician's AI reading the result and deciding: the orb shows it thinking.
// Tools that only wait or scan get the circling arcs of a running command, so no wait is a frozen orb.

import { orb } from "./orb";
import { ripple } from "./light";

/** Quiet this long after the last result before it counts as thinking (back-to-back calls never flicker) */
const QUIET_MS = 450;
/** Right after connecting the AI is still reading its first instructions */
const FIRST_MS = 900;
/** No call this long means the AI has most likely finished its turn: stop thinking */
const THINK_MAX_MS = 60_000;
/** Tools that only wait or scan and draw nothing of their own: they get the circling arcs */
const WAITS = new Set(["wait", "wait_for", "find_text"]);

let live = false;
let running = 0;
let waits = 0;
let quietTimer: number | undefined;
let capTimer: number | undefined;

function clearTimers() {
  window.clearTimeout(quietTimer);
  window.clearTimeout(capTimer);
}

function think() {
  if (!live || running > 0) return;
  orb.setThinking(true);
  window.clearTimeout(capTimer);
  capTimer = window.setTimeout(() => stop(), THINK_MAX_MS);
}

/** `decided`: thinking ended because the next action began, so the orb lands it with a small ripple */
function stop(decided = false) {
  window.clearTimeout(capTimer);
  if (!orb.isThinking) return;
  orb.setThinking(false, decided);
  if (decided) ripple(orb.pos, { size: 46, ms: 520, from: 1.5, to: 0.4 });
}

function quietSoon(ms: number) {
  window.clearTimeout(quietTimer);
  quietTimer = window.setTimeout(think, ms);
}

/** Follow the connection state: thinking only exists inside a live session */
export function setSession(state: string) {
  const on = state === "connected";
  if (on === live) return;
  live = on;
  if (on) {
    if (running === 0) quietSoon(FIRST_MS);
    return;
  }
  clearTimers();
  stop();
  // A permission question pauses the session with the calling tool still running; anything else ends it
  if (state !== "admin" && state !== "reboot") {
    running = 0;
    waits = 0;
    orb.setWaiting(false);
  }
}

export function toolStarted(tool: string) {
  running++;
  window.clearTimeout(quietTimer);
  stop(true);
  if (WAITS.has(tool)) {
    waits++;
    orb.setWaiting(true);
  }
}

export function toolEnded(tool: string) {
  running = Math.max(0, running - 1);
  if (WAITS.has(tool)) {
    waits = Math.max(0, waits - 1);
    if (waits === 0) orb.setWaiting(false);
  }
  if (running === 0 && live) quietSoon(QUIET_MS);
}

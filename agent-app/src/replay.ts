// The replay page (?replay): plays a recorded animation log (ASCTRL_FX_LOG) through the real overlay code, at the
// times it was recorded, and hands back what was drawn (probe.ts) for test/fxcheck.ts to measure.

import { fxEnd, fxPlan, fxStart, fxStep, type FxEvent } from "./fx";
import { bodyState } from "./body";
import { home, initGeo, pt, rect, type OverlayInfo } from "./geo";
import { orb } from "./orb";
import { probe } from "./probe";
import { sleep } from "./motion";

export interface Line {
  t: number;
  e: FxEvent;
}

/** Play `lines` (ms since the log began, event) on screen `info`; `tail` ms more after the last, then the marks */
export async function replay(info: OverlayInfo | null, lines: Line[], tail: number, ready: () => void) {
  initGeo(info);
  ready();
  // A session's orb rests at home between pieces of work: start there, as the recorded work did (once the
  // session's own entrance is over)
  await sleep(1200);
  orb.place(home());
  await sleep(300);
  probe.marks.length = 0;
  probe.t0 = performance.now();
  // Where the orb rests in a session with no work going on: its trips there and back are not hops
  const h = home();
  probe.mark({ k: "home", x: h.x, y: h.y });
  let last = "";
  let sampling = true;
  const sample = () => {
    const b = bodyState();
    const key = `${b.x.toFixed(0)},${b.y.toFixed(0)},${b.w.toFixed(0)},${b.h.toFixed(0)},${b.mode},${b.out},${b.shown}`;
    if (key !== last) {
      last = key;
      probe.mark({ k: "body", x: b.x, y: b.y, w: b.w, h: b.h, mode: b.mode, out: b.out });
    }
    if (sampling) requestAnimationFrame(sample);
  };
  requestAnimationFrame(sample);
  const base = lines.length ? lines[0].t : 0;
  for (const { t, e } of lines) {
    const wait = t - base - (performance.now() - probe.t0);
    if (wait > 0) await sleep(wait);
    const g = e.geo ?? {};
    const at = pt(g.at) ?? (Array.isArray(g.targets) ? pt(g.targets[0]) : null);
    probe.mark({ k: "fx", phase: e.phase, tool: e.tool, id: e.id, at, zone: rect(g.zone) ?? rect(g.rect), win: rect(g.win), app: rect(g.app), gone: e.tool === "zone", ok: e.ok, act: typeof e.args?.action === "string" ? e.args.action : undefined });
    if (e.phase === "plan") fxPlan(e);
    else if (e.phase === "start") fxStart(e);
    else if (e.phase === "step") fxStep(e);
    else fxEnd(e);
  }
  await sleep(tail);
  sampling = false;
  return probe.marks;
}

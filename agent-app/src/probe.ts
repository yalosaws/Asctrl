// What the person would see, written down: only in the replay page (?replay), where test/fxcheck.ts feeds a
// recorded animation log (ASCTRL_FX_LOG) through the real overlay code and measures the drawing. Off, every hook
// here is a single test of `on`.

import type { Pt, R } from "./geo";

export type Mark =
  | { k: "ptr+"; id: number; col: number; x: number; y: number }
  | { k: "ptr-"; id: number }
  | { k: "press"; id: number; x: number; y: number }
  | { k: "frame+"; id: number; r: R; hug: boolean }
  | { k: "frame-"; id: number }
  | { k: "fx"; phase: string; tool: string; id: string; at?: Pt | null; zone?: R | null; win?: R | null; app?: R | null; gone?: boolean; ok?: boolean; act?: string }
  | { k: "home"; x: number; y: number }
  | { k: "body"; x: number; y: number; w: number; h: number; mode: string; out: boolean };

export const probe = {
  on: new URLSearchParams(location.search).has("replay"),
  t0: performance.now(),
  marks: [] as (Mark & { t: number })[],
  mark(m: Mark) {
    if (this.on) this.marks.push({ ...m, t: Math.round(performance.now() - this.t0) });
  },
};

let next = 0;
/** A number for a pointer or a frame, to tell them apart in the marks */
export const probeId = () => ++next;

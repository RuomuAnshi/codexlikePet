// Codex V2 pet atlas contract.
// The spritesheet is a fixed 8-column x 11-row grid of 192x208 cells.

export const CELL_WIDTH = 192;
export const CELL_HEIGHT = 208;
export const COLS = 8;
export const ROWS = 11;

export type AnimationState =
  | "idle"
  | "running-right"
  | "running-left"
  | "waving"
  | "jumping"
  | "failed"
  | "waiting"
  | "running"
  | "review";

// Frame durations (ms) per standard animation row, from
// sakipet-hatch-pet/references/animation-rows.md. Durations describe one loop for the
// *used* columns; trailing cells in a row are transparent and skipped.
export const STATE_TIMING: Record<AnimationState, { row: number; used: number; durations: number[] }> = {
  idle: { row: 0, used: 6, durations: [280, 110, 110, 140, 140, 320] },
  "running-right": { row: 1, used: 8, durations: [120, 120, 120, 120, 120, 120, 120, 220] },
  "running-left": { row: 2, used: 8, durations: [120, 120, 120, 120, 120, 120, 120, 220] },
  waving: { row: 3, used: 4, durations: [140, 140, 140, 280] },
  jumping: { row: 4, used: 5, durations: [140, 140, 140, 140, 280] },
  failed: { row: 5, used: 8, durations: [140, 140, 140, 140, 140, 140, 140, 240] },
  waiting: { row: 6, used: 6, durations: [150, 150, 150, 150, 150, 260] },
  running: { row: 7, used: 6, durations: [120, 120, 120, 120, 120, 220] },
  review: { row: 8, used: 6, durations: [150, 150, 150, 150, 150, 280] },
};

export const STATE_KEYS = Object.keys(STATE_TIMING) as AnimationState[];

export type LookDirection = 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 14 | 15;

// Map a 16-way direction index to (row, col):
// row 9 carries 000..=157.5 (indices 0-7), row 10 carries 180..=337.5 (8-15).
export function lookSlot(direction: LookDirection): { row: number; col: number } {
  return { row: direction < 8 ? 9 : 10, col: direction % 8 };
}

export interface PetManifest {
  id: string;
  displayName: string;
  description: string;
  spriteVersionNumber: number;
  spritesheetPath: string;
}

import { getCurrentWindow } from "@tauri-apps/api/window";

/**
 * Polls the Rust `look_direction` command and reports direction changes.
 * Rust returns null when the cursor is outside the pet's local look area.
 *
 * - `number` 0..=15: the 16-way direction index (0 = up, clockwise).
 * - `null`: cursor is outside the local look area or sits in the deadzone ->
 *   fall back to idle/front.
 *
 * Skipped while the page is hidden or `active()` returns false (paused), so
 * idle pet windows do not keep paying a 10/s IPC toll.
 */
export function watchCursorDirection(
  onDirection: (d: number | null) => void,
  intervalMs = 100,
  active: () => boolean = () => true,
): () => void {
  let last: number | null | undefined = undefined;
  const windowLabel = getCurrentWindow().label;
  const timer = window.setInterval(async () => {
    if (!active() || document.visibilityState === "hidden") return;
    let dir: number | null;
    try {
      // @ts-expect-error Tauri injects window.__TAURI_INTERNALS__ when withGlobalTauri is true
      dir = (await window.__TAURI_INTERNALS__.invoke("look_direction", { windowLabel })) as
        | number
        | null;
    } catch {
      dir = null;
    }
    if (dir !== last) {
      last = dir;
      onDirection(dir);
    }
  }, intervalMs);
  return () => window.clearInterval(timer);
}

import { LogicalPosition } from "@tauri-apps/api/dpi";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

/** Shared pointer state used by hover/look logic and the gesture arbiter. */
export const dragState = { current: false, petting: false };

export type MoveDirection =
  | "left"
  | "right"
  | "up"
  | "down"
  | "up-left"
  | "up-right"
  | "down-left"
  | "down-right";
export type DragDirection = MoveDirection;

/** Pointer release speed in logical px/ms, averaged over a short sample window. */
export interface ReleaseVelocity {
  x: number;
  y: number;
}

const VELOCITY_SAMPLE_WINDOW_MS = 120;
/**
 * Releases slower than this settle in place (legacy drop path); faster ones
 * hand the pet to the throw physics loop. Shared with main.ts so the drag
 * settle invoke and the throw decision can never disagree.
 */
export const THROW_MIN_SPEED = 0.6;

export function isThrowVelocity(velocity: ReleaseVelocity | null): boolean {
  return velocity !== null && Math.hypot(velocity.x, velocity.y) >= THROW_MIN_SPEED;
}

type DragChange = (
  dragging: boolean,
  direction: DragDirection | null,
  carried: boolean,
  releaseVelocity: ReleaseVelocity | null,
) => void;

/** Starts a window drag only after the pointer has moved more than 8px. */
export function attachDrag(
  element: HTMLElement,
  instanceId: string,
  onDragChange?: DragChange,
  canDrag: () => boolean = () => true,
): void {
  const win = getCurrentWindow();
  let active = false;
  let dragging = false;
  let dragToken = 0;
  let pointerId = -1;
  let startPointer = { x: 0, y: 0 };
  let latestPointer = { x: 0, y: 0 };
  let startWin: { x: number; y: number } | null = null;
  let dragDirection: DragDirection | null = null;
  let carried = false;
  let pendingPosition: { x: number; y: number } | null = null;
  let latestPosition: { x: number; y: number } | null = null;
  let flushingPosition = false;
  // Recent pointer samples for release-velocity estimation. A flick lives in
  // the last ~120ms of motion, so a sliding window beats a single-frame
  // delta, which one tremor in the opposite direction would ruin.
  const velocitySamples: { x: number; y: number; t: number }[] = [];
  let speechSyncTimer: number | undefined;
  let speechSyncQueued = false;
  let speechSyncInFlight = false;

  const pushVelocitySample = (x: number, y: number): void => {
    const now = performance.now();
    velocitySamples.push({ x, y, t: now });
    while (velocitySamples.length > 2 && now - velocitySamples[0].t > VELOCITY_SAMPLE_WINDOW_MS) {
      velocitySamples.shift();
    }
  };

  const computeReleaseVelocity = (): ReleaseVelocity | null => {
    if (velocitySamples.length < 2) return null;
    const first = velocitySamples[0];
    const last = velocitySamples[velocitySamples.length - 1];
    const dt = last.t - first.t;
    if (dt < 8) return null;
    return { x: (last.x - first.x) / dt, y: (last.y - first.y) / dt };
  };

  /**
   * Speech windows are separate native windows. Keep them following the pet,
   * but never put their layout work on the high-frequency drag path.
   */
  const queueSpeechSync = (): void => {
    speechSyncQueued = true;
    if (speechSyncTimer !== undefined) return;
    speechSyncTimer = globalThis.setTimeout(() => {
      speechSyncTimer = undefined;
      if (!speechSyncQueued || speechSyncInFlight) return;
      speechSyncQueued = false;
      speechSyncInFlight = true;
      void invoke("sync_pet_speech_position", { instanceId })
        .catch(() => undefined)
        .finally(() => {
          speechSyncInFlight = false;
          if (speechSyncQueued) queueSpeechSync();
        });
    }, 60);
  };

  const flushPosition = async (): Promise<void> => {
    if (flushingPosition) return;
    flushingPosition = true;
    try {
      while (dragging && pendingPosition) {
        const next = pendingPosition;
        pendingPosition = null;
        try {
          // Moving the native window directly keeps pointer feedback smooth.
          // Collision arbitration is performed once after release below;
          // querying every sibling and speech window for every pointermove
          // can stall a transparent WebView while it is being dragged.
          await win.setPosition(new LogicalPosition(next.x, next.y));
          queueSpeechSync();
        } catch {
          // The window may disappear while the pointer is still captured.
        }
      }
    } finally {
      flushingPosition = false;
    }
  };

  const directionFor = (dx: number, dy: number): DragDirection => {
    const horizontalDistance = Math.abs(dx);
    const verticalDistance = Math.abs(dy);
    const diagonal = horizontalDistance > 8 && verticalDistance > 8;
    return diagonal
      ? dy < 0
        ? dx < 0 ? "up-left" : "up-right"
        : dx < 0 ? "down-left" : "down-right"
      : horizontalDistance >= verticalDistance
        ? dx < 0 ? "left" : "right"
        : dy < 0 ? "up" : "down";
  };

  const updateFromPointer = (x: number, y: number): void => {
    if (!active || !startWin || dragState.petting) return;
    const dx = x - startPointer.x;
    const dy = y - startPointer.y;
    const distance = Math.hypot(dx, dy);
    if (!dragging && (!canDrag() || distance <= 8)) return;

    if (!dragging) {
      dragging = true;
      dragState.current = true;
      dragDirection = directionFor(dx, dy);
      carried = dy < -24 && Math.abs(dy) > Math.abs(dx);
      onDragChange?.(true, dragDirection, carried, null);
    }

    pushVelocitySample(x, y);

    pendingPosition = { x: startWin.x + dx, y: startWin.y + dy };
    latestPosition = pendingPosition;
    const nextDirection = directionFor(dx, dy);
    const nextCarried = dy < -24 && Math.abs(dy) > Math.abs(dx);
    if (dragDirection !== nextDirection || carried !== nextCarried) {
      dragDirection = nextDirection;
      carried = nextCarried;
      onDragChange?.(true, nextDirection, nextCarried, null);
    }
    void flushPosition();
  };

  element.addEventListener("pointerdown", async (event) => {
    if (event.button !== 0 || !canDrag()) return;
    const token = ++dragToken;
    active = true;
    dragging = false;
    velocitySamples.length = 0;
    pointerId = event.pointerId;
    startWin = null;
    pendingPosition = null;
    startPointer = { x: event.screenX, y: event.screenY };
    latestPointer = startPointer;
    dragDirection = null;
    carried = false;
    element.setPointerCapture(event.pointerId);

    try {
      const [position, scaleFactor] = await Promise.all([win.outerPosition(), win.scaleFactor()]);
      if (!active || token !== dragToken) return;
      const logicalPosition = position.toLogical(scaleFactor);
      startWin = { x: logicalPosition.x, y: logicalPosition.y };
      updateFromPointer(latestPointer.x, latestPointer.y);
    } catch {
      if (token !== dragToken) return;
      endDrag(event);
    }
  });

  element.addEventListener("pointermove", (event) => {
    if (!active) return;
    latestPointer = { x: event.screenX, y: event.screenY };
    updateFromPointer(latestPointer.x, latestPointer.y);
  });

  const endDrag = (event?: PointerEvent): void => {
    if (!active) return;
    const wasDragging = dragging;
    const releasedPosition = latestPosition;
    const releaseVelocity = wasDragging ? computeReleaseVelocity() : null;
    const throwing = isThrowVelocity(releaseVelocity);
    active = false;
    dragging = false;
    dragToken += 1;
    pendingPosition = null;
    latestPosition = null;
    startWin = null;
    dragDirection = null;
    carried = false;
    velocitySamples.length = 0;
    if (wasDragging || dragState.current) {
      dragState.current = false;
      onDragChange?.(false, null, false, throwing ? releaseVelocity : null);
    }
    try {
      if (event && event.pointerId === pointerId && element.hasPointerCapture(event.pointerId)) {
        element.releasePointerCapture(event.pointerId);
      }
    } catch {
      /* ignore */
    }

    if (wasDragging && releasedPosition && !throwing) {
      // Keep collision protection, but do it after pointer feedback has
      // stopped. A slow layout query must never hold the drag loop hostage.
      // A throw skips this: the physics loop repositions the window and
      // arbitrates once when it settles.
      void invoke<{ x: number; y: number }>("set_pet_position_safely", {
        instanceId,
        x: releasedPosition.x,
        y: releasedPosition.y,
      })
        .then(() => queueSpeechSync())
        .catch(() => queueSpeechSync());
    }
  };

  element.addEventListener("pointerup", endDrag);
  element.addEventListener("pointercancel", endDrag);
  element.addEventListener("lostpointercapture", (event) => {
    endDrag(event);
  });
  window.addEventListener("blur", () => endDrag());
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) endDrag();
  });
}

export type Gesture = "left" | "right" | "petting-start" | "petting-end";

/** Dispatches short clicks, right clicks and a non-moving 350ms petting hold. */
export function attachGestures(element: HTMLElement, onGesture: (gesture: Gesture) => void): void {
  let downAt = 0;
  let pointerId = -1;
  let start = { x: 0, y: 0 };
  let holdTimer: number | undefined;
  let petting = false;

  const clearHold = (): void => {
    if (holdTimer !== undefined) globalThis.clearTimeout(holdTimer);
    holdTimer = undefined;
  };

  element.addEventListener("pointerdown", (event) => {
    if (event.button !== 0) return;
    downAt = performance.now();
    pointerId = event.pointerId;
    start = { x: event.screenX, y: event.screenY };
    clearHold();
    holdTimer = globalThis.setTimeout(() => {
      holdTimer = undefined;
      if (dragState.current) return;
      petting = true;
      dragState.petting = true;
      onGesture("petting-start");
    }, 350);
  });

  element.addEventListener("pointermove", (event) => {
    if (event.pointerId !== pointerId || petting) return;
    if (Math.hypot(event.screenX - start.x, event.screenY - start.y) > 8) clearHold();
  });

  const end = (event?: PointerEvent): void => {
    if (event && event.pointerId !== pointerId) return;
    clearHold();
    if (petting) {
      petting = false;
      dragState.petting = false;
      onGesture("petting-end");
      return;
    }
    if (!event) {
      pointerId = -1;
      return;
    }
    const dt = performance.now() - downAt;
    const moved = Math.hypot(event.screenX - start.x, event.screenY - start.y) > 8;
    if (!moved && dt < 350 && event.button === 0 && !dragState.current) onGesture("left");
    pointerId = -1;
  };

  element.addEventListener("pointerup", end);
  element.addEventListener("pointercancel", end);
  element.addEventListener("lostpointercapture", () => end());
  window.addEventListener("blur", () => end());
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) end();
  });
  element.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    onGesture("right");
  });
}

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

type DragChange = (dragging: boolean, direction: DragDirection | null, carried: boolean) => void;

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
  let flushingPosition = false;

  const flushPosition = async (): Promise<void> => {
    if (flushingPosition) return;
    flushingPosition = true;
    try {
      while (dragging && pendingPosition) {
        const next = pendingPosition;
        pendingPosition = null;
        try {
          await invoke<{ x: number; y: number }>("set_pet_position_safely", {
            // Window labels are not instance ids for secondary pets
            // (`pet-instance-…`). Pass the runtime id explicitly so the
            // backend can arbitrate every pet against the right siblings.
            instanceId,
            x: next.x,
            y: next.y,
          });
        } catch {
          // Fall back to direct movement if the backend is shutting down or
          // this is an older dev binary without the collision command.
          try {
            await win.setPosition(new LogicalPosition(next.x, next.y));
            void invoke("sync_pet_speech_position", { instanceId }).catch(() => undefined);
          } catch {
            // The window may disappear while the pointer is still captured.
          }
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
      onDragChange?.(true, dragDirection, carried);
    }

    pendingPosition = { x: startWin.x + dx, y: startWin.y + dy };
    const nextDirection = directionFor(dx, dy);
    const nextCarried = dy < -24 && Math.abs(dy) > Math.abs(dx);
    if (dragDirection !== nextDirection || carried !== nextCarried) {
      dragDirection = nextDirection;
      carried = nextCarried;
      onDragChange?.(true, nextDirection, nextCarried);
    }
    void flushPosition();
  };

  element.addEventListener("pointerdown", async (event) => {
    if (event.button !== 0 || !canDrag()) return;
    const token = ++dragToken;
    active = true;
    dragging = false;
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

  const endDrag = (event: PointerEvent): void => {
    if (!active) return;
    const wasDragging = dragging;
    active = false;
    dragging = false;
    dragToken += 1;
    pendingPosition = null;
    startWin = null;
    dragDirection = null;
    carried = false;
    if (wasDragging) {
      dragState.current = false;
      onDragChange?.(false, null, false);
    }
    try {
      if (event.pointerId === pointerId) element.releasePointerCapture(event.pointerId);
    } catch {
      /* ignore */
    }
  };

  element.addEventListener("pointerup", endDrag);
  element.addEventListener("pointercancel", endDrag);
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

  const end = (event: PointerEvent): void => {
    if (event.pointerId !== pointerId) return;
    clearHold();
    if (petting) {
      petting = false;
      dragState.petting = false;
      onGesture("petting-end");
      return;
    }
    const dt = performance.now() - downAt;
    const moved = Math.hypot(event.screenX - start.x, event.screenY - start.y) > 8;
    if (!moved && dt < 350 && event.button === 0 && !dragState.current) onGesture("left");
  };

  element.addEventListener("pointerup", end);
  element.addEventListener("pointercancel", end);
  element.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    onGesture("right");
  });
}

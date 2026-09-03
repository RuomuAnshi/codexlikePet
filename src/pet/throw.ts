import { LogicalPosition } from "@tauri-apps/api/dpi";
import { currentMonitor, getCurrentWindow, primaryMonitor } from "@tauri-apps/api/window";
import type { ReleaseVelocity } from "./window";

/**
 * Ballistic simulation for a thrown pet window.
 *
 * The loop mirrors the established drag pattern: move the native window
 * directly every frame for smooth feedback, and leave collision arbitration
 * to a single `set_pet_position_safely` call once the pet settles (owned by
 * the caller's onRest handler). Being JS-side means the user can catch the
 * pet mid-flight on the very pointerdown that starts a new drag.
 *
 * All coordinates are logical pixels; bounds use the same work-area/top-left
 * contract as PetWalker.
 */

const GRAVITY = 3600;
const RESTITUTION = 0.5;
const FLOOR_FRICTION = 0.82;
const SLIDE_DECELERATION = 1400;
const MAX_BOUNCES = 3;
const BOUNCE_MIN_SPEED = 260;
const WALL_MIN_SPEED = 60;
const REST_SLIDE_SPEED = 40;
const MAX_SPEED = 3000;
const MAX_DURATION_MS = 2500;
/** Mirrors the dt cap in PetEngine so an IPC stall cannot teleport the pet. */
const MAX_DT_S = 0.12;
const MAX_LEAN_DEG = 14;
const LEAN_SPEED = 2400;

export interface ThrowFinalPosition {
  x: number;
  y: number;
}

export interface ThrowCallbacks {
  /** Suggested sprite lean in degrees, derived from horizontal velocity. */
  onFrame?: (lean: number) => void;
  onBounce?: (surface: "floor" | "wall") => void;
  onRest: (final: ThrowFinalPosition) => void;
  onCaught?: () => void;
}

function clamp(value: number, limit: number): number {
  return Math.min(limit, Math.max(-limit, value));
}

export class ThrowPhysics {
  private flying = false;
  private rafId = 0;
  private callbacks: ThrowCallbacks | null = null;

  /**
   * Catching must win over attachDrag's own pointerdown handler. Both listen
   * on the same element, where capture/bubble ordering collapses to
   * registration order, so this listener is installed on `document` in the
   * capture phase to run before any target-phase listener.
   */
  private readonly onPointerDown = (event: PointerEvent): void => {
    if (!this.flying) return;
    if (!this.element.contains(event.target as Node)) return;
    this.stop();
    const callbacks = this.callbacks;
    this.callbacks = null;
    callbacks?.onCaught?.();
  };

  constructor(
    private readonly element: HTMLElement,
  ) {
    document.addEventListener("pointerdown", this.onPointerDown, true);
  }

  get isFlying(): boolean {
    return this.flying;
  }

  launch(velocity: ReleaseVelocity, callbacks: ThrowCallbacks): void {
    if (this.flying) return;
    this.callbacks = callbacks;
    void this.run(velocity);
  }

  private stop(): void {
    this.flying = false;
    if (this.rafId) cancelAnimationFrame(this.rafId);
    this.rafId = 0;
  }

  private async run(velocity: ReleaseVelocity): Promise<void> {
    this.flying = true;
    const win = getCurrentWindow();
    const reducedMotion =
      globalThis.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
    try {
      const monitor = (await currentMonitor()) ?? (await primaryMonitor());
      if (!monitor) throw new Error("no monitor available");
      const [position, size, scaleFactor] = await Promise.all([
        win.outerPosition(),
        win.outerSize(),
        win.scaleFactor(),
      ]);
      if (!this.flying) return;
      const logical = position.toLogical(scaleFactor);
      const logicalSize = size.toLogical(scaleFactor);
      const workPos = monitor.workArea.position.toLogical(monitor.scaleFactor);
      const workSize = monitor.workArea.size.toLogical(monitor.scaleFactor);
      const bounds = {
        minX: workPos.x,
        maxX: Math.max(workPos.x, workPos.x + workSize.width - logicalSize.width),
        minY: workPos.y,
        maxY: Math.max(workPos.y, workPos.y + workSize.height - logicalSize.height),
      };

      let x = clamp(logical.x - bounds.minX, bounds.maxX - bounds.minX) + bounds.minX;
      let y = clamp(logical.y - bounds.minY, bounds.maxY - bounds.minY) + bounds.minY;
      let vx = clamp(velocity.x * 1000, MAX_SPEED);
      let vy = clamp(velocity.y * 1000, MAX_SPEED);
      let bounces = 0;
      let sliding = false;
      const startedAt = performance.now();
      let lastTick = startedAt;

      const settle = (): void => {
        this.stop();
        const callbacks = this.callbacks;
        this.callbacks = null;
        callbacks?.onRest({ x, y });
      };

      const step = (now: number): void => {
        if (!this.flying) return;
        const dt = Math.min((now - lastTick) / 1000, MAX_DT_S);
        lastTick = now;
        vy += GRAVITY * dt;
        x += vx * dt;
        y += vy * dt;

        let bounced: "floor" | "wall" | null = null;

        if (x <= bounds.minX) {
          x = bounds.minX;
          if (vx < 0) {
            vx = Math.abs(vx) < WALL_MIN_SPEED ? 0 : -vx * RESTITUTION;
            bounced = "wall";
          }
        } else if (x >= bounds.maxX) {
          x = bounds.maxX;
          if (vx > 0) {
            vx = Math.abs(vx) < WALL_MIN_SPEED ? 0 : -vx * RESTITUTION;
            bounced = "wall";
          }
        }
        if (y <= bounds.minY) {
          y = bounds.minY;
          if (vy < 0) {
            vy = Math.abs(vy) < WALL_MIN_SPEED ? 0 : -vy * RESTITUTION;
            bounced = "wall";
          }
        }
        if (y >= bounds.maxY) {
          y = bounds.maxY;
          if (vy > 0) {
            if (reducedMotion) {
              // Reduced motion settles on first floor contact.
              vy = 0;
              vx = 0;
              sliding = true;
            } else if (bounces >= MAX_BOUNCES || vy < BOUNCE_MIN_SPEED) {
              vy = 0;
              sliding = true;
            } else {
              vy = -vy * RESTITUTION;
              vx *= FLOOR_FRICTION;
              bounces += 1;
              bounced = "floor";
            }
          }
        }

        if (sliding && vx !== 0) {
          const decay = SLIDE_DECELERATION * dt;
          if (Math.abs(vx) <= REST_SLIDE_SPEED + decay) vx = 0;
          else vx -= Math.sign(vx) * decay;
        }

        this.callbacks?.onFrame?.(clamp((vx / LEAN_SPEED) * MAX_LEAN_DEG, MAX_LEAN_DEG));
        if (bounced) this.callbacks?.onBounce?.(bounced);

        // Move the native window directly, exactly like the drag loop does.
        // Per-frame arbitration would stall a transparent WebView; the caller
        // arbitrates once through onRest instead.
        void win
          .setPosition(new LogicalPosition(x, y))
          .catch(() => undefined);

        if (vx === 0 && vy === 0 && y >= bounds.maxY) {
          settle();
          return;
        }
        if (now - startedAt >= MAX_DURATION_MS) {
          settle();
          return;
        }
        this.rafId = requestAnimationFrame(step);
      };

      // A gentle release can have the pet already resting on the floor.
      if (y >= bounds.maxY && Math.abs(vy) < BOUNCE_MIN_SPEED && reducedMotion) {
        settle();
        return;
      }
      this.rafId = requestAnimationFrame(step);
    } catch (error) {
      console.warn("throw physics aborted:", error);
      if (this.flying) {
        // Cannot continue without bounds; report the last known position so
        // the caller can still arbitrate and save.
        try {
          const [position, scaleFactor] = await Promise.all([
            win.outerPosition(),
            win.scaleFactor(),
          ]);
          const logical = position.toLogical(scaleFactor);
          this.stop();
          const callbacks = this.callbacks;
          this.callbacks = null;
          callbacks?.onRest({ x: logical.x, y: logical.y });
        } catch {
          this.stop();
          this.callbacks = null;
        }
      }
    }
  }
}

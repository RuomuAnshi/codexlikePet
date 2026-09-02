import { LogicalPosition } from "@tauri-apps/api/dpi";
import { invoke } from "@tauri-apps/api/core";
import { currentMonitor, getCurrentWindow, primaryMonitor } from "@tauri-apps/api/window";
import {
  isCollisionFree,
  planCollisionFreeRoute,
  type CollisionBounds,
  type CollisionPoint,
  type CollisionRect,
} from "./collision";
import { dragState, type MoveDirection } from "./window";

const WALK_DELAY_MIN = 30000;
const WALK_DELAY_MAX = 60000;
const WALK_MIN_DISTANCE = 160;
const WALK_TICK_MS = 50;
const OCCUPANCY_REFRESH_MS = 200;
const COLLISION_GAP = 12;
const STUCK_ABORT_MS = 1800;

interface WalkTarget {
  x: number;
  y: number;
}

interface PetOccupancy {
  instanceId: string;
  petId: string;
  monitorKey: string;
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Moves the pet occasionally while leaving long, quiet idle periods. */
export class PetWalker {
  private readonly window = getCurrentWindow();
  private timer: number | null = null;
  private walkToken = 0;
  private walking = false;
  private speed = 95;
  private enabled = true;
  private quietMode = false;
  private forcedTarget: WalkTarget | null = null;
  private occupancy: PetOccupancy[] = [];
  private occupancyStaleAt = 0;
  private width = 0;
  private height = 0;
  private monitorKey = "";

  constructor(
    private readonly instanceId: string,
    private readonly onChange: (walking: boolean, direction: MoveDirection | null) => void,
  ) {}

  get isWalking(): boolean {
    return this.walking;
  }

  setSettings(speed: number, enabled: boolean, quietMode: boolean): void {
    this.speed = speed;
    this.enabled = enabled;
    this.quietMode = quietMode;
    if (!enabled || quietMode) this.stop();
    else if (this.timer === null && !this.walking) this.schedule();
  }

  start(): void {
    if (!this.enabled || this.quietMode || this.timer !== null || this.walking) return;
    this.schedule();
  }

  /** Start one autonomous walk immediately, for an AI behavior decision. */
  walkNow(): void {
    if (!this.enabled || this.quietMode || this.walking || dragState.current) return;
    this.walkToken += 1;
    if (this.timer !== null) {
      window.clearTimeout(this.timer);
      this.timer = null;
    }
    void this.walk();
  }

  /** Walk to a target chosen by a social desktop event. */
  walkTo(x: number, y: number): void {
    if (!this.enabled || this.quietMode || dragState.current) return;
    this.walkToken += 1;
    if (this.timer !== null) {
      window.clearTimeout(this.timer);
      this.timer = null;
    }
    this.forcedTarget = { x, y };
    if (this.walking) {
      this.walking = false;
      this.onChange(false, null);
    }
    void this.walk();
  }

  stop(): void {
    this.walkToken += 1;
    this.forcedTarget = null;
    if (this.timer !== null) {
      window.clearTimeout(this.timer);
      this.timer = null;
    }
    if (this.walking) {
      this.walking = false;
      this.onChange(false, null);
    }
  }

  private schedule(): void {
    if (!this.enabled || this.quietMode) return;
    const delay = WALK_DELAY_MIN + Math.random() * (WALK_DELAY_MAX - WALK_DELAY_MIN);
    this.timer = window.setTimeout(() => {
      this.timer = null;
      void this.walk();
    }, delay);
  }

  private async walk(): Promise<void> {
    if (!this.enabled || this.quietMode || dragState.current) {
      this.schedule();
      return;
    }

    const token = this.walkToken;
    try {
      const monitor = (await currentMonitor()) ?? (await primaryMonitor());
      if (!monitor || token !== this.walkToken || dragState.current) {
        if (token === this.walkToken) this.schedule();
        return;
      }

      const [position, windowSize, scaleFactor] = await Promise.all([
        this.window.outerPosition(),
        this.window.outerSize(),
        this.window.scaleFactor(),
      ]);
      if (token !== this.walkToken || dragState.current) return;

      const workAreaPosition = monitor.workArea.position.toLogical(monitor.scaleFactor);
      const workAreaSize = monitor.workArea.size.toLogical(monitor.scaleFactor);
      const currentPosition = position.toLogical(scaleFactor);
      const currentSize = windowSize.toLogical(scaleFactor);
      this.monitorKey = `${monitor.position.x}:${monitor.position.y}`;
      this.width = currentSize.width;
      this.height = currentSize.height;
      const bounds: CollisionBounds = {
        minX: workAreaPosition.x,
        maxX: Math.max(workAreaPosition.x, workAreaPosition.x + workAreaSize.width - currentSize.width),
        minY: workAreaPosition.y,
        maxY: Math.max(workAreaPosition.y, workAreaPosition.y + workAreaSize.height - currentSize.height),
      };
      const currentX = Math.min(bounds.maxX, Math.max(bounds.minX, currentPosition.x));
      const currentY = Math.min(bounds.maxY, Math.max(bounds.minY, currentPosition.y));
      const forcedTarget = this.forcedTarget;
      this.forcedTarget = null;
      let targetX: number;
      let targetY: number;
      if (forcedTarget) {
        targetX = Math.min(bounds.maxX, Math.max(bounds.minX, forcedTarget.x));
        targetY = Math.min(bounds.maxY, Math.max(bounds.minY, forcedTarget.y));
      } else {
        const movementRoll = Math.random();
        const diagonalUp = movementRoll < 0.22;
        const vertical = !diagonalUp && movementRoll < 0.40;
        targetX = vertical ? currentX : this.pickTarget(currentX, bounds.minX, bounds.maxX);
        targetY = diagonalUp
          ? this.pickUpperTarget(currentY, bounds.minY)
          : vertical
            ? this.pickTarget(currentY, bounds.minY, bounds.maxY)
            : currentY;
      }
      const distance = Math.hypot(targetX - currentX, targetY - currentY);
      if (distance < 1) {
        this.schedule();
        return;
      }
      const direction = this.directionFor(currentX, currentY, targetX, targetY);
      const duration = Math.max(3500, Math.min(14000, (distance / this.speed) * 1000));

      this.walking = true;
      this.onChange(true, direction);
      await this.move(token, currentX, currentY, targetX, targetY, duration, bounds);
    } catch (error) {
      console.warn("autonomous pet walk stopped:", error);
      this.finish(token);
    }
  }

  private pickTarget(current: number, minBound: number, maxBound: number): number {
    const padding = Math.min(24, Math.max(0, (maxBound - minBound) / 2));
    const min = minBound + padding;
    const max = maxBound - padding;
    if (max - min < WALK_MIN_DISTANCE) return current;

    let target = min + Math.random() * (max - min);
    if (Math.abs(target - current) < WALK_MIN_DISTANCE) {
      target = current < (min + max) / 2 ? max : min;
    }
    return target;
  }

  private pickUpperTarget(current: number, minBound: number): number {
    const max = current - WALK_MIN_DISTANCE;
    if (max < minBound) return current;
    return minBound + Math.random() * (max - minBound);
  }

  private directionFor(startX: number, startY: number, targetX: number, targetY: number): MoveDirection {
    const horizontal = targetX - startX;
    const vertical = targetY - startY;
    if (Math.abs(horizontal) < 1) return vertical < 0 ? "up" : "down";
    if (Math.abs(vertical) < 1) return horizontal < 0 ? "left" : "right";
    if (vertical < 0) return horizontal < 0 ? "up-left" : "up-right";
    return horizontal < 0 ? "down-left" : "down-right";
  }

  private async refreshOccupancy(): Promise<void> {
    if (performance.now() < this.occupancyStaleAt) return;
    this.occupancyStaleAt = performance.now() + OCCUPANCY_REFRESH_MS;
    try {
      this.occupancy = await invoke<PetOccupancy[]>("get_pet_occupancies", {
        instanceId: this.instanceId,
      });
      this.occupancy = this.occupancy.filter((other) => other.monitorKey === this.monitorKey);
    } catch {
      this.occupancyStaleAt = 0;
    }
  }

  private async move(
    token: number,
    startX: number,
    startY: number,
    targetX: number,
    targetY: number,
    duration: number,
    bounds: CollisionBounds,
  ): Promise<void> {
    const startedAt = performance.now();
    let lastTick = startedAt;
    let stuckSince: number | null = null;
    let lastDirection: MoveDirection | null = null;
    const maxDuration = Math.max(duration + 7_000, duration * 1.8);
    const ownSize = { width: this.width, height: this.height };

    while (token === this.walkToken && !dragState.current) {
      const elapsed = performance.now() - startedAt;
      if (elapsed >= maxDuration) break;
      await this.refreshOccupancy();
      const current: CollisionPoint = {
        x: startX,
        y: startY,
      };
      // `startX/startY` become the last committed position below.  Keeping
      // the current point explicit makes every re-plan use the real top-left
      // of this window, not the centre or a stale target interpolation.
      const obstacles: CollisionRect[] = this.occupancy;
      const route = planCollisionFreeRoute(
        current,
        { x: targetX, y: targetY },
        ownSize,
        bounds,
        obstacles,
        COLLISION_GAP,
      );
      const waypoint = route[0];
      if (!waypoint) {
        if (stuckSince === null) stuckSince = elapsed;
        if (elapsed - stuckSince >= STUCK_ABORT_MS) break;
        await new Promise<void>((resolve) => window.setTimeout(resolve, WALK_TICK_MS));
        continue;
      }

      const routeEnd = route[route.length - 1];
      if (Math.hypot(current.x - routeEnd.x, current.y - routeEnd.y) <= 4) break;

      const now = performance.now();
      const seconds = Math.min(0.2, Math.max(0.01, (now - lastTick) / 1000));
      lastTick = now;
      const dx = waypoint.x - current.x;
      const dy = waypoint.y - current.y;
      const stepDistance = Math.min(Math.hypot(dx, dy), this.speed * seconds);
      const length = Math.hypot(dx, dy) || 1;
      const candidate = {
        x: Math.min(bounds.maxX, Math.max(bounds.minX, current.x + (dx / length) * stepDistance)),
        y: Math.min(bounds.maxY, Math.max(bounds.minY, current.y + (dy / length) * stepDistance)),
      };

      if (!isCollisionFree(candidate, ownSize, obstacles, COLLISION_GAP)) {
        if (stuckSince === null) stuckSince = elapsed;
        if (elapsed - stuckSince >= STUCK_ABORT_MS) break;
      } else {
        stuckSince = null;
        const direction = this.directionFor(current.x, current.y, waypoint.x, waypoint.y);
        if (direction !== lastDirection) {
          lastDirection = direction;
          this.onChange(true, direction);
        }
        try {
          const committed = await invoke<CollisionPoint>("set_pet_position_safely", {
            instanceId: this.instanceId,
            x: candidate.x,
            y: candidate.y,
          });
          startX = committed.x;
          startY = committed.y;
        } catch {
          // Keep dev mode usable while the backend is restarting. The normal
          // path is always arbitrated by Rust before the window is moved.
          await this.window.setPosition(new LogicalPosition(candidate.x, candidate.y));
          startX = candidate.x;
          startY = candidate.y;
        }
      }

      await new Promise<void>((resolve) => window.setTimeout(resolve, WALK_TICK_MS));
    }
    this.occupancy = [];
    this.finish(token);
  }

  private finish(token: number): void {
    if (token !== this.walkToken) return;
    if (this.walking) {
      this.walking = false;
      this.onChange(false, null);
    }
    this.schedule();
  }
}

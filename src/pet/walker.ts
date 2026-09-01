import { LogicalPosition } from "@tauri-apps/api/dpi";
import { invoke } from "@tauri-apps/api/core";
import { currentMonitor, getCurrentWindow, primaryMonitor } from "@tauri-apps/api/window";
import { dragState, type MoveDirection } from "./window";

const WALK_DELAY_MIN = 30000;
const WALK_DELAY_MAX = 60000;
const WALK_MIN_DISTANCE = 160;
const WALK_TICK_MS = 50;
const WALK_WOBBLE_COUNT = 3.5;
const OCCUPANCY_REFRESH_MS = 200;
const OCCUPANCY_MARGIN = 6;
const STUCK_ABORT_MS = 1600;

interface WalkBounds {
  minX: number;
  maxX: number;
  minY: number;
  maxY: number;
}

interface WalkTarget {
  x: number;
  y: number;
}

interface PetOccupancy {
  instanceId: string;
  petId: string;
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

  constructor(
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
      this.width = currentSize.width;
      this.height = currentSize.height;
      const bounds: WalkBounds = {
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
      await this.move(token, currentX, currentY, targetX, targetY, duration, bounds, forcedTarget === null);
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
        instanceId: this.window.label,
      });
    } catch {
      this.occupancyStaleAt = 0;
    }
  }

  private overlapsAny(x: number, y: number): boolean {
    for (const other of this.occupancy) {
      const margin = OCCUPANCY_MARGIN;
      const overlapsX =
        x < other.x + other.width + margin && x + this.width + margin > other.x;
      const overlapsY = y < other.y + other.height + margin && y + this.height + margin > other.y;
      if (overlapsX && overlapsY) return true;
    }
    return false;
  }

  private async move(
    token: number,
    startX: number,
    startY: number,
    targetX: number,
    targetY: number,
    duration: number,
    bounds: WalkBounds,
    autonomous: boolean,
  ): Promise<void> {
    const dx = targetX - startX;
    const dy = targetY - startY;
    const distance = Math.hypot(dx, dy) || 1;
    const perpX = -dy / distance;
    const perpY = dx / distance;
    const amplitude = Math.min(16, distance * 0.12);
    const startedAt = performance.now();
    let stuckSince: number | null = null;
    await this.refreshOccupancy();
    while (token === this.walkToken && !dragState.current) {
      const elapsed = performance.now() - startedAt;
      const progress = Math.min(1, elapsed / duration);
      const baseX = startX + dx * progress;
      const baseY = startY + dy * progress;
      const wobble = Math.sin(progress * Math.PI * 2 * WALK_WOBBLE_COUNT) * amplitude;
      const x = Math.min(bounds.maxX, Math.max(bounds.minX, baseX + perpX * wobble));
      const y = Math.min(bounds.maxY, Math.max(bounds.minY, baseY + perpY * wobble));

      await this.refreshOccupancy();
      if (this.overlapsAny(x, y)) {
        // A sibling pet is in the way: hold still instead of passing through.
        if (stuckSince === null) stuckSince = elapsed;
        if (autonomous && elapsed - stuckSince >= STUCK_ABORT_MS) break;
      } else {
        stuckSince = null;
        await this.window.setPosition(new LogicalPosition(x, y));
      }
      if (progress >= 1) break;
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

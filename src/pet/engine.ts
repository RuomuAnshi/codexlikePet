import {
  CELL_WIDTH,
  STATE_TIMING,
  type AnimationState,
  type LookDirection,
} from "./atlas";
import { drawLookCell, drawStateFrame } from "./loader";

/**
 * PetEngine drives a single-output canvas from a Codex v2 sprite atlas.
 *
 * - Regular states loop their row with per-frame durations.
 * - A non-null `lookDirection` takes precedence and renders a static
 *   look-direction cell (rows 9/10). Setting it back to null resumes the
 *   state animation.
 * - `playOnce` runs a gesture animation (e.g. jumping) for exactly one loop
 *   and then returns to `idle`.
 */
export class PetEngine {
  private source: HTMLCanvasElement;
  private readonly target: CanvasRenderingContext2D;
  private scale: number;

  private state: AnimationState = "idle";
  private stateFrame = 0;
  private stateElapsed = 0;
  private lastTick = performance.now();
  private pausedLookFrames = false;
  private actionComplete: (() => void) | null = null;

  private look: LookDirection | null = null;

  private playing = false;
  private rafId = 0;

  constructor(source: HTMLCanvasElement, target: HTMLCanvasElement, scale = 2) {
    this.source = source;
    this.target = target.getContext("2d")!;
    this.scale = scale;
  }

  setState(state: AnimationState): void {
    if (this.state !== state) {
      this.state = state;
      this.stateFrame = 0;
      this.stateElapsed = 0;
    }
  }

  setSource(source: HTMLCanvasElement): void {
    this.source = source;
    this.state = "idle";
    this.stateFrame = 0;
    this.stateElapsed = 0;
    this.pausedLookFrames = false;
    this.actionComplete = null;
    this.target.clearRect(0, 0, this.target.canvas.width, this.target.canvas.height);
  }

  setScale(scale: number): void {
    this.scale = scale;
    this.target.clearRect(0, 0, this.target.canvas.width, this.target.canvas.height);
  }

  getState(): AnimationState {
    return this.state;
  }

  setLook(direction: LookDirection | null): void {
    this.look = direction;
  }

  getLook(): LookDirection | null {
    return this.look;
  }

  /**
   * Play a state once through its full loop, then settle on idle.
   * Gesture animations (jumping, failed, etc.) free the look-chasing so the
   * user can see the whole gesture near the pet.
   */
  playOnce(state: AnimationState, onComplete?: () => void): void {
    if (this.pausedLookFrames) return;
    this.state = state;
    this.stateFrame = 0;
    this.stateElapsed = 0;
    this.pausedLookFrames = true;
    this.actionComplete = onComplete ?? null;
  }

  cancelAction(): void {
    this.pausedLookFrames = false;
    this.actionComplete = null;
    this.state = "idle";
    this.stateFrame = 0;
    this.stateElapsed = 0;
  }

  play(active: boolean): void {
    if (active === this.playing) {
      // A paused pet must still render a static frame; otherwise the
      // transparent window shows nothing at all.
      if (!active) this.drawStaticFrame();
      return;
    }
    this.playing = active;
    if (active) {
      this.lastTick = performance.now();
      this.rafId = requestAnimationFrame(this.tick);
    } else {
      cancelAnimationFrame(this.rafId);
      this.drawStaticFrame();
    }
  }

  private drawStaticFrame(): void {
    const x = (this.target.canvas.width - CELL_WIDTH * this.scale) / 2;
    if (this.look !== null && !this.pausedLookFrames) {
      drawLookCell(this.source, this.target, this.look, this.scale, x, 0);
    } else {
      drawStateFrame(this.source, this.target, this.state, this.stateFrame, this.scale, x, 0);
    }
  }

  private readonly tick = (now: number): void => {
    const dt = now - this.lastTick;
    this.lastTick = now;

    if (this.look === null || this.pausedLookFrames) {
      // Advance the looping state animation using per-frame durations.
      const spec = STATE_TIMING[this.state];
      this.stateElapsed += dt;
      while (this.stateFrame < spec.durations.length && this.stateElapsed >= spec.durations[this.stateFrame]) {
        this.stateElapsed -= spec.durations[this.stateFrame];
        const next = this.stateFrame + 1;
        this.stateFrame = next % spec.used;
        // A gesture that just finished its loop settles back to idle.
        if (this.pausedLookFrames && next >= spec.used) {
          const onComplete = this.actionComplete;
          this.actionComplete = null;
          this.pausedLookFrames = false;
          this.state = "idle";
          this.stateFrame = 0;
          this.stateElapsed = 0;
          onComplete?.();
          break;
        }
      }
      drawStateFrame(
        this.source,
        this.target,
        this.state,
        this.stateFrame,
        this.scale,
        (this.target.canvas.width - CELL_WIDTH * this.scale) / 2,
        0,
      );
    } else {
      // Static look-direction pose (no frame advancing).
      drawLookCell(
        this.source,
        this.target,
        this.look,
        this.scale,
        (this.target.canvas.width - CELL_WIDTH * this.scale) / 2,
        0,
      );
    }

    if (this.playing) {
      this.rafId = requestAnimationFrame(this.tick);
    }
  };
}

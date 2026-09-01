import {
  CELL_HEIGHT,
  CELL_WIDTH,
  STATE_TIMING,
  type AnimationState,
  type LookDirection,
} from "./atlas";
import { drawLookCell, drawStateFrame } from "./loader";
import type { LoadedAnimationClip } from "./loader";

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

  private clips = new Map<string, LoadedAnimationClip>();
  private activeClip: LoadedAnimationClip | null = null;
  private clipFrame = 0;
  private clipElapsed = 0;
  private clipComplete: (() => void) | null = null;

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
    this.activeClip = null;
    this.clipComplete = null;
    this.clipFrame = 0;
    this.clipElapsed = 0;
    this.target.clearRect(0, 0, this.target.canvas.width, this.target.canvas.height);
  }

  setScale(scale: number): void {
    this.scale = scale;
    this.target.clearRect(0, 0, this.target.canvas.width, this.target.canvas.height);
  }

  setAnimationClips(clips: Map<string, LoadedAnimationClip>): void {
    this.clips = clips;
    if (this.activeClip && ![...clips.values()].includes(this.activeClip)) this.cancelClip();
  }

  hasAnimationClip(id: string): boolean {
    return this.clips.has(id);
  }

  playClip(id: string, onComplete?: () => void): boolean {
    const clip = this.clips.get(id);
    if (!clip || this.activeClip !== null || this.pausedLookFrames) return false;
    this.activeClip = clip;
    this.clipFrame = 0;
    this.clipElapsed = 0;
    this.clipComplete = onComplete ?? null;
    return true;
  }

  cancelClip(): void {
    this.activeClip = null;
    this.clipComplete = null;
    this.clipFrame = 0;
    this.clipElapsed = 0;
  }

  isPlayingClip(): boolean {
    return this.activeClip !== null;
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
    if (this.pausedLookFrames || this.activeClip) return;
    this.state = state;
    this.stateFrame = 0;
    this.stateElapsed = 0;
    this.pausedLookFrames = true;
    this.actionComplete = onComplete ?? null;
  }

  cancelAction(): void {
    this.pausedLookFrames = false;
    this.actionComplete = null;
    this.cancelClip();
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
    if (this.activeClip) {
      this.drawClipFrame();
      return;
    }
    const x = (this.target.canvas.width - CELL_WIDTH * this.scale) / 2;
    if (this.look !== null && !this.pausedLookFrames) {
      drawLookCell(this.source, this.target, this.look, this.scale, x, 0);
    } else {
      drawStateFrame(this.source, this.target, this.state, this.stateFrame, this.scale, x, 0);
    }
  }

  private drawClipFrame(): void {
    if (!this.activeClip) return;
    const x = (this.target.canvas.width - CELL_WIDTH * this.scale) / 2;
    const sx = this.clipFrame * CELL_WIDTH;
    this.target.clearRect(0, 0, this.target.canvas.width, this.target.canvas.height);
    this.target.imageSmoothingEnabled = true;
    this.target.drawImage(
      this.activeClip.canvas,
      sx,
      0,
      CELL_WIDTH,
      CELL_HEIGHT,
      x,
      0,
      CELL_WIDTH * this.scale,
      CELL_HEIGHT * this.scale,
    );
  }

  private advanceClip(dt: number): void {
    const clip = this.activeClip;
    if (!clip) return;
    this.clipElapsed += dt;
    while (this.activeClip === clip && this.clipElapsed >= clip.manifest.durations[this.clipFrame]) {
      this.clipElapsed -= clip.manifest.durations[this.clipFrame];
      const next = this.clipFrame + 1;
      if (next < clip.manifest.frames) {
        this.clipFrame = next;
      } else if (clip.manifest.loop) {
        this.clipFrame = 0;
      } else {
        this.activeClip = null;
        this.clipFrame = 0;
        this.clipElapsed = 0;
        const onComplete = this.clipComplete;
        this.clipComplete = null;
        onComplete?.();
      }
    }
  }

  private readonly tick = (now: number): void => {
    const dt = now - this.lastTick;
    this.lastTick = now;

    if (this.activeClip) {
      this.advanceClip(dt);
      this.drawStaticFrame();
    } else if (this.look === null || this.pausedLookFrames) {
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

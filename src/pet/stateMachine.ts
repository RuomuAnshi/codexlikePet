import type { AnimationState } from "./atlas";
import type { DragDirection } from "./window";

export type PetAction = "waving" | "jumping" | "failed" | "waiting" | "review";

/**
 * Keeps interaction priorities in one place:
 * dragging wins over walking, walking wins over gestures, and gestures win
 * over the normal idle state.
 * The engine remains a renderer; this class owns the pet's intent.
 */
export class PetStateMachine {
  private dragging = false;
  private walking = false;
  private walkingDirection: DragDirection | null = null;
  private dragDirection: DragDirection | null = null;
  private carried = false;
  private action: PetAction | null = null;

  setDragging(
    dragging: boolean,
    direction: DragDirection | null = null,
    carried = false,
  ): void {
    this.dragging = dragging;
    this.dragDirection = direction;
    this.carried = carried;
  }

  setWalking(walking: boolean, direction: DragDirection | null = null): void {
    this.walking = walking;
    this.walkingDirection = direction;
  }

  startAction(action: PetAction): boolean {
    if (this.dragging || this.walking || this.action !== null) return false;
    this.action = action;
    return true;
  }

  finishAction(): void {
    this.action = null;
  }

  reset(): void {
    this.dragging = false;
    this.walking = false;
    this.walkingDirection = null;
    this.dragDirection = null;
    this.carried = false;
    this.action = null;
  }

  hasAction(): boolean {
    return this.action !== null;
  }

  animationState(): AnimationState {
    if (this.carried) return "waiting";
    if (this.dragDirection === "left" || this.dragDirection === "up-left" || this.dragDirection === "down-left") {
      return "running-left";
    }
    if (this.dragDirection === "right" || this.dragDirection === "up-right" || this.dragDirection === "down-right") {
      return "running-right";
    }
    if (this.dragging) return "running";
    if (
      this.walkingDirection === "left" ||
      this.walkingDirection === "up-left" ||
      this.walkingDirection === "down-left"
    ) {
      return "running-left";
    }
    if (
      this.walkingDirection === "right" ||
      this.walkingDirection === "up-right" ||
      this.walkingDirection === "down-right"
    ) {
      return "running-right";
    }
    if (this.walking) return "running";
    return this.action ?? "idle";
  }
}

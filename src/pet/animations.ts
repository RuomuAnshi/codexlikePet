import type { AnimationState } from "./atlas";

export type PetClipType = "mode" | "gesture";

/** Metadata for one optional full-body frame-animation clip. */
export interface PetAnimationClip {
  path: string;
  frames: number;
  durations: number[];
  loop: boolean;
  loopStart?: number;
  type: PetClipType;
  returnTo?: AnimationState;
  fallback?: AnimationState;
  dataUrl: string;
}

/** Runtime payload for the optional animations.json sidecar. */
export interface PetAnimationPack {
  format: "sakipet-frame-pack";
  version: 1;
  cellWidth: number;
  cellHeight: number;
  clips: Record<string, PetAnimationClip>;
}

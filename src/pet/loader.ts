import {
  CELL_HEIGHT,
  CELL_WIDTH,
  COLS,
  ROWS,
  STATE_TIMING,
  type AnimationState,
  type LookDirection,
  type PetManifest,
  lookSlot,
} from "./atlas";
import type { PetAnimationClip, PetAnimationPack } from "./animations";

export interface PetLoaderResult {
  manifest: PetManifest;
  canvas: HTMLCanvasElement;
}

export interface LoadedAnimationClip {
  manifest: PetAnimationClip;
  canvas: HTMLCanvasElement;
}

async function decodePet(manifest: PetManifest, blob: Blob): Promise<PetLoaderResult> {
  if (manifest.spriteVersionNumber !== 2) {
    throw new Error(`unsupported spriteVersionNumber ${manifest.spriteVersionNumber}; only v2 is supported`);
  }

  const bitmap = await createImageBitmap(blob);
  const width = bitmap.width;
  const height = bitmap.height;
  if (width !== CELL_WIDTH * COLS || height !== CELL_HEIGHT * ROWS) {
    bitmap.close();
    throw new Error(`spritesheet is ${width}x${height}; expected ${CELL_WIDTH * COLS}x${CELL_HEIGHT * ROWS}`);
  }

  // Normalize the bitmap for drawImage + sub-rect slicing inside a canvas.
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext("2d")!;
  ctx.drawImage(bitmap, 0, 0);
  bitmap.close();
  return { manifest, canvas };
}

// Fetch + decode a v2 pet package (pet.json + spritesheet) into a source canvas.
export async function loadPet(baseUrl: string): Promise<PetLoaderResult> {
  const manifestRes = await fetch(`${baseUrl}/pet.json`);
  if (!manifestRes.ok) throw new Error(`cannot fetch pet.json: ${manifestRes.status}`);
  const manifest: PetManifest = await manifestRes.json();
  const spritesheetRes = await fetch(`${baseUrl}/${manifest.spritesheetPath}`);
  if (!spritesheetRes.ok) throw new Error(`cannot fetch spritesheet: ${spritesheetRes.status}`);
  return decodePet(manifest, await spritesheetRes.blob());
}

/** Decode an imported pet whose spritesheet is delivered by the Rust backend. */
export async function loadPetFromData(
  manifest: PetManifest,
  spritesheetDataUrl: string,
): Promise<PetLoaderResult> {
  const response = await fetch(spritesheetDataUrl);
  return decodePet(manifest, await response.blob());
}

/** Decode optional SakiPet full-body frame clips from the Rust runtime payload. */
export async function loadAnimationPack(
  pack: PetAnimationPack | null | undefined,
): Promise<Map<string, LoadedAnimationClip>> {
  const loaded = new Map<string, LoadedAnimationClip>();
  if (!pack || pack.format !== "sakipet-frame-pack" || pack.version !== 1) return loaded;
  if (pack.cellWidth !== CELL_WIDTH || pack.cellHeight !== CELL_HEIGHT) return loaded;

  await Promise.all(
    Object.entries(pack.clips).map(async ([id, manifest]) => {
      try {
        if (!/^[-a-zA-Z0-9_]{1,64}$/.test(id)) throw new Error("invalid clip id");
        if (!Number.isInteger(manifest.frames) || manifest.frames < 1) throw new Error("invalid frame count");
        if (manifest.durations.length !== manifest.frames || manifest.durations.some((duration) => duration <= 0)) {
          throw new Error("invalid frame durations");
        }
        if (
          manifest.loopStart !== undefined
          && (!Number.isInteger(manifest.loopStart) || manifest.loopStart < 0 || manifest.loopStart >= manifest.frames)
        ) {
          throw new Error("invalid loop start");
        }
        const bitmap = await createImageBitmap(await (await fetch(manifest.dataUrl)).blob());
        if (bitmap.width !== CELL_WIDTH * manifest.frames || bitmap.height !== CELL_HEIGHT) {
          bitmap.close();
          throw new Error(`clip is ${bitmap.width}x${bitmap.height}; expected ${CELL_WIDTH * manifest.frames}x${CELL_HEIGHT}`);
        }
        const canvas = document.createElement("canvas");
        canvas.width = bitmap.width;
        canvas.height = bitmap.height;
        canvas.getContext("2d")!.drawImage(bitmap, 0, 0);
        bitmap.close();
        loaded.set(id, { manifest, canvas });
      } catch (error) {
        console.warn(`skipping invalid animation clip ${id}:`, error);
      }
    }),
  );
  return loaded;
}

// Draw the plane sprite for a standard animation row onto a target canvas.
export function drawStateFrame(
  source: HTMLCanvasElement,
  target: CanvasRenderingContext2D,
  state: AnimationState,
  frame: number,
  scale: number,
  offsetX: number,
  offsetY: number,
): void {
  const spec = STATE_TIMING[state];
  const col = frame % spec.used;
  const sx = col * CELL_WIDTH;
  const sy = spec.row * CELL_HEIGHT;
  target.clearRect(0, 0, target.canvas.width, target.canvas.height);
  target.imageSmoothingEnabled = true;
  target.drawImage(source, sx, sy, CELL_WIDTH, CELL_HEIGHT, offsetX, offsetY, CELL_WIDTH * scale, CELL_HEIGHT * scale);
}

// Draw the plane sprite for a look-direction cell onto a target canvas.
export function drawLookCell(
  source: HTMLCanvasElement,
  target: CanvasRenderingContext2D,
  direction: LookDirection,
  scale: number,
  offsetX: number,
  offsetY: number,
): void {
  const { row, col } = lookSlot(direction);
  const sx = col * CELL_WIDTH;
  const sy = row * CELL_HEIGHT;
  target.clearRect(0, 0, target.canvas.width, target.canvas.height);
  target.imageSmoothingEnabled = true;
  target.drawImage(source, sx, sy, CELL_WIDTH, CELL_HEIGHT, offsetX, offsetY, CELL_WIDTH * scale, CELL_HEIGHT * scale);
}

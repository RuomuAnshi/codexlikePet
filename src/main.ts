import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { CELL_HEIGHT, CELL_WIDTH, type LookDirection } from "./pet/atlas";
import { loadPet } from "./pet/loader";
import { loadPetFromData } from "./pet/loader";
import { PetEngine } from "./pet/engine";
import { watchCursorDirection } from "./pet/cursorWatcher";
import { PetStateMachine, type PetAction } from "./pet/stateMachine";
import { attachDrag, attachGestures, dragState, type DragDirection, type Gesture } from "./pet/window";
import { PetWalker } from "./pet/walker";
import { waitForAppReady } from "./appReady";
import type {
  PetBehavior,
  PetDialogue,
  PetSettings,
  PetSettingsEvent,
  RuntimeConfig,
} from "./pet/config";
import { DEFAULT_DIALOGUE } from "./pet/dialogue";

const PETS_BASE = import.meta.env.BASE_URL + "pets";
type DialogueTrigger = Exclude<keyof PetDialogue, "version">;
const IDLE_SPEECH_DELAY_MS = 90_000;
const PET_BUBBLE_MAX_CHARS = 100;

interface PetMeetupEvent {
  meetupId: string;
  petId: string;
  partnerPetId: string;
  targetX: number;
  targetY: number;
  travelMs: number;
}

async function boot(): Promise<void> {
  await waitForAppReady();
  const window = getCurrentWindow();
  const runtime = await invoke<RuntimeConfig>("get_runtime_config", { windowLabel: window.label });
  const loadRuntimePet = async (): Promise<Awaited<ReturnType<typeof loadPet>>> => {
    if (runtime.source === "imported" && runtime.manifest && runtime.spritesheetDataUrl) {
      return loadPetFromData(runtime.manifest, runtime.spritesheetDataUrl);
    }
    if (!runtime.path) throw new Error(`宠物资源不存在：${runtime.petId}`);
    return loadPet(`${PETS_BASE}/${runtime.path}`);
  };

  const initialPet = await loadRuntimePet();
  let dialogue = runtime.dialogue ?? DEFAULT_DIALOGUE;
  let settings: PetSettings = runtime.settings;
  const stage = document.querySelector<HTMLCanvasElement>("#stage")!;
  const petEl = document.querySelector<HTMLElement>("#pet")!;
  const speech = document.querySelector<HTMLElement>("#speech")!;
  const speechText = document.querySelector<HTMLElement>("#speech-text")!;
  const setStageSize = (scale: number): void => {
    stage.width = Math.round(CELL_WIDTH * scale);
    stage.height = Math.round(CELL_HEIGHT * scale);
  };
  setStageSize(settings.scale);
  petEl.style.opacity = String(settings.opacity);

  const engine = new PetEngine(initialPet.canvas, stage, settings.scale);
  const stateMachine = new PetStateMachine();
  let paused = settings.paused;
  let dragging = false;
  let hovered = false;
  let lastDirection: LookDirection | null = null;
  let walking = false;
  const dialogueIndices: Record<DialogueTrigger, number> = {
    doubleClick: 0,
    click: 0,
    rightClick: 0,
    walk: 0,
    drag: 0,
    idle: 0,
  };
  let speechTimer: number | undefined;
  let idleSpeechTimer: number | undefined;
  let clickTimer: number | undefined;
  let dragDialogueShown = false;
  let chatRequestId: string | null = null;
  let chatReply = "";
  let behaviorLookTimer: number | undefined;

  const speechPreview = (text: string): string => {
    const chars = [...text.trim()];
    return chars.length > PET_BUBBLE_MAX_CHARS
      ? `${chars.slice(0, PET_BUBBLE_MAX_CHARS - 1).join("")}…`
      : chars.join("");
  };

  const showSpeech = (text: string, duration: number): void => {
    const preview = speechPreview(text);
    if (!preview) return;
    speechText.textContent = preview;
    speech.hidden = false;
    speech.classList.add("speech-visible");
    if (speechTimer !== undefined) globalThis.clearTimeout(speechTimer);
    speechTimer = globalThis.setTimeout(() => {
      speech.classList.remove("speech-visible");
      speechTimer = globalThis.setTimeout(() => {
        speech.hidden = true;
      }, 180);
    }, duration);
  };

  const recordPetInteraction = (kind: string): void => {
    void invoke("record_pet_interaction", { petId: runtime.petId, kind }).catch((error) => {
      console.warn("failed to update pet life state:", error);
    });
  };

  const settlePetActivity = (): void => {
    void invoke("settle_pet_activity", { petId: runtime.petId }).catch((error) => {
      console.warn("failed to settle pet life state:", error);
    });
  };

  const sayLine = (trigger: DialogueTrigger): void => {
    if (settings.quietMode) return;
    const lines = dialogue[trigger];
    if (!lines.length) return;
    const index = dialogueIndices[trigger] % lines.length;
    dialogueIndices[trigger] += 1;
    showSpeech(lines[index], 3600);
  };

  const scheduleIdleSpeech = (): void => {
    if (idleSpeechTimer !== undefined) globalThis.clearTimeout(idleSpeechTimer);
    idleSpeechTimer = undefined;
    if (settings.quietMode || !dialogue.idle.length) return;
    idleSpeechTimer = globalThis.setTimeout(() => {
      idleSpeechTimer = undefined;
      if (!settings.quietMode && !dragging && !walking && !stateMachine.hasAction()) {
        sayLine("idle");
      }
      scheduleIdleSpeech();
    }, IDLE_SPEECH_DELAY_MS);
  };

  const walker = new PetWalker((isWalking, direction) => {
    if (isWalking && dragging) return;
    const startedWalking = isWalking && !walking;
    walking = isWalking;
    stateMachine.setWalking(isWalking, direction);
    if (isWalking) {
      engine.setLook(null);
      if (startedWalking) {
        recordPetInteraction("walk");
        sayLine("walk");
      }
    } else if (!dragging) engine.setLook(lastDirection);
    syncAnimation();
    if (!isWalking) {
      settlePetActivity();
      void savePosition();
    }
  });

  const syncAnimation = (): void => {
    engine.setState(stateMachine.animationState());
  };

  const savePosition = async (): Promise<void> => {
    try {
      const [position, scaleFactor] = await Promise.all([window.outerPosition(), window.scaleFactor()]);
      const logicalPosition = position.toLogical(scaleFactor);
      await invoke("save_pet_position", {
        instanceId: runtime.instanceId,
        x: logicalPosition.x,
        y: logicalPosition.y,
      });
    } catch (error) {
      console.warn("failed to save pet position:", error);
    }
  };

  const applySettings = (next: PetSettings): void => {
    settings = next;
    paused = next.paused;
    setStageSize(next.scale);
    petEl.style.opacity = String(next.opacity);
    engine.setScale(next.scale);
    walker.setSettings(next.speed, next.wanderEnabled, next.quietMode);
    engine.play(!next.paused);
    if (next.paused || !next.wanderEnabled || next.quietMode) walker.stop();
    else if (!dragging) walker.start();
    if (!dragging) engine.setLook(lastDirection);
    syncAnimation();
    scheduleIdleSpeech();
  };

  const openPetManager = async (): Promise<void> => {
    try {
      await invoke("open_pet_manager");
    } catch (error) {
      console.error("failed to open pet manager:", error);
    }
  };

  const openPetChat = async (): Promise<void> => {
    try {
      await invoke("open_pet_chat", { petId: runtime.petId });
    } catch (error) {
      console.error("failed to open pet chat:", error);
      sayLine("doubleClick");
    }
  };

  speech.addEventListener("click", () => void openPetChat());

  const playAction = (action: PetAction): void => {
    if (paused || dragging || !stateMachine.startAction(action)) return;
    engine.setLook(null);
    engine.playOnce(action, () => {
      stateMachine.finishAction();
      if (!dragging) engine.setLook(lastDirection);
      syncAnimation();
      settlePetActivity();
    });
  };

  const applyPetBehavior = (behavior: PetBehavior | null | undefined): void => {
    if (!behavior || paused || settings.quietMode) return;
    if (behaviorLookTimer !== undefined) globalThis.clearTimeout(behaviorLookTimer);
    behaviorLookTimer = undefined;
    switch (behavior.action) {
      case "walk":
        walker.walkNow();
        break;
      case "sleep":
        playAction("waiting");
        break;
      case "idle":
        break;
      default:
        playAction(behavior.action);
        break;
    }
    if (behavior.action === "idle" && behavior.look) {
      const directionNames: Record<string, LookDirection> = {
        up: 0,
        "up-right": 2,
        right: 4,
        "down-right": 6,
        down: 8,
        "down-left": 10,
        left: 12,
        "up-left": 14,
      };
      const numericDirection = Number(behavior.look);
      const direction = Number.isInteger(numericDirection) && numericDirection >= 0 && numericDirection < 16
        ? numericDirection as LookDirection
        : directionNames[behavior.look];
      if (direction !== undefined) {
        engine.setLook(direction);
        behaviorLookTimer = globalThis.setTimeout(() => {
          behaviorLookTimer = undefined;
          if (!dragging) engine.setLook(lastDirection);
        }, behavior.duration);
      }
    }
  };

  watchCursorDirection((direction) => {
    lastDirection = direction === null ? null : (direction as LookDirection);
    if (!dragging) engine.setLook(lastDirection);
  });

  attachDrag(
    petEl,
    (enabled, direction: DragDirection | null) => {
      dragging = enabled;
      if (enabled) walker.stop();
      if (enabled && stateMachine.hasAction()) {
        stateMachine.finishAction();
        engine.cancelAction();
      }
      stateMachine.setDragging(enabled, direction);
      if (enabled && direction && !dragDialogueShown) {
        dragDialogueShown = true;
        recordPetInteraction("drag");
        sayLine("drag");
      }
      if (enabled) engine.setLook(null);
      else engine.setLook(lastDirection);
      syncAnimation();
      if (enabled) void savePosition();
      if (!enabled) {
        dragDialogueShown = false;
        void savePosition();
        if (!paused && settings.wanderEnabled && !settings.quietMode) walker.start();
      }
    },
    () => !settings.lockPosition && !settings.clickThrough,
  );

  petEl.addEventListener("pointerenter", () => {
    if (hovered || dragState.current) return;
    hovered = true;
    playAction("jumping");
  });
  petEl.addEventListener("pointerleave", () => {
    hovered = false;
    if (!dragging && !stateMachine.hasAction()) engine.setLook(lastDirection);
  });
  petEl.addEventListener("dblclick", (event) => {
    event.preventDefault();
    if (clickTimer !== undefined) {
      globalThis.clearTimeout(clickTimer);
      clickTimer = undefined;
    }
    void openPetChat();
    recordPetInteraction("doubleClick");
    playAction("jumping");
  });

  attachGestures(petEl, (gesture: Gesture) => {
    if (gesture === "right") {
      recordPetInteraction("rightClick");
      sayLine("rightClick");
      playAction("failed");
      return;
    }
    if (clickTimer !== undefined) globalThis.clearTimeout(clickTimer);
    clickTimer = globalThis.setTimeout(() => {
      clickTimer = undefined;
      recordPetInteraction("click");
      sayLine("click");
      playAction("jumping");
    }, 320);
  });

  await listen<PetSettingsEvent>("pet://settings", ({ payload }) => {
    if (payload.petId === runtime.petId) applySettings(payload.settings);
  });
  await listen<string>("pet://command", ({ payload }) => {
    if (payload === "open-manager") {
      void openPetManager();
    }
  });
  await listen<PetMeetupEvent>("pet://meetup", ({ payload }) => {
    if (payload.petId !== runtime.petId) return;
    walker.walkTo(payload.targetX, payload.targetY);
  });
  await listen<{ requestId: string; petId: string; delta: string }>("chat://delta", ({ payload }) => {
    if (payload.petId !== runtime.petId) return;
    if (chatRequestId !== null && payload.requestId !== chatRequestId) return;
    chatRequestId ??= payload.requestId;
    chatReply += payload.delta;
    showSpeech(chatReply, 7_000);
  });
  await listen<{
    requestId: string;
    petId: string;
    message: { content: string; source: string };
    behavior?: PetBehavior | null;
  }>("chat://complete", ({ payload }) => {
    if (payload.petId !== runtime.petId) return;
    applyPetBehavior(payload.behavior);
    if (payload.message.source === "heartbeat") {
      showSpeech(payload.message.content, payload.behavior?.duration ?? 5_200);
      return;
    }
    if (payload.message.source === "pet-conversation") {
      showSpeech(payload.message.content, payload.behavior?.duration ?? 5_200);
      return;
    }
    if (chatRequestId !== null && payload.requestId !== chatRequestId) return;
    chatRequestId = null;
    chatReply = "";
    showSpeech(payload.message.content, payload.behavior?.duration ?? 7_000);
  });
  await listen<{ requestId: string; petId: string; message: string }>("chat://error", ({ payload }) => {
    if (payload.petId !== runtime.petId) return;
    if (chatRequestId !== null && payload.requestId !== chatRequestId) return;
    chatRequestId = null;
    chatReply = "";
    showSpeech(`刚才没听清……${payload.message}`, 5_200);
  });

  document.title = initialPet.manifest.displayName;
  engine.play(!paused);
  walker.setSettings(settings.speed, settings.wanderEnabled, settings.quietMode);
  scheduleIdleSpeech();
  if (!paused && settings.wanderEnabled && !settings.quietMode) walker.start();
}

boot().catch((err) => {
  console.error("failed to boot pet:", err);
  const stage = document.querySelector<HTMLCanvasElement>("#stage")!;
  const ctx = stage.getContext("2d")!;
  ctx.fillStyle = "#333";
  ctx.fillRect(0, 0, stage.width, stage.height);
  ctx.fillStyle = "#fff";
  ctx.font = "13px monospace";
  ctx.fillText(String(err?.message ?? err), 10, 30);
});

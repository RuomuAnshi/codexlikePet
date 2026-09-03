import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { CELL_HEIGHT, CELL_WIDTH, type AnimationState, type LookDirection } from "./pet/atlas";
import { loadAnimationPack, loadPet, loadPetFromData } from "./pet/loader";
import { PetEngine } from "./pet/engine";
import { watchCursorDirection } from "./pet/cursorWatcher";
import { PetStateMachine, type PetAction } from "./pet/stateMachine";
import { attachDrag, attachGestures, dragState, isThrowVelocity, type DragDirection, type Gesture } from "./pet/window";
import { PetWalker } from "./pet/walker";
import { ThrowPhysics } from "./pet/throw";
import { extractSayText } from "./pet/streaming";
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

const EMOTION_ICONS: Record<string, string> = {
  sleeping: "💤",
  sleepy: "💤",
  sad: "😿",
  lonely: "💔",
  social: "💬",
  curious: "👀",
  happy: "😊",
  content: "🙂",
  calm: "🙂",
  low: "!",
  hungry: "🍽️",
};

interface PetMeetupEvent {
  meetupId: string;
  petId: string;
  partnerPetId: string;
  targetX: number;
  targetY: number;
  travelMs: number;
}

interface SocialSceneParticipant {
  instanceId: string;
  petId: string;
  role: string;
}

interface SocialScenePhaseParticipant {
  instanceId: string;
  petId: string;
  animation: string;
  look?: string | null;
  say?: string | null;
  effect?: string | null;
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
  const animationClips = await loadAnimationPack(runtime.animationPack);
  let dialogue = runtime.dialogue ?? DEFAULT_DIALOGUE;
  let settings: PetSettings = runtime.settings;
  const stage = document.querySelector<HTMLCanvasElement>("#stage")!;
  const petEl = document.querySelector<HTMLElement>("#pet")!;
  const effects = document.querySelector<HTMLElement>("#effects")!;
  const emotion = document.querySelector<HTMLElement>("#emotion")!;
  const setStageSize = (scale: number): void => {
    stage.width = Math.round(CELL_WIDTH * scale);
    stage.height = Math.round(CELL_HEIGHT * scale);
  };
  setStageSize(settings.scale);
  petEl.style.opacity = String(settings.opacity);

  const engine = new PetEngine(initialPet.canvas, stage, settings.scale);
  engine.setAnimationClips(animationClips);
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
    morning: 0,
    evening: 0,
    sleep: 0,
    wake: 0,
    petting: 0,
    feed: 0,
    play: 0,
    pickup: 0,
    putDown: 0,
    lowBattery: 0,
    breakReminder: 0,
    reunion: 0,
    milestone: 0,
  };
  let idleSpeechTimer: number | undefined;
  let clickTimer: number | undefined;
  let dragDialogueShown = false;
  let chatRequestId: string | null = null;
  let chatReply = "";
  let behaviorLookTimer: number | undefined;
  let pettingTimer: number | undefined;
  let autoQuiet = false;
  let socialSceneId: string | null = null;
  let windowSceneId: string | null = null;
  let lifeSleeping = false;
  let lifeResting = false;
  let thrownActive = false;
  let squashTimer: number | undefined;
  const reducedMotion = (): boolean =>
    globalThis.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;

  const applySquash = (): void => {
    if (reducedMotion()) return;
    // The window itself cannot deform, but the canvas inside can: a short
    // squash-and-stretch sells the landing better than any animation frame.
    stage.style.transform = "scaleY(0.82) scaleX(1.06)";
    if (squashTimer !== undefined) globalThis.clearTimeout(squashTimer);
    squashTimer = globalThis.setTimeout(() => {
      squashTimer = undefined;
      if (!thrownActive) stage.style.transform = "";
    }, 120);
  };

  const throwPhysics = new ThrowPhysics(petEl);

  const finishThrow = (rest: { x: number; y: number } | null): void => {
    const landed = rest !== null;
    thrownActive = false;
    stateMachine.setThrown(false);
    stage.style.transform = "";
    if (!landed) {
      // Caught mid-air: the drag handler owns everything from here.
      stopFrameClip();
      syncAnimation();
      return;
    }
    showEffect("dust");
    applySquash();
    void invoke("set_pet_position_safely", {
      instanceId: runtime.instanceId,
      x: rest.x,
      y: rest.y,
    })
      .catch(() => undefined)
      .finally(() => {
        void savePosition();
        void reportRuntimeState(false);
      });
    settlePetActivity();
    stopFrameClip();
    if (!playFrameClip("land")) syncAnimation();
    if (!paused && settings.wanderEnabled && !settings.quietMode && !autoQuiet) walker.start();
  };

  const startThrow = (velocity: { x: number; y: number }): void => {
    thrownActive = true;
    stateMachine.setThrown(true);
    if (stateMachine.hasAction()) {
      stateMachine.finishAction();
      engine.cancelAction();
    }
    stopFrameClip();
    // Optional per-pet clip; falls back to the running flail via the
    // state machine while airborne.
    if (!playFrameClip("fall")) syncAnimation();
    engine.setLook(null);
    void invoke("hide_pet_speech", { instanceId: runtime.instanceId }).catch(() => undefined);
    recordPetInteraction("throw");
    // Stay marked busy so the social coordinator never targets a flying pet.
    void reportRuntimeState(true);
    throwPhysics.launch(velocity, {
      onFrame: (lean) => {
        if (!reducedMotion()) stage.style.transform = `rotate(${lean.toFixed(1)}deg)`;
      },
      onBounce: () => showEffect("dust"),
      onRest: (rest) => finishThrow(rest),
      onCaught: () => finishThrow(null),
    });
  };

  const speechPreview = (text: string): string => {
    const chars = [...text.trim()];
    return chars.length > PET_BUBBLE_MAX_CHARS
      ? `${chars.slice(0, PET_BUBBLE_MAX_CHARS - 1).join("")}…`
      : chars.join("");
  };

  const showSpeech = (text: string, duration: number): void => {
    const preview = speechPreview(text);
    if (!preview) return;
    // Speech uses a separate transparent window. The pet canvas can therefore
    // remain exactly its configured size, even when the pet is scaled down.
    void invoke("show_pet_speech", {
      instanceId: runtime.instanceId,
      petId: runtime.petId,
      text: preview,
      duration: Math.max(900, Math.min(12_000, Math.round(duration))),
    }).catch((error) => {
      console.warn("failed to show pet speech:", error);
    });
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
    if (settings.quietMode || autoQuiet) return;
    const lines = dialogue[trigger] ?? [];
    if (!lines.length) return;
    const index = dialogueIndices[trigger] % lines.length;
    dialogueIndices[trigger] += 1;
    showSpeech(lines[index], 3600);
  };

  const showEffect = (kind: "heart" | "star" | "food" | "dust"): void => {
    if (globalThis.matchMedia?.("(prefers-reduced-motion: reduce)").matches) return;
    const symbols = { heart: "♥", star: "✦", food: "✿", dust: "·" };
    const particle = document.createElement("span");
    particle.className = `particle particle-${kind}`;
    particle.textContent = symbols[kind];
    particle.style.left = `${35 + Math.random() * 30}%`;
    particle.style.top = `${34 + Math.random() * 22}%`;
    effects.append(particle);
    globalThis.setTimeout(() => particle.remove(), 1_200);
  };

  const showEmotion = (value: string, duration = 2_000): void => {
    emotion.textContent = EMOTION_ICONS[value] ?? "❤";
    emotion.hidden = false;
    emotion.classList.add("emotion-visible");
    globalThis.setTimeout(() => emotion.classList.remove("emotion-visible"), duration);
  };

  const spawnFoodVisual = (): void => {
    if (globalThis.matchMedia?.("(prefers-reduced-motion: reduce)").matches) return;
    const food = document.createElement("img");
    food.className = "food-drop";
    food.src = `${import.meta.env.BASE_URL}props/snack/sprite.svg`;
    food.alt = "";
    effects.append(food);
    food.addEventListener("animationend", () => food.remove(), { once: true });
  };

  const scheduleIdleSpeech = (): void => {
    if (idleSpeechTimer !== undefined) globalThis.clearTimeout(idleSpeechTimer);
    idleSpeechTimer = undefined;
    if (settings.quietMode || autoQuiet || !dialogue.idle.length) return;
    idleSpeechTimer = globalThis.setTimeout(() => {
      idleSpeechTimer = undefined;
      if (!settings.quietMode && !autoQuiet && !dragging && !walking && !thrownActive && !stateMachine.hasAction()) {
        sayLine("idle");
      }
      scheduleIdleSpeech();
    }, IDLE_SPEECH_DELAY_MS);
  };

  const walker = new PetWalker(runtime.instanceId, (isWalking, direction) => {
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
      // Social snapshots may temporarily use the runtime position for
      // proximity decisions. Refresh it after autonomous movement so the
      // coordinator does not plan from the walk's old starting point.
      void reportRuntimeState(false);
    }
  });

  const syncAnimation = (): void => {
    engine.setState(stateMachine.animationState());
  };

  const stopFrameClip = (): void => {
    if (!engine.isPlayingClip()) return;
    engine.cancelClip();
    stateMachine.finishClip();
    if (!dragging) engine.setLook(lastDirection);
    syncAnimation();
  };

  const setBaseMode = (mode: NonNullable<PetBehavior["mode"]>): void => {
    stopFrameClip();
    stateMachine.setBaseState(
      mode === "working" ? "running" : mode === "waiting" || mode === "sleeping" ? "waiting" : "idle",
    );
    if (!dragging && !stateMachine.hasAction()) syncAnimation();
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

  async function reportRuntimeState(isDragging: boolean): Promise<void> {
    try {
      const [position, scaleFactor] = await Promise.all([window.outerPosition(), window.scaleFactor()]);
      const logical = position.toLogical(scaleFactor);
      await invoke("report_pet_runtime_state", {
        instanceId: runtime.instanceId,
        petId: runtime.petId,
        dragging: isDragging,
        busy: chatRequestId !== null,
        position: { x: logical.x, y: logical.y },
      });
    } catch (error) {
      console.warn("failed to report social runtime state:", error);
    }
  }

  const applySettings = (next: PetSettings): void => {
    if (!next.windowInteractionEnabled && windowSceneId) {
      void invoke("cancel_window_scene", { sceneId: windowSceneId }).catch(() => undefined);
    }
    settings = next;
    paused = next.paused;
    setStageSize(next.scale);
    petEl.style.opacity = String(next.opacity);
    engine.setScale(next.scale);
    walker.setSettings(next.speed, next.wanderEnabled, next.quietMode);
    engine.play(!next.paused);
    if (next.paused || !next.wanderEnabled || next.quietMode || autoQuiet || lifeSleeping || lifeResting) {
      walker.stop();
    } else if (!dragging) {
      walker.start();
    }
    if (!dragging && !walking) engine.setLook(lastDirection);
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

  const togglePetChat = async (): Promise<void> => {
    try {
      if (socialSceneId) {
        await invoke("cancel_social_scene", { sceneId: socialSceneId });
      }
      if (windowSceneId) {
        await invoke("cancel_window_scene", { sceneId: windowSceneId });
      }
      const opened = await invoke<boolean>("toggle_pet_chat", { petId: runtime.petId });
      if (opened) sayLine("doubleClick");
    } catch (error) {
      console.error("failed to toggle pet chat:", error);
      sayLine("doubleClick");
    }
  };

  const playAction = (action: PetAction): void => {
    stopFrameClip();
    if (paused || dragging || dragState.petting || !stateMachine.startAction(action)) return;
    engine.setLook(null);
    engine.playOnce(action, () => {
      stateMachine.finishAction();
      if (!dragging) engine.setLook(lastDirection);
      syncAnimation();
      settlePetActivity();
    });
  };

  const playFrameClip = (clipId: string, onComplete?: () => void): boolean => {
    if (paused || dragging || dragState.petting || !engine.hasAnimationClip(clipId)) return false;
    if (!stateMachine.startClip(clipId)) return false;
    engine.setLook(null);
    if (!engine.playClip(clipId, () => {
      const returnTo = animationClips.get(clipId)?.manifest.returnTo;
      stateMachine.finishClip();
      if (returnTo) stateMachine.setBaseState(returnTo);
      if (!dragging) engine.setLook(lastDirection);
      syncAnimation();
      settlePetActivity();
      onComplete?.();
    })) {
      stateMachine.finishClip();
      return false;
    }
    return true;
  };

  // Extra frame clips are optional per pet. Keep the standard atlas as the
  // safe fallback, but use the richer lifecycle clips whenever this pet has
  // them. This is deliberately resolved from the runtime clip map instead of
  // assuming that every pet ships the same animation files.
  const startSleepSequence = (preferredGesture?: string): boolean => {
    setBaseMode("sleeping");
    const leadClip = preferredGesture && preferredGesture !== "sleep"
      ? preferredGesture
      : "yawn";
    if (engine.hasAnimationClip(leadClip)) {
      return playFrameClip(leadClip, () => {
        if (!playFrameClip("sleep")) syncAnimation();
      });
    }
    if (!playFrameClip("sleep")) {
      syncAnimation();
      return false;
    }
    return true;
  };

  const startWalkingAfterWake = (): void => {
    if (
      !paused &&
      !dragging &&
      !dragState.petting &&
      !settings.quietMode &&
      !autoQuiet &&
      settings.wanderEnabled &&
      !lifeSleeping &&
      !lifeResting
    ) {
      // Wake-up should lead back into the pet's normal life instead of
      // leaving it parked forever after the sleep clip finishes.
      walker.walkNow();
    }
  };

  const startWakeSequence = (onComplete?: () => void): boolean => {
    setBaseMode("idle");
    const wakeClip = engine.hasAnimationClip("wake")
      ? "wake"
      : engine.hasAnimationClip("stretch")
        ? "stretch"
        : null;
    if (!wakeClip) {
      syncAnimation();
      return false;
    }
    return playFrameClip(wakeClip, () => {
      if (wakeClip !== "stretch" && engine.hasAnimationClip("stretch")) {
        if (!playFrameClip("stretch", () => {
          syncAnimation();
          onComplete?.();
        })) {
          syncAnimation();
          onComplete?.();
        }
      } else {
        syncAnimation();
        onComplete?.();
      }
    });
  };

  const applyPetBehavior = (behavior: PetBehavior | null | undefined): void => {
    if (!behavior || paused || settings.quietMode) return;
    if (behaviorLookTimer !== undefined) globalThis.clearTimeout(behaviorLookTimer);
    behaviorLookTimer = undefined;
    if (behavior.mode) setBaseMode(behavior.mode);
    else if (behavior.action !== "walk" && behavior.action !== "sleep" && behavior.action !== "running") {
      setBaseMode("idle");
    }
    const playedGesture = behavior.action !== "sleep" && behavior.gesture
      ? playFrameClip(behavior.gesture)
      : false;
    switch (behavior.action) {
      case "walk":
        walker.walkNow();
        break;
      case "sleep":
        startSleepSequence(behavior.gesture ?? undefined);
        break;
      case "running":
        setBaseMode("working");
        break;
      case "idle":
        if (!behavior.mode) setBaseMode("idle");
        break;
      default:
        if (!playedGesture) playAction(behavior.action);
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

  watchCursorDirection(
    (direction) => {
      lastDirection = direction === null ? null : (direction as LookDirection);
      if (!dragging && !walking && !dragState.petting && !socialSceneId && !windowSceneId && !thrownActive) {
        engine.setLook(lastDirection);
      }
    },
    100,
    () => !paused,
  );

  attachDrag(
    petEl,
    runtime.instanceId,
    (enabled, direction: DragDirection | null, carried, releaseVelocity) => {
      const throwing =
        !enabled && releaseVelocity !== null && isThrowVelocity(releaseVelocity);
      dragging = enabled;
      if (enabled) {
        if (windowSceneId) {
          void invoke("cancel_window_scene", { sceneId: windowSceneId }).catch(() => undefined);
        }
        void invoke("hide_pet_speech", { instanceId: runtime.instanceId }).catch(() => undefined);
      }
      if (enabled) walker.stop();
      if (enabled && stateMachine.hasAction()) {
        stateMachine.finishAction();
        engine.cancelAction();
      }
      stateMachine.setDragging(enabled, direction, carried);
      if (enabled && direction && !dragDialogueShown) {
        dragDialogueShown = true;
        recordPetInteraction("drag");
        sayLine("drag");
      }
      if (enabled && carried && !petEl.classList.contains("is-carried")) {
        petEl.classList.add("is-carried");
        showEffect("star");
        sayLine("pickup");
      }
      if (!enabled && petEl.classList.contains("is-carried")) {
        petEl.classList.remove("is-carried");
        // The throw sequence provides its own landing effects and dialogue
        // timing, so the plain put-down feedback only fits a normal drop.
        if (!throwing) {
          showEffect("dust");
          sayLine("putDown");
        }
      }
      if (enabled) engine.setLook(null);
      else if (!throwing) engine.setLook(lastDirection);
      syncAnimation();
      if (enabled) void savePosition();
      if (throwing && releaseVelocity) {
        startThrow(releaseVelocity);
        return;
      }
      void reportRuntimeState(enabled);
      if (!enabled) {
        dragDialogueShown = false;
        void savePosition();
        void reportRuntimeState(false);
        if (!paused && settings.wanderEnabled && !settings.quietMode) walker.start();
      }
    },
    () => !settings.lockPosition && !settings.clickThrough,
  );

  petEl.addEventListener("pointerenter", () => {
    if (hovered || dragState.current || thrownActive) return;
    hovered = true;
    playAction("jumping");
  });
  petEl.addEventListener("pointerleave", () => {
    hovered = false;
    if (
      !dragging &&
      !walking &&
      !dragState.petting &&
      !socialSceneId &&
      !windowSceneId &&
      !thrownActive &&
      !stateMachine.hasAction()
    ) {
      engine.setLook(lastDirection);
    }
  });
  petEl.addEventListener("dblclick", (event) => {
    event.preventDefault();
    if (clickTimer !== undefined) {
      globalThis.clearTimeout(clickTimer);
      clickTimer = undefined;
    }
    void togglePetChat();
    recordPetInteraction("doubleClick");
    playAction("jumping");
  });

  attachGestures(petEl, (gesture: Gesture) => {
    if (gesture === "right") {
      void invoke("show_pet_context_menu", { windowLabel: window.label }).catch((error) => {
        console.warn("failed to open pet menu:", error);
      });
      return;
    }
    if (gesture === "petting-start") {
      walker.stop();
      if (stateMachine.hasAction()) {
        stateMachine.finishAction();
        engine.cancelAction();
      }
      engine.setLook(null);
      stateMachine.setBaseState("waiting");
      syncAnimation();
      sayLine("petting");
      showEffect("heart");
      void invoke("record_petting", { petId: runtime.petId });
      pettingTimer = globalThis.setInterval(() => {
        showEffect("heart");
        void invoke("record_petting", { petId: runtime.petId });
      }, 700);
      return;
    }
    if (gesture === "petting-end") {
      if (pettingTimer !== undefined) globalThis.clearInterval(pettingTimer);
      pettingTimer = undefined;
      settlePetActivity();
      if (!dragging) engine.setLook(lastDirection);
      syncAnimation();
      if (!paused && settings.wanderEnabled && !settings.quietMode && !autoQuiet) walker.start();
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
  await listen<{ sceneId: string; participants: SocialSceneParticipant[] }>(
    "pet://social-scene-start",
    ({ payload }) => {
      if (!payload.participants.some((participant) => participant.instanceId === runtime.instanceId)) return;
      socialSceneId = payload.sceneId;
      walker.stop();
      stateMachine.setSocialState("waiting");
      syncAnimation();
    },
  );
  await listen<{ sceneId: string; participants: SocialScenePhaseParticipant[] }>(
    "pet://social-phase",
    ({ payload }) => {
      if (payload.sceneId !== socialSceneId) return;
      const participant = payload.participants.find((item) => item.instanceId === runtime.instanceId);
      if (!participant || dragging) return;
      const animationMap: Record<string, AnimationState> = {
        idle: "idle",
        walking: "running",
        running: "running",
        waving: "waving",
        jumping: "jumping",
        waiting: "waiting",
      };
      stateMachine.setSocialState(animationMap[participant.animation] ?? "idle");
      if (participant.look) {
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
        const direction = directionNames[participant.look];
        if (direction !== undefined) engine.setLook(direction);
      }
      if (participant.say) showSpeech(participant.say, 4_200);
      if (participant.effect === "heart") showEffect("heart");
      if (participant.effect === "food") showEffect("food");
      if (participant.effect === "dust") showEffect("dust");
      if (participant.effect === "star" || participant.effect === "sparkle") showEffect("star");
      syncAnimation();
    },
  );
  await listen<{ sceneId: string }>("pet://social-scene-end", ({ payload }) => {
    if (payload.sceneId !== socialSceneId) return;
    socialSceneId = null;
    stateMachine.setSocialState(null);
    if (!dragging && !paused && !settings.quietMode && !autoQuiet && settings.wanderEnabled) walker.start();
    if (!dragging) engine.setLook(lastDirection);
    syncAnimation();
    void savePosition();
  });
  await listen<{
    sceneId: string;
    instanceId: string;
    petId: string;
    windowId: number;
    mode: "crawl" | "sit";
  }>("desktop://window-scene-start", ({ payload }) => {
    if (payload.instanceId !== runtime.instanceId || payload.petId !== runtime.petId) return;
    windowSceneId = payload.sceneId;
    walker.stop();
    stateMachine.setSocialState("waiting");
    syncAnimation();
  });
  await listen<{
    sceneId: string;
    instanceId: string;
    petId: string;
    phase: string;
    animation: string;
    look: string;
    onWindow: boolean;
  }>("desktop://window-scene-phase", ({ payload }) => {
    if (payload.sceneId !== windowSceneId || payload.instanceId !== runtime.instanceId) return;
    const animationMap: Record<string, AnimationState> = {
      idle: "idle",
      waiting: "waiting",
      running: "running",
      jumping: "jumping",
    };
    stateMachine.setSocialState(animationMap[payload.animation] ?? "idle");
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
    const direction = directionNames[payload.look];
    if (direction !== undefined) engine.setLook(direction);
    if (payload.phase === "jump-off") showEffect("dust");
    syncAnimation();
  });
  await listen<{ sceneId: string; instanceId: string; cancelled: boolean }>(
    "desktop://window-scene-end",
    ({ payload }) => {
      if (payload.sceneId !== windowSceneId || payload.instanceId !== runtime.instanceId) return;
      windowSceneId = null;
      stateMachine.setSocialState(null);
      if (
        !dragging &&
        !paused &&
        !settings.quietMode &&
        !autoQuiet &&
        settings.wanderEnabled &&
        !lifeSleeping &&
        !lifeResting
      ) {
        walker.start();
      }
      if (!dragging) engine.setLook(lastDirection);
      syncAnimation();
      void savePosition();
    },
  );
  await listen<{
    petId: string;
    state: { activity: string; mood: string; energy: number; food?: number };
  }>("pet://life-state",
    ({ payload }) => {
      if (payload.petId !== runtime.petId) return;
      const wasSleeping = lifeSleeping;
      const wasResting = lifeResting;
      lifeSleeping = payload.state.activity === "sleeping";
      lifeResting = payload.state.activity === "resting";
      if (lifeSleeping) {
        walker.stop();
        stateMachine.setBaseState("waiting");
        if (!engine.isPlayingClip()) {
          if (wasSleeping) {
            if (!playFrameClip("sleep")) syncAnimation();
          } else {
            startSleepSequence();
          }
        }
        if (!engine.isPlayingClip()) syncAnimation();
      } else if (lifeResting) {
        // Low energy is a daytime rest state, not the real sleep animation.
        walker.stop();
        stopFrameClip();
        stateMachine.setBaseState("waiting");
        syncAnimation();
      } else if (!dragging && !dragState.petting) {
        stopFrameClip();
        stateMachine.setBaseState("idle");
        if (wasSleeping) {
          if (!startWakeSequence(startWalkingAfterWake)) {
            syncAnimation();
            startWalkingAfterWake();
          }
        } else if (wasResting) {
          if (!paused && settings.wanderEnabled && !settings.quietMode && !autoQuiet) {
            walker.start();
          }
          syncAnimation();
        } else {
          syncAnimation();
        }
      }
      showEmotion(
        payload.state.activity === "sleeping"
          ? "sleeping"
          : payload.state.activity === "resting" || payload.state.energy < 20
            ? "sleepy"
            : payload.state.mood,
        2_400,
      );
      if (
        payload.state.food === 0 &&
        payload.state.activity !== "sleeping" &&
        payload.state.activity !== "resting" &&
        payload.state.energy >= 20
      ) {
        showEmotion("hungry", 2_400);
      }
    },
  );
  await listen<{ petId: string; trigger: DialogueTrigger }>("pet://life-dialogue", ({ payload }) => {
    if (payload.petId === runtime.petId) sayLine(payload.trigger);
  });
  await listen<{ petId: string; toy: string }>("pet://toy-play", ({ payload }) => {
    if (payload.petId !== runtime.petId) return;
    showEffect("star");
    showEmotion("happy");
    playAction("jumping");
  });
  await listen<{ petId: string; message: string }>("pet://context-error", ({ payload }) => {
    if (payload.petId !== runtime.petId) return;
    showSpeech(payload.message, 3_200);
  });
  await listen<{ petId: string; instanceId: string; action: string }>(
    "pet://context-action",
    ({ payload }) => {
      if (payload.petId !== runtime.petId || payload.instanceId !== runtime.instanceId) return;
      const action = payload.action as DialogueTrigger;
      if (action === "feed" || action === "play" || action === "petting") {
        sayLine(action);
        if (action === "feed") {
          spawnFoodVisual();
          showEffect("food");
        } else {
          showEffect(action === "play" ? "star" : "heart");
        }
      } else if (action === "sleep" || action === "wake") {
        sayLine(action);
        showEmotion(action === "sleep" ? "sleeping" : "happy");
        if (action === "sleep") {
          lifeSleeping = true;
          walker.stop();
          stateMachine.setBaseState("waiting");
          startSleepSequence();
        } else if (!dragging) {
          lifeSleeping = false;
          stopFrameClip();
          stateMachine.setBaseState("idle");
          if (!startWakeSequence(startWalkingAfterWake)) startWalkingAfterWake();
        }
      }
      if (action === "play") playAction("jumping");
      if (action === "feed") playAction("waiting");
    },
  );
  await listen<{ snapshot: { session: string }; autoQuiet: boolean; breakReminder: boolean; lowBattery: boolean }>(
    "environment://state",
    ({ payload }) => {
      autoQuiet = payload.autoQuiet;
      if (payload.breakReminder) sayLine("breakReminder");
      if (payload.lowBattery) sayLine("lowBattery");
      if (payload.autoQuiet || payload.snapshot.session !== "active" || lifeSleeping || lifeResting) {
        walker.stop();
        stopFrameClip();
        stateMachine.setBaseState("waiting");
        syncAnimation();
      } else {
        stateMachine.setBaseState("idle");
        if (!paused && !dragging && settings.wanderEnabled && !settings.quietMode) walker.start();
        if (!dragging && !dragState.petting) syncAnimation();
      }
      scheduleIdleSpeech();
    },
  );
  await listen<{ petId: string; milestones: string[] }>("pet://milestone", ({ payload }) => {
    if (payload.petId === runtime.petId) {
      sayLine("milestone");
      showEmotion("happy");
    }
  });
  await listen<{ petId: string; kind: string; mood: string }>("pet://interaction-feedback", ({ payload }) => {
    if (payload.petId !== runtime.petId) return;
    if (payload.kind === "petting") showEmotion(payload.mood);
  });
  await listen<{ requestId: string; petId: string; delta: string }>("chat://delta", ({ payload }) => {
    if (payload.petId !== runtime.petId) return;
    if (chatRequestId !== null && payload.requestId !== chatRequestId) return;
    chatRequestId ??= payload.requestId;
    if (!dragging && !stateMachine.hasAction()) setBaseMode("working");
    chatReply += payload.delta;
    showSpeech(extractSayText(chatReply), 7_000);
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
    stopFrameClip();
    stateMachine.setBaseState("idle");
    playAction("failed");
    showSpeech(`刚才没听清……${payload.message}`, 5_200);
  });

  document.title = initialPet.manifest.displayName;
  try {
    const initialLifeState = await invoke<{ activity: string }>("get_pet_state", {
      petId: runtime.petId,
    });
    lifeSleeping = initialLifeState.activity === "sleeping";
    lifeResting = initialLifeState.activity === "resting";
  } catch (error) {
    console.warn("failed to load initial pet life state:", error);
  }
  engine.play(!paused);
  walker.setSettings(settings.speed, settings.wanderEnabled, settings.quietMode);
  scheduleIdleSpeech();
  void reportRuntimeState(false);
  if (lifeSleeping) {
    walker.stop();
    stateMachine.setBaseState("waiting");
    if (!paused) {
      if (!startSleepSequence()) syncAnimation();
    } else {
      syncAnimation();
    }
  } else if (lifeResting) {
    walker.stop();
    stateMachine.setBaseState("waiting");
    syncAnimation();
  } else if (!paused && settings.wanderEnabled && !settings.quietMode) {
    walker.start();
  }
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

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface SpeechPayload {
  instanceId: string;
  petId: string;
  text: string;
  duration: number;
}

const params = new URLSearchParams(location.search);
const instanceId = params.get("instance") ?? "";
const petId = params.get("petId") ?? "";
const bubble = document.querySelector<HTMLButtonElement>("#bubble")!;
const bubbleText = document.querySelector<HTMLSpanElement>("#bubble-text")!;
let currentPetId = petId;
let hideTimer: number | undefined;

function hideBubble(): void {
  bubble.classList.remove("bubble-visible");
  if (hideTimer !== undefined) globalThis.clearTimeout(hideTimer);
  hideTimer = globalThis.setTimeout(() => {
    bubble.hidden = true;
  }, 170);
}

function showBubble(payload: SpeechPayload): void {
  if (!instanceId || payload.instanceId !== instanceId || !payload.text.trim()) return;
  if (hideTimer !== undefined) globalThis.clearTimeout(hideTimer);
  currentPetId = payload.petId;
  bubbleText.textContent = payload.text.trim();
  bubble.hidden = false;
  requestAnimationFrame(() => bubble.classList.add("bubble-visible"));
  hideTimer = globalThis.setTimeout(hideBubble, Math.max(900, payload.duration));
}

bubble.addEventListener("click", () => {
  if (!currentPetId) return;
  void invoke("open_pet_chat", { petId: currentPetId }).catch((error) => {
    console.warn("failed to open pet chat from speech bubble:", error);
  });
});

async function initialize(): Promise<void> {
  await listen<SpeechPayload>("speech://show", ({ payload }) => showBubble(payload));
  await listen("speech://hide", () => hideBubble());
  if (instanceId) {
    await invoke("pet_speech_ready", { instanceId });
  }
}

void initialize().catch((error) => {
  console.warn("failed to initialize pet speech window:", error);
});

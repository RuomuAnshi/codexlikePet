import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { waitForAppReady } from "./appReady";
import type { AiSettings, ChatMessage } from "./pet/config";

const params = new URLSearchParams(location.search);
const petId = params.get("petId") ?? "sakimiao";
const messagesEl = document.querySelector<HTMLElement>("#messages")!;
const input = document.querySelector<HTMLTextAreaElement>("#input")!;
const composer = document.querySelector<HTMLFormElement>("#composer")!;
const sendButton = document.querySelector<HTMLButtonElement>("#send")!;
const stopButton = document.querySelector<HTMLButtonElement>("#stop")!;
const closeButton = document.querySelector<HTMLButtonElement>("#close")!;
const status = document.querySelector<HTMLElement>("#status")!;
const setup = document.querySelector<HTMLElement>("#setup")!;
const chatWindow = getCurrentWindow();
let activeRequest: string | null = null;
let streamingMessage: HTMLElement | null = null;
let pendingUserMessage: HTMLElement | null = null;
let historyVisible = false;
let historyMessages: ChatMessage[] = [];
const sessionMessages: ChatMessage[] = [];

function setStatus(text: string): void { status.textContent = text; }

function createMessageElement(message: ChatMessage): HTMLElement {
  const element = document.createElement("div");
  const passiveClass = message.source === "heartbeat" || message.source === "pet-conversation"
    ? message.source
    : "";
  element.className = `message ${message.role} ${passiveClass}`;
  const speaker = message.speakerName && message.speakerPetId !== petId
    ? `${message.speakerName}：`
    : "";
  element.textContent = `${speaker}${message.content}`;
  return element;
}

function renderMessages(messages: ChatMessage[]): void {
  messagesEl.replaceChildren(...messages.map(createMessageElement));
  messagesEl.scrollTop = messagesEl.scrollHeight;
}

function addMessage(message: ChatMessage): HTMLElement {
  sessionMessages.push(message);
  const element = createMessageElement(message);
  messagesEl.append(element);
  messagesEl.scrollTop = messagesEl.scrollHeight;
  return element;
}

function lastSessionMessage(): ChatMessage | undefined {
  return sessionMessages[sessionMessages.length - 1];
}

async function resizeChatWindow(showHistory: boolean): Promise<void> {
  await chatWindow.setSize(new LogicalSize(showHistory ? 420 : 320, showHistory ? 560 : 230));
}

async function setHistoryVisible(visible: boolean): Promise<void> {
  historyVisible = visible;
  if (visible) {
    const history = await invoke<{ petId: string; messages: ChatMessage[] }>("get_chat_history", { petId });
    historyMessages = history.messages;
    renderMessages(historyMessages);
  } else {
    renderMessages(sessionMessages);
  }
  document.body.classList.toggle("history-visible", visible);
  await resizeChatWindow(visible);
}

async function load(): Promise<void> {
  const history = await invoke<{ petId: string; messages: ChatMessage[] }>("get_chat_history", { petId });
  historyMessages = history.messages;
  sessionMessages.length = 0;
  renderMessages([]);
  document.body.classList.remove("history-visible");
  await resizeChatWindow(false);
  const ai = await invoke<AiSettings>("get_ai_settings");
  setup.hidden = Boolean(ai.enabled && ai.chatModel?.model);
}

async function send(content: string): Promise<void> {
  if (activeRequest || !content.trim()) return;
  pendingUserMessage = addMessage({
    id: `local-${Date.now()}`,
    role: "user",
    content: content.trim(),
    timestamp: Date.now(),
    source: "chat",
  });
  setStatus("正在思考…");
  sendButton.disabled = true;
  stopButton.hidden = false;
  try {
    const started = await invoke<{ requestId: string }>("send_chat_message", { petId, content });
    activeRequest = started.requestId;
    streamingMessage = null;
    flushPending();
  } catch (error) {
    pendingUserMessage?.remove();
    if (lastSessionMessage()?.id.startsWith("local-")) sessionMessages.pop();
    pendingUserMessage = null;
    setStatus(String(error));
    sendButton.disabled = false;
    stopButton.hidden = true;
  }
}

type PendingEvent =
  | { type: "delta"; payload: { requestId: string; petId: string; delta: string } }
  | { type: "complete"; payload: { requestId: string; petId: string; message: ChatMessage } }
  | { type: "error"; payload: { requestId: string; petId: string; message: string } };
const pendingEvents: PendingEvent[] = [];

function isChatRequest(requestId: string): boolean {
  return requestId.startsWith("req-");
}

function removeStreamingMessage(): void {
  streamingMessage?.remove();
  if (lastSessionMessage()?.id === "stream") sessionMessages.pop();
  streamingMessage = null;
}

function applyDelta(payload: PendingEvent["payload"] & { delta: string }): void {
  if (payload.petId !== petId || payload.requestId !== activeRequest) return;
  streamingMessage ??= addMessage({ id: "stream", role: "assistant", content: "", timestamp: Date.now(), source: "chat" });
  streamingMessage.textContent += payload.delta;
  const streamedMessage = lastSessionMessage();
  if (streamedMessage?.id === "stream") streamedMessage.content += payload.delta;
  messagesEl.scrollTop = messagesEl.scrollHeight;
}

function applyComplete(payload: Extract<PendingEvent, { type: "complete" }>["payload"]): void {
  const passive = payload.message.source === "heartbeat" || payload.message.source === "pet-conversation";
  if (payload.petId !== petId || (payload.requestId !== activeRequest && !passive)) {
    if (payload.petId === petId && passive) addMessage(payload.message);
    return;
  }
  if (streamingMessage) {
    streamingMessage.textContent = payload.message.content;
    const streamedMessage = lastSessionMessage();
    if (streamedMessage?.id === "stream") streamedMessage.content = payload.message.content;
  } else {
    addMessage(payload.message);
  }
  pendingUserMessage = null;
  activeRequest = null;
  streamingMessage = null;
  sendButton.disabled = false;
  stopButton.hidden = true;
  setStatus("");
}

function applyError(payload: Extract<PendingEvent, { type: "error" }>["payload"]): void {
  if (payload.petId !== petId || payload.requestId !== activeRequest) return;
  activeRequest = null;
  removeStreamingMessage();
  pendingUserMessage = null;
  sendButton.disabled = false;
  stopButton.hidden = true;
  setStatus(payload.message);
}

function flushPending(): void {
  for (const event of pendingEvents.splice(0)) {
    if (event.type === "delta") applyDelta(event.payload);
    if (event.type === "complete") applyComplete(event.payload);
    if (event.type === "error") applyError(event.payload);
  }
}

async function bootChat(): Promise<void> {
  await listen<{ requestId: string; petId: string; delta: string }>("chat://delta", ({ payload }) => {
    if (payload.petId !== petId) return;
    if (activeRequest === null && isChatRequest(payload.requestId)) {
      pendingEvents.push({ type: "delta", payload });
      return;
    }
    applyDelta(payload);
  });
  await listen<{ requestId: string; petId: string; message: ChatMessage }>("chat://complete", ({ payload }) => {
    if (payload.petId !== petId) return;
    if (activeRequest === null && isChatRequest(payload.requestId)) {
      pendingEvents.push({ type: "complete", payload });
      return;
    }
    applyComplete(payload);
  });
  await listen<{ requestId: string; petId: string; message: string }>("chat://error", ({ payload }) => {
    if (payload.petId !== petId) return;
    if (activeRequest === null && isChatRequest(payload.requestId)) {
      pendingEvents.push({ type: "error", payload });
      return;
    }
    applyError(payload);
  });

  composer.addEventListener("submit", (event) => {
    event.preventDefault();
    const value = input.value.trim();
    input.value = "";
    void send(value);
  });
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      composer.requestSubmit();
    }
  });
  stopButton.addEventListener("click", async () => {
    await invoke("cancel_chat_response", { petId });
    activeRequest = null;
    removeStreamingMessage();
    sendButton.disabled = false;
    stopButton.hidden = true;
    setStatus("已停止");
  });
  // The native close button is disabled on this window (WebView2 teardown can
  // leave a ghost HWND behind), so closing goes through this command instead.
  closeButton.addEventListener("click", () => {
    closeButton.disabled = true;
    void invoke("hide_pet_chat", { petId })
      .catch((error) => setStatus(String(error)))
      .finally(() => {
        closeButton.disabled = false;
      });
  });
  const openSettings = (): void => { void invoke("open_ai_settings"); };
  document.querySelector("#setup-button")?.addEventListener("click", openSettings);
  document.addEventListener("keydown", (event) => {
    if (!(event.metaKey || event.ctrlKey) || event.key.toLowerCase() !== "h") return;
    event.preventDefault();
    if (activeRequest) return;
    void setHistoryVisible(!historyVisible).catch((error) => setStatus(String(error)));
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && historyVisible && !activeRequest) {
      void setHistoryVisible(false).catch((error) => setStatus(String(error)));
    }
  });
  document.title = "SakiPet";
  await waitForAppReady();
  await load();
}

void bootChat();

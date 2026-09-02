import { invoke } from "@tauri-apps/api/core";
import { waitForAppReady } from "./appReady";
import { confirmDialog, promptDialog } from "./ui/confirm";
import type {
  AiSettings,
  InstalledPetInfo,
  MemoryFact,
  ModelEndpointConfig,
  ProviderKind,
} from "./pet/config";

const PROVIDER_OPTIONS: Array<{ value: ProviderKind; label: string }> = [
  { value: "openai-responses", label: "OpenAI Responses" },
  { value: "anthropic-messages", label: "Anthropic Messages" },
  { value: "openai-compatible", label: "OpenAI-compatible" },
];

let currentAiSettings: AiSettings | null = null;
let pets: InstalledPetInfo[] = [];

const status = document.querySelector<HTMLElement>("#status")!;
const openPetManagerButton = document.querySelector<HTMLButtonElement>("#open-pet-manager")!;
const aiEnabled = document.querySelector<HTMLInputElement>("#ai-enabled")!;
const chatProvider = document.querySelector<HTMLSelectElement>("#chat-provider")!;
const chatModel = document.querySelector<HTMLInputElement>("#chat-model")!;
const chatBaseUrl = document.querySelector<HTMLInputElement>("#chat-base-url")!;
const chatKey = document.querySelector<HTMLInputElement>("#chat-key")!;
const clearChatKey = document.querySelector<HTMLButtonElement>("#clear-chat-key")!;
const visionEnabled = document.querySelector<HTMLInputElement>("#vision-enabled")!;
const visionProvider = document.querySelector<HTMLSelectElement>("#vision-provider")!;
const visionModel = document.querySelector<HTMLInputElement>("#vision-model")!;
const visionBaseUrl = document.querySelector<HTMLInputElement>("#vision-base-url")!;
const visionKey = document.querySelector<HTMLInputElement>("#vision-key")!;
const clearVisionKey = document.querySelector<HTMLButtonElement>("#clear-vision-key")!;
const memoryEnabled = document.querySelector<HTMLInputElement>("#memory-enabled")!;
const heartbeatEnabled = document.querySelector<HTMLInputElement>("#heartbeat-enabled")!;
const petConversationEnabled = document.querySelector<HTMLInputElement>("#pet-conversation-enabled")!;
const desktopVisionEnabled = document.querySelector<HTMLInputElement>("#desktop-vision-enabled")!;
const maxRecentMessages = document.querySelector<HTMLInputElement>("#max-recent-messages")!;
const heartbeatMinutes = document.querySelector<HTMLInputElement>("#heartbeat-minutes")!;
const heartbeatMaxMinutes = document.querySelector<HTMLInputElement>("#heartbeat-max-minutes")!;
const heartbeatVisionChance = document.querySelector<HTMLInputElement>("#heartbeat-vision-chance")!;
const saveAiButton = document.querySelector<HTMLButtonElement>("#save-ai-settings")!;
const testChatButton = document.querySelector<HTMLButtonElement>("#test-chat-provider")!;
const testVisionButton = document.querySelector<HTMLButtonElement>("#test-vision-provider")!;
const aiStatus = document.querySelector<HTMLElement>("#ai-status")!;
const memoryPet = document.querySelector<HTMLSelectElement>("#memory-pet")!;
const memoryList = document.querySelector<HTMLElement>("#memory-list")!;
const refreshMemoriesButton = document.querySelector<HTMLButtonElement>("#refresh-memories")!;
const clearMemoriesButton = document.querySelector<HTMLButtonElement>("#clear-memories")!;
const memoryForm = document.querySelector<HTMLFormElement>("#memory-form")!;
const newMemory = document.querySelector<HTMLInputElement>("#new-memory")!;

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function setStatus(message: string, kind: "normal" | "error" = "normal"): void {
  status.textContent = message;
  status.dataset.kind = kind;
}

function setBusy(button: HTMLButtonElement, busy: boolean): void {
  button.disabled = busy;
  if (busy) button.dataset.previousText = button.textContent ?? "";
  if (!busy && button.dataset.previousText) button.textContent = button.dataset.previousText;
}

function setupProviderSelect(select: HTMLSelectElement): void {
  select.replaceChildren(
    ...PROVIDER_OPTIONS.map(({ value, label }) => {
      const option = document.createElement("option");
      option.value = value;
      option.textContent = label;
      return option;
    }),
  );
}

function populateAiSettings(settings: AiSettings): void {
  currentAiSettings = settings;
  const chat = settings.chatModel;
  const vision = settings.visionModel;
  aiEnabled.checked = settings.enabled;
  chatProvider.value = chat?.provider ?? "openai-responses";
  chatModel.value = chat?.model ?? "";
  chatBaseUrl.value = chat?.baseUrl ?? "https://api.openai.com/v1";
  visionEnabled.checked = vision !== null;
  visionProvider.value = vision?.provider ?? chat?.provider ?? "openai-compatible";
  visionModel.value = vision?.model ?? "";
  visionBaseUrl.value = vision?.baseUrl ?? "";
  memoryEnabled.checked = settings.memoryEnabled;
  heartbeatEnabled.checked = settings.heartbeatEnabled;
  petConversationEnabled.checked = settings.petConversationEnabled;
  desktopVisionEnabled.checked = settings.desktopVisionEnabled;
  maxRecentMessages.value = String(settings.maxRecentMessages);
  heartbeatMinutes.value = String(settings.heartbeatMinMinutes);
  heartbeatMaxMinutes.value = String(settings.heartbeatMaxMinutes);
  heartbeatVisionChance.value = String(settings.heartbeatVisionChance);
  aiStatus.textContent = "桌面截图只会在内存中短暂处理，不会保存到磁盘。";
}

function endpointFromForm(
  provider: HTMLSelectElement,
  baseUrl: HTMLInputElement,
  model: HTMLInputElement,
  credentialRef: string,
  fallbackBaseUrl = "",
): ModelEndpointConfig | null {
  const modelName = model.value.trim();
  if (!modelName) return null;
  return {
    provider: provider.value as ProviderKind,
    baseUrl: (baseUrl.value.trim() || fallbackBaseUrl.trim()).replace(/\/$/, ""),
    model: modelName,
    credentialRef: credentialRef || null,
    maxOutputTokens: 1024,
  };
}

function numericValue(input: HTMLInputElement, fallback: number): number {
  const value = Number(input.value);
  return Number.isFinite(value) ? value : fallback;
}

async function saveAiSettings(): Promise<void> {
  const previous = currentAiSettings ?? {
    enabled: false,
    chatModel: null,
    visionModel: null,
    memoryEnabled: true,
    maxRecentMessages: 12,
    heartbeatEnabled: true,
    heartbeatMinMinutes: 20,
    heartbeatMaxMinutes: 60,
    heartbeatVisionChance: 0.3,
    desktopVisionEnabled: false,
    petConversationEnabled: true,
  } satisfies AiSettings;
  const chatCredentialRef = previous.chatModel?.credentialRef ?? "chat-api-key";
  const chat = endpointFromForm(chatProvider, chatBaseUrl, chatModel, chatCredentialRef);
  if (!chat) throw new Error("请先填写聊天模型名称");

  const visionCredentialRef = visionKey.value.trim()
    ? "vision-api-key"
    : (previous.visionModel?.credentialRef ?? chat.credentialRef ?? "vision-api-key");
  const vision = visionEnabled.checked
    ? endpointFromForm(
        visionProvider,
        visionBaseUrl,
        visionModel,
        visionCredentialRef,
        chatBaseUrl.value,
      )
    : null;
  if (visionEnabled.checked && !vision) throw new Error("已启用独立视觉模型，请填写模型名称");

  if (chatKey.value) {
    await invoke("set_ai_secret", { reference: "chat-api-key", secret: chatKey.value });
  }
  if (visionKey.value) {
    await invoke("set_ai_secret", { reference: "vision-api-key", secret: visionKey.value });
  }
  const saved = await invoke<AiSettings>("update_ai_settings", {
    settings: {
      enabled: aiEnabled.checked,
      chatModel: chat,
      visionModel: vision,
      memoryEnabled: memoryEnabled.checked,
      maxRecentMessages: numericValue(maxRecentMessages, previous.maxRecentMessages),
      heartbeatEnabled: heartbeatEnabled.checked,
      heartbeatMinMinutes: numericValue(heartbeatMinutes, previous.heartbeatMinMinutes),
      heartbeatMaxMinutes: numericValue(heartbeatMaxMinutes, previous.heartbeatMaxMinutes),
      heartbeatVisionChance: numericValue(heartbeatVisionChance, previous.heartbeatVisionChance),
      desktopVisionEnabled: desktopVisionEnabled.checked,
      petConversationEnabled: petConversationEnabled.checked,
    },
  });
  populateAiSettings(saved);
  chatKey.value = "";
  visionKey.value = "";
  setStatus("AI 设置已保存");
}

async function testAiProvider(vision: boolean): Promise<void> {
  const chat = endpointFromForm(chatProvider, chatBaseUrl, chatModel, "chat-api-key");
  const endpoint = vision
    ? endpointFromForm(
        visionProvider,
        visionBaseUrl,
        visionModel,
        "vision-api-key",
        chatBaseUrl.value,
      )
    : chat;
  if (!endpoint) {
    aiStatus.textContent = vision ? "请先填写视觉模型名称" : "请先填写聊天模型名称";
    return;
  }
  if (vision && !visionEnabled.checked) {
    aiStatus.textContent = "请先勾选“使用独立视觉模型”";
    return;
  }
  const key = vision ? visionKey.value : chatKey.value;
  if (key) await invoke("set_ai_secret", { reference: endpoint.credentialRef, secret: key });
  const button = vision ? testVisionButton : testChatButton;
  setBusy(button, true);
  aiStatus.textContent = "正在测试模型连接…";
  try {
    const result = await invoke<string>("test_ai_provider", { config: endpoint, vision });
    aiStatus.textContent = `连接成功：${result || "模型已响应"}`;
  } catch (error) {
    aiStatus.textContent = `连接失败：${errorMessage(error)}`;
  } finally {
    setBusy(button, false);
  }
}

function populateMemoryPets(): void {
  const selected = memoryPet.value || "__shared__";
  memoryPet.replaceChildren();
  const shared = document.createElement("option");
  shared.value = "__shared__";
  shared.textContent = "共享用户资料";
  memoryPet.append(shared);
  for (const pet of pets) {
    const option = document.createElement("option");
    option.value = pet.id;
    option.textContent = pet.displayName || pet.id;
    memoryPet.append(option);
  }
  memoryPet.value = [...memoryPet.options].some((option) => option.value === selected)
    ? selected
    : "__shared__";
}

function memoryScope(): "shared" | "pet" {
  return memoryPet.value === "__shared__" ? "shared" : "pet";
}

function renderMemories(memories: MemoryFact[]): void {
  memoryList.replaceChildren();
  if (!memories.length) {
    const empty = document.createElement("p");
    empty.className = "helper-text memory-empty";
    empty.textContent = "还没有记忆。聊天后，AI 会自动提取重要信息；也可以在下方手动添加。";
    memoryList.append(empty);
    return;
  }
  for (const memory of memories) {
    const row = document.createElement("article");
    row.className = "memory-row";
    const text = document.createElement("p");
    text.textContent = memory.content;
    const meta = document.createElement("small");
    meta.textContent = `${memory.kind || "fact"} · 重要度 ${Math.round(memory.importance * 100)}%`;
    const edit = document.createElement("button");
    edit.type = "button";
    edit.className = "small-button";
    edit.textContent = "编辑";
    edit.addEventListener("click", async () => {
      const content = (await promptDialog(memory.content, { title: "修改这条记忆" }))?.trim();
      if (!content || content === memory.content) return;
      try {
        await invoke(memoryScope() === "shared" ? "update_shared_memory" : "update_memory", {
          ...(memoryScope() === "shared" ? {} : { petId: memoryPet.value }),
          memory: { ...memory, content, updatedAt: Date.now() },
        });
        await refreshMemories();
      } catch (error) {
        setStatus(errorMessage(error), "error");
      }
    });
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "small-button danger-button";
    remove.textContent = "删除";
    remove.addEventListener("click", async () => {
      if (!(await confirmDialog("删除这条记忆吗？"))) return;
      try {
        await invoke(memoryScope() === "shared" ? "delete_shared_memory" : "delete_memory", {
          ...(memoryScope() === "shared" ? {} : { petId: memoryPet.value }),
          memoryId: memory.id,
        });
        await refreshMemories();
      } catch (error) {
        setStatus(errorMessage(error), "error");
      }
    });
    const actions = document.createElement("div");
    actions.className = "memory-row-actions";
    actions.append(edit, remove);
    row.append(text, meta, actions);
    memoryList.append(row);
  }
}

async function refreshMemories(): Promise<void> {
  try {
    const memories = await invoke<MemoryFact[]>(
      memoryScope() === "shared" ? "get_shared_memories" : "get_memories",
      memoryScope() === "shared" ? {} : { petId: memoryPet.value },
    );
    renderMemories(memories);
  } catch (error) {
    memoryList.replaceChildren();
    const message = document.createElement("p");
    message.className = "helper-text";
    message.textContent = errorMessage(error);
    memoryList.append(message);
  }
}

async function addManualMemory(): Promise<void> {
  const content = newMemory.value.trim();
  if (!content) return;
  const now = Date.now();
  const memory: MemoryFact = {
    id: `manual-${now}`,
    content,
    kind: "manual",
    scope: memoryScope(),
    importance: 0.8,
    confidence: 1,
    createdAt: now,
    updatedAt: now,
    status: "active",
  };
  await invoke(memoryScope() === "shared" ? "update_shared_memory" : "update_memory", {
    ...(memoryScope() === "shared" ? {} : { petId: memoryPet.value }),
    memory,
  });
  newMemory.value = "";
  await refreshMemories();
}

async function load(): Promise<void> {
  try {
    const [settings, catalog] = await Promise.all([
      invoke<AiSettings>("get_ai_settings"),
      invoke<InstalledPetInfo[]>("get_pet_catalog"),
    ]);
    pets = catalog;
    setupProviderSelect(chatProvider);
    setupProviderSelect(visionProvider);
    populateAiSettings(settings);
    populateMemoryPets();
    await refreshMemories();
    setStatus("AI 设置已加载");
  } catch (error) {
    setStatus(errorMessage(error), "error");
  }
}

openPetManagerButton.addEventListener("click", async () => {
  setBusy(openPetManagerButton, true);
  try {
    await invoke("open_pet_manager");
  } catch (error) {
    setStatus(errorMessage(error), "error");
  } finally {
    setBusy(openPetManagerButton, false);
  }
});
saveAiButton.addEventListener("click", () => {
  setBusy(saveAiButton, true);
  void saveAiSettings()
    .catch((error) => setStatus(errorMessage(error), "error"))
    .finally(() => setBusy(saveAiButton, false));
});
testChatButton.addEventListener("click", () => void testAiProvider(false));
testVisionButton.addEventListener("click", () => void testAiProvider(true));
const listChatModelsButton = document.querySelector<HTMLButtonElement>("#list-chat-models")!;
const chatModelSuggestions = document.querySelector<HTMLDataListElement>("#chat-model-suggestions")!;
listChatModelsButton.addEventListener("click", async () => {
  const endpoint = endpointFromForm(
    chatProvider,
    chatBaseUrl,
    chatModel,
    "chat-api-key",
  );
  if (!chatModel.value.trim() && !endpoint) {
    aiStatus.textContent = "请先填写模型名称或 Base URL";
    return;
  }
  if (endpoint?.provider === "anthropic-messages") {
    aiStatus.textContent = "Anthropic Messages 协议没有标准的模型列表接口。";
    return;
  }
  setBusy(listChatModelsButton, true);
  aiStatus.textContent = "正在拉取模型列表…";
  try {
    if (chatKey.value) {
      await invoke("set_ai_secret", { reference: "chat-api-key", secret: chatKey.value });
    }
    const models = await invoke<string[]>("list_models", {
      config: endpoint ?? {
        provider: chatProvider.value as ProviderKind,
        baseUrl: chatBaseUrl.value || "https://api.openai.com/v1",
        model: chatModel.value || "",
        credentialRef: "chat-api-key",
        maxOutputTokens: 1024,
      },
    });
    chatModelSuggestions.replaceChildren(
      ...models.map((id) => {
        const option = document.createElement("option");
        option.value = id;
        return option;
      }),
    );
    if (!chatModel.value.trim() && models.length > 0) chatModel.value = models[0];
    aiStatus.textContent = `已拉取 ${models.length} 个模型，可从下拉选择。`;
  } catch (error) {
    aiStatus.textContent = `拉取失败：${errorMessage(error)}`;
  } finally {
    setBusy(listChatModelsButton, false);
  }
});
clearChatKey.addEventListener("click", async () => {
  if (!(await confirmDialog("删除已保存的聊天 API Key 吗？"))) return;
  try {
    await invoke("delete_ai_secret", { reference: "chat-api-key" });
    chatKey.value = "";
    aiStatus.textContent = "聊天 API Key 已删除。";
  } catch (error) {
    aiStatus.textContent = `删除失败：${errorMessage(error)}`;
  }
});
clearVisionKey.addEventListener("click", async () => {
  if (!(await confirmDialog("删除已保存的视觉 API Key 吗？"))) return;
  try {
    await invoke("delete_ai_secret", { reference: "vision-api-key" });
    visionKey.value = "";
    aiStatus.textContent = "视觉 API Key 已删除。";
  } catch (error) {
    aiStatus.textContent = `删除失败：${errorMessage(error)}`;
  }
});
memoryPet.addEventListener("change", () => void refreshMemories());
refreshMemoriesButton.addEventListener("click", () => void refreshMemories());
clearMemoriesButton.addEventListener("click", async () => {
  if (!(await confirmDialog("清空当前范围的全部记忆吗？此操作不可恢复。"))) return;
  try {
    await invoke(
      memoryScope() === "shared" ? "clear_shared_memories" : "clear_memories",
      memoryScope() === "shared" ? {} : { petId: memoryPet.value },
    );
    await refreshMemories();
    setStatus("记忆已清空");
  } catch (error) {
    setStatus(errorMessage(error), "error");
  }
});
memoryForm.addEventListener("submit", (event) => {
  event.preventDefault();
  void addManualMemory().catch((error) => setStatus(errorMessage(error), "error"));
});

void waitForAppReady()
  .then(() => load())
  .catch((error) => setStatus(errorMessage(error), "error"));

import { invoke } from "@tauri-apps/api/core";

interface SocialLogDialogue { petId: string; text: string; }
interface SocialLogEntry {
  timestamp: number;
  participants: string[];
  interactionType: string;
  trigger: string;
  prop?: string | null;
  dialogue: SocialLogDialogue[];
  milestones: string[];
}
interface PublicRelationship {
  firstPetId: string;
  secondPetId: string;
  affinity: number;
  level: number;
  interactionCount: number;
  romanceStatus: string;
}
interface PetPreview {
  id: string;
  displayName: string;
  description: string;
  spriteVersionNumber: number;
  spritesheetDataUrl: string;
}

const petFilter = document.querySelector<HTMLSelectElement>("#pet-filter")!;
const typeFilter = document.querySelector<HTMLSelectElement>("#type-filter")!;
const status = document.querySelector<HTMLElement>("#status")!;
const logList = document.querySelector<HTMLElement>("#log-list")!;
const relationshipList = document.querySelector<HTMLElement>("#relationship-list")!;
const previewCanvas = document.querySelector<HTMLCanvasElement>("#preview-canvas")!;
const previewContext = previewCanvas.getContext("2d")!;
const previewTitle = document.querySelector<HTMLElement>("#preview-title")!;
const previewDescription = document.querySelector<HTMLElement>("#preview-description")!;
const previewMeta = document.querySelector<HTMLElement>("#preview-meta")!;
const names: Record<string, string> = {};
const previews = new Map<string, PetPreview>();
const previewImages = new Map<string, HTMLImageElement>();
let logs: SocialLogEntry[] = [];
let relationships: PublicRelationship[] = [];
let selectedPetId = "";
let previewTimer: number | undefined;
let previewRequest = 0;

const displayName = (id: string): string => names[id] ?? id;
const relationshipName = (level: number): string =>
  ["初识", "熟悉", "亲近", "信赖", "挚友"][Math.max(0, Math.min(4, level - 1))] ?? "初识";

function setStatus(message: string, kind: "normal" | "error" = "normal"): void {
  status.textContent = message;
  status.dataset.kind = kind;
}

function petButton(petId: string): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "pet-link";
  button.dataset.petId = petId;
  button.textContent = displayName(petId);
  button.title = `预览${displayName(petId)}`;
  button.setAttribute("aria-label", `预览${displayName(petId)}`);
  button.setAttribute("aria-pressed", String(selectedPetId === petId));
  button.addEventListener("click", () => void selectPetPreview(petId));
  return button;
}

function updatePreviewButtons(): void {
  document.querySelectorAll<HTMLButtonElement>(".pet-link").forEach((button) => {
    button.setAttribute("aria-pressed", String(button.dataset.petId === selectedPetId));
  });
}

function drawPreviewFrame(image: HTMLImageElement, frame: number): void {
  previewContext.clearRect(0, 0, previewCanvas.width, previewCanvas.height);
  previewContext.drawImage(image, frame * 192, 0, 192, 208, 0, 0, 192, 208);
}

function stopPreviewAnimation(): void {
  if (previewTimer !== undefined) {
    window.clearInterval(previewTimer);
    previewTimer = undefined;
  }
}

async function selectPetPreview(petId: string): Promise<void> {
  selectedPetId = petId;
  updatePreviewButtons();
  stopPreviewAnimation();
  previewContext.clearRect(0, 0, previewCanvas.width, previewCanvas.height);
  previewTitle.textContent = displayName(petId);
  previewDescription.textContent = "正在加载预览……";
  previewMeta.textContent = "";
  const request = ++previewRequest;

  try {
    const preview = previews.get(petId) ?? await invoke<PetPreview>("get_pet_preview", { petId });
    if (request !== previewRequest) return;
    previews.set(petId, preview);
    previewTitle.textContent = preview.displayName || displayName(petId);
    previewDescription.textContent = preview.description || "这只宠物还没有写下介绍。";
    const relationCount = relationships.filter((item) =>
      item.firstPetId === petId || item.secondPetId === petId,
    ).length;
    previewMeta.textContent = relationCount
      ? `已记录 ${relationCount} 段宠物关系`
      : "暂时还没有宠物关系记录";

    let image = previewImages.get(petId);
    if (!image) {
      image = new Image();
      image.src = preview.spritesheetDataUrl;
      previewImages.set(petId, image);
    }
    await new Promise<void>((resolve, reject) => {
      if (image?.complete && image.naturalWidth > 0) {
        resolve();
      } else {
        image?.addEventListener("load", () => resolve(), { once: true });
        image?.addEventListener("error", () => reject(new Error("预览图片加载失败")), { once: true });
      }
    });
    if (request !== previewRequest || !image) return;
    drawPreviewFrame(image, 0);
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    let frame = 0;
    previewTimer = window.setInterval(() => {
      frame = (frame + 1) % 6;
      drawPreviewFrame(image!, frame);
    }, 260);
  } catch (error) {
    if (request !== previewRequest) return;
    previewTitle.textContent = displayName(petId);
    previewDescription.textContent = "这只宠物的资源暂时无法预览。";
    previewMeta.textContent = String(error);
  }
}

function renderRelationships(relationships: PublicRelationship[]): void {
  relationshipList.replaceChildren();
  for (const relationship of relationships) {
    const card = document.createElement("article");
    card.className = "relationship-card";
    const title = document.createElement("div");
    title.className = "relationship-title";
    const pair = document.createElement("span");
    pair.className = "relationship-pair";
    pair.append(
      petButton(relationship.firstPetId),
      document.createTextNode(" × "),
      petButton(relationship.secondPetId),
    );
    const level = document.createElement("strong");
    level.className = "relationship-level";
    level.textContent = relationship.romanceStatus === "dating"
      ? "恋爱中"
      : relationshipName(relationship.level);
    title.append(pair, level);
    const meta = document.createElement("div");
    meta.className = "relationship-meta";
    meta.textContent = "好感 " + relationship.affinity + "/100 · 互动 " + relationship.interactionCount + " 次";
    card.append(title, meta);
    relationshipList.append(card);
  }
}

function renderLogs(): void {
  const pet = petFilter.value;
  const type = typeFilter.value;
  logList.replaceChildren();
  const filtered = logs.filter((entry) =>
    (!pet || entry.participants.includes(pet)) &&
    (!type || entry.interactionType === type)
  );
  for (const entry of filtered) {
    const card = document.createElement("article");
    card.className = "log-card";
    const header = document.createElement("header");
    const kind = document.createElement("span");
    kind.className = "log-kind";
    kind.textContent = entry.prop
      ? `${entry.interactionType} · ${entry.prop}`
      : entry.interactionType;
    const time = document.createElement("span");
    time.className = "log-time";
    time.textContent = new Date(entry.timestamp).toLocaleString();
    header.append(kind, time);
    const people = document.createElement("div");
    people.className = "relationship-meta";
    people.append(document.createTextNode(entry.trigger + " · "));
    entry.participants.forEach((petId, index) => {
      if (index) people.append(document.createTextNode("、"));
      people.append(petButton(petId));
    });
    const dialogue = document.createElement("div");
    dialogue.className = "log-dialogue";
    for (const line of entry.dialogue) {
      const bubble = document.createElement("div");
      bubble.className = "bubble";
      bubble.append(petButton(line.petId), document.createTextNode("：" + line.text));
      dialogue.append(bubble);
    }
    card.append(header, people, dialogue);
    if (entry.milestones.length) {
      const milestones = document.createElement("div");
      milestones.className = "milestones";
      milestones.textContent = "里程碑：" + entry.milestones.join("、");
      card.append(milestones);
    }
    logList.append(card);
  }
  setStatus(filtered.length ? "共 " + filtered.length + " 条记录" : "还没有宠物社交记录。");
}

async function reload(): Promise<void> {
  try {
    const [catalog, nextLogs, nextRelationships] = await Promise.all([
      invoke<Array<{ id: string; displayName: string }>>("get_pet_catalog"),
      invoke<SocialLogEntry[]>("get_social_log", {
        petId: null,
        secondPetId: null,
        interactionType: null,
        fromMs: null,
        toMs: null,
      }),
      invoke<PublicRelationship[]>("get_public_relationships"),
    ]);
    for (const pet of catalog) names[pet.id] = pet.displayName;
    logs = nextLogs;
    relationships = nextRelationships;
    const petValues = [...new Set(logs.flatMap((entry) => entry.participants))].sort();
    petFilter.replaceChildren(
      new Option("全部", ""),
      ...petValues.map((id) => new Option(displayName(id), id)),
    );
    const types = [...new Set(logs.map((entry) => entry.interactionType))].sort();
    typeFilter.replaceChildren(
      new Option("全部", ""),
      ...types.map((type) => new Option(type, type)),
    );
    renderRelationships(relationships);
    const knownPetIds = catalog.map((pet) => pet.id);
    const fallbackPetId = knownPetIds[0] ?? logs.flatMap((entry) => entry.participants)[0] ?? "";
    if (fallbackPetId && !knownPetIds.includes(selectedPetId)) {
      await selectPetPreview(fallbackPetId);
    } else if (!fallbackPetId) {
      selectedPetId = "";
      previewTitle.textContent = "还没有宠物";
      previewDescription.textContent = "导入宠物后，就可以在这里预览它。";
      previewMeta.textContent = "";
      stopPreviewAnimation();
    }
    renderLogs();
  } catch (error) {
    setStatus(String(error), "error");
  }
}

petFilter.addEventListener("change", () => {
  renderLogs();
  if (petFilter.value) void selectPetPreview(petFilter.value);
});
typeFilter.addEventListener("change", renderLogs);
document.querySelector("#refresh")?.addEventListener("click", () => void reload());
document.querySelector("#clear-log")?.addEventListener("click", async () => {
  if (!window.confirm("确定清空所有宠物社交日志吗？")) return;
  await invoke("clear_social_log");
  await reload();
});
void reload();

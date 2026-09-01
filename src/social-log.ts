import { invoke } from "@tauri-apps/api/core";

interface SocialLogDialogue { petId: string; text: string; }
interface SocialLogEntry {
  timestamp: number;
  participants: string[];
  interactionType: string;
  trigger: string;
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

const petFilter = document.querySelector<HTMLSelectElement>("#pet-filter")!;
const typeFilter = document.querySelector<HTMLSelectElement>("#type-filter")!;
const status = document.querySelector<HTMLElement>("#status")!;
const logList = document.querySelector<HTMLElement>("#log-list")!;
const relationshipList = document.querySelector<HTMLElement>("#relationship-list")!;
const names: Record<string, string> = {};
let logs: SocialLogEntry[] = [];

const displayName = (id: string): string => names[id] ?? id;
const relationshipName = (level: number): string =>
  ["初识", "熟悉", "亲近", "信赖", "挚友"][Math.max(0, Math.min(4, level - 1))] ?? "初识";

function setStatus(message: string, kind: "normal" | "error" = "normal"): void {
  status.textContent = message;
  status.dataset.kind = kind;
}

function renderRelationships(relationships: PublicRelationship[]): void {
  relationshipList.replaceChildren();
  for (const relationship of relationships) {
    const card = document.createElement("article");
    card.className = "relationship-card";
    const title = document.createElement("div");
    title.className = "relationship-title";
    title.textContent = displayName(relationship.firstPetId) + " × " + displayName(relationship.secondPetId);
    const level = document.createElement("strong");
    level.textContent = relationship.romanceStatus === "dating"
      ? "恋爱中"
      : relationshipName(relationship.level);
    title.append(level);
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
    kind.textContent = entry.interactionType;
    const time = document.createElement("span");
    time.className = "log-time";
    time.textContent = new Date(entry.timestamp).toLocaleString();
    header.append(kind, time);
    const people = document.createElement("div");
    people.className = "relationship-meta";
    people.textContent = entry.trigger + " · " + entry.participants.map(displayName).join("、");
    const dialogue = document.createElement("div");
    dialogue.className = "log-dialogue";
    for (const line of entry.dialogue) {
      const bubble = document.createElement("div");
      bubble.className = "bubble";
      bubble.textContent = displayName(line.petId) + "：" + line.text;
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
    const [catalog, nextLogs, relationships] = await Promise.all([
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
    renderLogs();
  } catch (error) {
    setStatus(String(error), "error");
  }
}

petFilter.addEventListener("change", renderLogs);
typeFilter.addEventListener("change", renderLogs);
document.querySelector("#refresh")?.addEventListener("click", () => void reload());
document.querySelector("#clear-log")?.addEventListener("click", async () => {
  if (!window.confirm("确定清空所有宠物社交日志吗？")) return;
  await invoke("clear_social_log");
  await reload();
});
void reload();

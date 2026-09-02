import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { waitForAppReady } from "./appReady";
import { confirmDialog } from "./ui/confirm";
import { CELL_HEIGHT, CELL_WIDTH, type PetManifest } from "./pet/atlas";
import type {
  InstalledPetInfo,
  PetLifeState,
  PetInstanceInfo,
  PetSettings,
} from "./pet/config";

const PETS_BASE = import.meta.env.BASE_URL + "pets";
const PREVIEW_WIDTH = 96;
const PREVIEW_HEIGHT = 104;

interface LoadedPet {
  info: InstalledPetInfo;
  manifest: PetManifest | null;
  preview: HTMLCanvasElement | null;
  life: PetLifeState | null;
  error?: string;
}

let pets: LoadedPet[] = [];
let instances: PetInstanceInfo[] = [];

const list = document.querySelector<HTMLElement>("#pet-list")!;
const status = document.querySelector<HTMLElement>("#status")!;
const refreshButton = document.querySelector<HTMLButtonElement>("#refresh")!;
const importButton = document.querySelector<HTMLButtonElement>("#import-pet")!;
const importInput = document.querySelector<HTMLInputElement>("#pet-package")!;
const openAiSettingsButton = document.querySelector<HTMLButtonElement>("#open-ai-settings")!;
const openEnvironmentSettingsButton = document.querySelector<HTMLButtonElement>("#open-environment-settings")!;
const openSocialSettingsButton = document.querySelector<HTMLButtonElement>("#open-social-settings")!;
const openSocialLogButton = document.querySelector<HTMLButtonElement>("#open-social-log")!;
const settingsDialog = document.querySelector<HTMLDialogElement>("#pet-settings-dialog")!;
const settingsDialogTitle = document.querySelector<HTMLElement>("#settings-dialog-title")!;
const settingsDialogContent = document.querySelector<HTMLElement>("#settings-dialog-content")!;
const closeSettingsDialogButton = document.querySelector<HTMLButtonElement>("#close-settings-dialog")!;

function setStatus(message: string, kind: "normal" | "error" = "normal"): void {
  status.textContent = message;
  status.dataset.kind = kind;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function setBusy(button: HTMLButtonElement, busy: boolean): void {
  button.disabled = busy;
  if (busy) button.dataset.previousText = button.textContent ?? "";
  if (!busy && button.dataset.previousText) button.textContent = button.dataset.previousText;
}

async function loadManifest(info: InstalledPetInfo): Promise<PetManifest> {
  if (info.source === "imported") {
    return {
      id: info.id,
      displayName: info.displayName,
      description: info.description,
      spriteVersionNumber: info.spriteVersionNumber,
      spritesheetPath: info.spritesheetPath,
    };
  }
  if (!info.path) throw new Error("内置宠物缺少资源路径");
  const response = await fetch(`${PETS_BASE}/${info.path}/pet.json`);
  if (!response.ok) throw new Error(`pet.json 请求失败：${response.status}`);
  const manifest = (await response.json()) as PetManifest;
  if (
    manifest.id !== info.id ||
    manifest.spriteVersionNumber !== 2 ||
    typeof manifest.displayName !== "string" ||
    typeof manifest.description !== "string" ||
    typeof manifest.spritesheetPath !== "string"
  ) {
    throw new Error("不是有效的 V2 宠物资源");
  }
  return manifest;
}

function loadPreview(info: InstalledPetInfo, manifest: PetManifest): Promise<HTMLCanvasElement> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    const canvas = document.createElement("canvas");
    canvas.width = PREVIEW_WIDTH;
    canvas.height = PREVIEW_HEIGHT;
    image.decoding = "async";
    image.onload = () => {
      const context = canvas.getContext("2d");
      if (!context) {
        reject(new Error("无法创建预览画布"));
        return;
      }
      context.imageSmoothingEnabled = true;
      context.clearRect(0, 0, PREVIEW_WIDTH, PREVIEW_HEIGHT);
      context.drawImage(image, 0, 0, CELL_WIDTH, CELL_HEIGHT, 0, 0, PREVIEW_WIDTH, PREVIEW_HEIGHT);
      resolve(canvas);
    };
    image.onerror = () => reject(new Error(`无法读取 ${info.id} 的预览`));
    image.src = info.previewDataUrl ?? `${PETS_BASE}/${info.path}/${manifest.spritesheetPath}`;
  });
}

function createPreview(pet: LoadedPet): HTMLElement {
  const wrapper = document.createElement("div");
  wrapper.className = "pet-preview";
  if (pet.preview) wrapper.append(pet.preview);
  else {
    wrapper.textContent = "暂无预览";
    wrapper.classList.add("pet-preview-fallback");
  }
  return wrapper;
}

function createBondSummary(pet: LoadedPet): HTMLElement {
  const wrapper = document.createElement("div");
  wrapper.className = "bond-summary";
  const header = document.createElement("div");
  header.className = "bond-summary-header";
  const label = document.createElement("span");
  label.textContent = "好感度";
  const value = document.createElement("strong");
  const bond = pet.life?.bond ?? 0;
  value.textContent = `${bond} / 100`;
  header.append(label, value);
  const track = document.createElement("div");
  track.className = "bond-track";
  const fill = document.createElement("span");
  fill.style.width = `${bond}%`;
  track.append(fill);
  const detail = document.createElement("small");
  detail.textContent = pet.life
    ? `${relationshipName(pet.life.relationshipLevel)} · ${pet.life.mood} · 互动 ${pet.life.interactionCount} 次`
    : "正在读取宠物状态…";
  if (pet.life) {
    const milestones = document.createElement("small");
    milestones.className = "milestone-list";
    milestones.textContent = pet.life.unlockedMilestones.length
      ? `已解锁：${pet.life.unlockedMilestones.length} 个里程碑 · 最高 ${pet.life.peakBond}`
      : `最高好感 ${pet.life.peakBond} · 还没有里程碑`;
    wrapper.append(milestones);
  }
  wrapper.prepend(header, track, detail);
  return wrapper;
}

function relationshipName(level: number): string {
  return ["初识", "熟悉", "亲近", "信赖", "挚友"][Math.max(0, Math.min(4, level - 1))] ?? "初识";
}

function petInstances(petId: string): PetInstanceInfo[] {
  return instances.filter((instance) => instance.petId === petId);
}

function createInstanceRow(instance: PetInstanceInfo): HTMLElement {
  const row = document.createElement("div");
  row.className = "instance-row";
  const label = document.createElement("span");
  label.className = "instance-label";
  label.textContent = instance.isMain ? "默认宠物" : instance.id;
  row.append(label);

  const state = document.createElement("span");
  state.className = instance.visible ? "instance-state visible" : "instance-state";
  state.textContent = instance.visible ? "显示中" : "已隐藏";
  row.append(state);

  const toggle = document.createElement("button");
  toggle.type = "button";
  toggle.className = "small-button";
  toggle.textContent = instance.visible ? "隐藏" : "显示";
  toggle.addEventListener("click", async () => {
    setBusy(toggle, true);
    try {
      await invoke("set_pet_instance_visible", { instanceId: instance.id, visible: !instance.visible });
      await reloadAll();
      setStatus(instance.visible ? "宠物已隐藏" : "宠物已显示");
    } catch (error) {
      setBusy(toggle, false);
      setStatus(errorMessage(error), "error");
    }
  });
  row.append(toggle);

  if (!instance.isMain) {
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "small-button danger-button";
    remove.textContent = "移除实例";
    remove.addEventListener("click", async () => {
      if (!(await confirmDialog("只移除这个显示实例，不会删除宠物资源。继续吗？"))) return;
      setBusy(remove, true);
      try {
        await invoke("remove_pet_instance", { instanceId: instance.id });
        await reloadAll();
        setStatus("宠物实例已移除");
      } catch (error) {
        setBusy(remove, false);
        setStatus(errorMessage(error), "error");
      }
    });
    row.append(remove);
  }
  return row;
}

interface RangeControl {
  input: HTMLInputElement;
  output: HTMLOutputElement;
  format: (value: number) => string;
}

interface PetSettingsControls {
  form: HTMLFormElement;
  scale: RangeControl;
  opacity: RangeControl;
  speed: RangeControl;
  wanderEnabled: HTMLInputElement;
  quietMode: HTMLInputElement;
  lockPosition: HTMLInputElement;
  clickThrough: HTMLInputElement;
  showInFullscreen: HTMLInputElement;
  circadianEnabled: HTMLInputElement;
  socialEnabled: HTMLInputElement;
  sleepStart: HTMLInputElement;
  wake: HTMLInputElement;
  pause: HTMLButtonElement;
}

function createRangeRow(
  title: string,
  description: string,
  min: string,
  max: string,
  step: string,
  value: number,
  format: (value: number) => string,
): { row: HTMLLabelElement; control: RangeControl } {
  const row = document.createElement("label");
  row.className = "setting-row";
  const copy = document.createElement("span");
  const strong = document.createElement("strong");
  strong.textContent = title;
  const small = document.createElement("small");
  small.textContent = description;
  copy.append(strong, small);

  const output = document.createElement("output");
  output.textContent = format(value);
  const input = document.createElement("input");
  input.type = "range";
  input.min = min;
  input.max = max;
  input.step = step;
  input.value = String(value);
  input.addEventListener("input", () => {
    output.textContent = format(Number(input.value));
  });
  row.append(copy, output, input);
  return { row, control: { input, output, format } };
}

function createToggleRow(title: string, description: string, checked: boolean): HTMLLabelElement {
  const row = document.createElement("label");
  row.className = "toggle-row";
  const input = document.createElement("input");
  input.type = "checkbox";
  input.checked = checked;
  const copy = document.createElement("span");
  const strong = document.createElement("strong");
  strong.textContent = title;
  const small = document.createElement("small");
  small.textContent = description;
  copy.append(strong, small);
  row.append(input, copy);
  return row;
}

function syncSettingsControls(controls: PetSettingsControls, next: PetSettings): void {
  controls.scale.input.value = String(next.scale);
  controls.opacity.input.value = String(Math.round(next.opacity * 100));
  controls.speed.input.value = String(next.speed);
  controls.wanderEnabled.checked = next.wanderEnabled;
  controls.quietMode.checked = next.quietMode;
  controls.lockPosition.checked = next.lockPosition;
  controls.clickThrough.checked = next.clickThrough;
  controls.showInFullscreen.checked = next.showInFullscreen;
  controls.circadianEnabled.checked = next.circadianEnabled;
  controls.socialEnabled.checked = next.socialEnabled;
  controls.sleepStart.value = minutesToTime(next.sleepStartMinutes);
  controls.wake.value = minutesToTime(next.wakeMinutes);
  controls.pause.textContent = next.paused ? "继续动画" : "暂停动画";
  controls.scale.output.textContent = controls.scale.format(next.scale);
  controls.opacity.output.textContent = controls.opacity.format(next.opacity * 100);
  controls.speed.output.textContent = controls.speed.format(next.speed);
}

function minutesToTime(minutes: number): string {
  const hours = Math.floor(minutes / 60).toString().padStart(2, "0");
  const remainder = (minutes % 60).toString().padStart(2, "0");
  return `${hours}:${remainder}`;
}

function timeToMinutes(value: string, fallback: number): number {
  const match = /^(\d{2}):(\d{2})$/.exec(value);
  if (!match) return fallback;
  return Math.min(1439, Number(match[1]) * 60 + Number(match[2]));
}

function setSettingsBusy(controls: PetSettingsControls, busy: boolean): void {
  controls.form
    .querySelectorAll<HTMLInputElement | HTMLButtonElement>("input, button")
    .forEach((control) => {
      control.disabled = busy;
    });
}

async function savePetSettings(
  pet: LoadedPet,
  controls: PetSettingsControls,
  displayName: string,
): Promise<void> {
  const next: PetSettings = {
    scale: Number(controls.scale.input.value),
    opacity: Number(controls.opacity.input.value) / 100,
    speed: Number(controls.speed.input.value),
    wanderEnabled: controls.wanderEnabled.checked,
    clickThrough: controls.clickThrough.checked,
    lockPosition: controls.lockPosition.checked,
    quietMode: controls.quietMode.checked,
    showInFullscreen: controls.showInFullscreen.checked,
    paused: pet.info.settings.paused,
    circadianEnabled: controls.circadianEnabled.checked,
    sleepStartMinutes: timeToMinutes(controls.sleepStart.value, pet.info.settings.sleepStartMinutes),
    wakeMinutes: timeToMinutes(controls.wake.value, pet.info.settings.wakeMinutes),
    socialEnabled: controls.socialEnabled.checked,
  };
  setSettingsBusy(controls, true);
  try {
    const saved = await invoke<PetSettings>("update_pet_settings", {
      petId: pet.info.id,
      settings: next,
    });
    pet.info.settings = saved;
    syncSettingsControls(controls, saved);
    setStatus(`${displayName}的设置已保存`);
  } catch (error) {
    setStatus(errorMessage(error), "error");
  } finally {
    setSettingsBusy(controls, false);
  }
}

function createSettingsForm(pet: LoadedPet, displayName: string): HTMLFormElement {
  const form = document.createElement("form");
  form.className = "pet-settings-form";
  const settingGrid = document.createElement("div");
  settingGrid.className = "setting-grid";
  const scale = createRangeRow(
    "大小",
    "只调整这只宠物",
    "0.5",
    "2.5",
    "0.1",
    pet.info.settings.scale,
    (value) => `${value.toFixed(1)}×`,
  );
  const opacity = createRangeRow(
    "透明度",
    "只影响这只宠物",
    "20",
    "100",
    "5",
    pet.info.settings.opacity * 100,
    (value) => `${Math.round(value)}%`,
  );
  const speed = createRangeRow(
    "行走速度",
    "只影响自动散步",
    "30",
    "240",
    "5",
    pet.info.settings.speed,
    (value) => `${Math.round(value)}px/s`,
  );
  settingGrid.append(scale.row, opacity.row, speed.row);

  const toggleGrid = document.createElement("div");
  toggleGrid.className = "toggle-grid";
  const wander = createToggleRow("偶尔散步", "保留长时间 idle", pet.info.settings.wanderEnabled);
  const quiet = createToggleRow("安静模式", "只待机，不自动走动", pet.info.settings.quietMode);
  const lock = createToggleRow("锁定位置", "防止误拖动宠物", pet.info.settings.lockPosition);
  const clickThrough = createToggleRow(
    "点击穿透",
    "鼠标点击会传给桌面窗口",
    pet.info.settings.clickThrough,
  );
  const showInFullscreen = createToggleRow(
    "全屏显示",
    "普通/无边框全屏仍显示（独占全屏除外）",
    pet.info.settings.showInFullscreen,
  );
  const circadian = createToggleRow(
    "昼夜作息",
    "按本地时间睡觉和起床",
    pet.info.settings.circadianEnabled,
  );
  const social = createToggleRow(
    "参与宠物社交",
    "允许这只宠物加入靠近、玩耍和群体活动",
    pet.info.settings.socialEnabled,
  );
  const sleepStart = document.createElement("label");
  sleepStart.className = "time-setting";
  sleepStart.innerHTML = "<span><strong>入睡时间</strong><small>进入睡眠区间</small></span>";
  const sleepStartInput = document.createElement("input");
  sleepStartInput.type = "time";
  sleepStartInput.value = minutesToTime(pet.info.settings.sleepStartMinutes);
  sleepStart.append(sleepStartInput);
  const wake = document.createElement("label");
  wake.className = "time-setting";
  wake.innerHTML = "<span><strong>起床时间</strong><small>离开睡眠区间</small></span>";
  const wakeInput = document.createElement("input");
  wakeInput.type = "time";
  wakeInput.value = minutesToTime(pet.info.settings.wakeMinutes);
  wake.append(wakeInput);
  toggleGrid.append(wander, quiet, lock, clickThrough, showInFullscreen, circadian, social);
  const scheduleGrid = document.createElement("div");
  scheduleGrid.className = "schedule-grid";
  scheduleGrid.append(sleepStart, wake);

  const pause = document.createElement("button");
  pause.type = "button";
  pause.className = "secondary-button settings-pause";
  pause.textContent = pet.info.settings.paused ? "继续动画" : "暂停动画";
  const helper = document.createElement("p");
  helper.className = "helper-text";
  helper.textContent = "设置会自动保存。点击穿透或锁定位置后，需要从这里关闭才能再次拖动。";
  form.append(settingGrid, toggleGrid, scheduleGrid, pause, helper);
  const controls: PetSettingsControls = {
    form,
    scale: scale.control,
    opacity: opacity.control,
    speed: speed.control,
    wanderEnabled: wander.querySelector<HTMLInputElement>("input")!,
    quietMode: quiet.querySelector<HTMLInputElement>("input")!,
    lockPosition: lock.querySelector<HTMLInputElement>("input")!,
    clickThrough: clickThrough.querySelector<HTMLInputElement>("input")!,
    showInFullscreen: showInFullscreen.querySelector<HTMLInputElement>("input")!,
    circadianEnabled: circadian.querySelector<HTMLInputElement>("input")!,
    socialEnabled: social.querySelector<HTMLInputElement>("input")!,
    sleepStart: sleepStartInput,
    wake: wakeInput,
    pause,
  };
  form.addEventListener("change", () => void savePetSettings(pet, controls, displayName));
  form.addEventListener("submit", (event) => event.preventDefault());
  pause.addEventListener("click", async () => {
    setBusy(pause, true);
    try {
      const saved = await invoke<PetSettings>("toggle_pet_pause", { petId: pet.info.id });
      pet.info.settings = saved;
      syncSettingsControls(controls, saved);
      setStatus(saved.paused ? `${displayName}已暂停` : `${displayName}已继续动画`);
    } catch (error) {
      setStatus(errorMessage(error), "error");
    } finally {
      setBusy(pause, false);
    }
  });
  return form;
}

function openPetSettings(pet: LoadedPet, displayName: string): void {
  settingsDialogTitle.textContent = `${displayName}设置`;
  settingsDialogContent.replaceChildren(createSettingsForm(pet, displayName));
  settingsDialog.showModal();
}

function render(): void {
  list.replaceChildren();
  for (const pet of pets) {
    const card = document.createElement("article");
    card.className = "pet-card";
    if (!pet.info.enabled) card.classList.add("disabled-card");

    const preview = createPreview(pet);
    const content = document.createElement("div");
    content.className = "pet-content";
    const title = document.createElement("h2");
    title.textContent = pet.manifest?.displayName || pet.info.displayName || pet.info.id;
    content.append(title);

    const source = document.createElement("span");
    source.className = "source-badge";
    source.textContent = pet.info.source === "imported" ? "已导入" : "内置";
    content.append(source);

    const description = document.createElement("p");
    description.className = "pet-description";
    description.textContent = pet.manifest?.description ?? pet.error ?? "资源信息不可用";
    content.append(description);
    content.append(createBondSummary(pet));

    const actions = document.createElement("div");
    actions.className = "pet-actions";
    const current = petInstances(pet.info.id);
    if (current.length === 0) {
      const add = document.createElement("button");
      add.type = "button";
      add.className = "primary-button";
      add.textContent = "显示这只宠物";
      add.disabled = pet.manifest === null || !pet.info.enabled;
      add.addEventListener("click", async () => {
        setBusy(add, true);
        try {
          await invoke("add_pet_instance", { petId: pet.info.id });
          await reloadAll();
          setStatus(`已显示 ${title.textContent ?? pet.info.id}`);
        } catch (error) {
          setBusy(add, false);
          setStatus(errorMessage(error), "error");
        }
      });
      actions.append(add);
    }

    const enable = document.createElement("button");
    enable.type = "button";
    enable.className = "secondary-button";
    enable.textContent = pet.info.enabled ? "停用资源" : "启用资源";
    enable.addEventListener("click", async () => {
      setBusy(enable, true);
      try {
        await invoke("set_pet_enabled", { petId: pet.info.id, enabled: !pet.info.enabled });
        await reloadAll();
        setStatus(pet.info.enabled ? "宠物资源已停用" : "宠物资源已启用");
      } catch (error) {
        setBusy(enable, false);
        setStatus(errorMessage(error), "error");
      }
    });
    actions.append(enable);

    const exportButton = document.createElement("button");
    exportButton.type = "button";
    exportButton.className = "secondary-button";
    exportButton.textContent = "导出宠物";
    exportButton.title = "导出不包含记忆、聊天记录和应用配置的宠物包";
    exportButton.addEventListener("click", async () => {
      setBusy(exportButton, true);
      try {
        await invoke("export_pet_package", { petId: pet.info.id });
        setStatus("请选择保存位置导出宠物包…");
      } catch (error) {
        setStatus(errorMessage(error), "error");
      } finally {
        setBusy(exportButton, false);
      }
    });
    actions.append(exportButton);

    if (pet.info.source === "imported") {
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "secondary-button danger-button";
      remove.textContent = "删除资源";
      remove.addEventListener("click", async () => {
        const activeInstanceNote = current.length
          ? `当前正在显示 ${current.length} 只，删除时会一并关闭。`
          : "";
        if (
          !(await confirmDialog(
            `删除后需要重新导入宠物包。${activeInstanceNote}确定继续吗？`,
          ))
        ) return;
        setBusy(remove, true);
        try {
          await invoke("remove_imported_pet", { petId: pet.info.id });
          await reloadAll();
          setStatus("导入的宠物资源已删除");
        } catch (error) {
          setBusy(remove, false);
          setStatus(errorMessage(error), "error");
        }
      });
      actions.append(remove);
    }
    content.append(actions);

    const instanceMeta = document.createElement("div");
    instanceMeta.className = "instance-meta";
    const instanceTitle = document.createElement("p");
    instanceTitle.className = "instance-heading";
    instanceTitle.textContent = current.length ? `当前实例 · ${current.length} 只` : "当前没有显示实例";
    instanceMeta.append(instanceTitle);
    const settingsButton = document.createElement("button");
    settingsButton.type = "button";
    settingsButton.className = "settings-button";
    settingsButton.textContent = "独立设置";
    settingsButton.setAttribute("aria-haspopup", "dialog");
    settingsButton.addEventListener("click", () =>
      openPetSettings(pet, title.textContent ?? pet.info.id),
    );
    instanceMeta.append(settingsButton);
    content.append(instanceMeta);
    const instanceList = document.createElement("div");
    instanceList.className = "instance-list";
    for (const instance of current) instanceList.append(createInstanceRow(instance));
    content.append(instanceList);

    card.append(preview, content);
    list.append(card);
  }
}

async function loadAll(): Promise<void> {
  const [catalog, nextInstances] = await Promise.all([
    invoke<InstalledPetInfo[]>("get_pet_catalog"),
    invoke<PetInstanceInfo[]>("get_pet_instances"),
  ]);
  instances = nextInstances;
  pets = await Promise.all(catalog.map(async (info): Promise<LoadedPet> => {
    try {
      const [manifest, life] = await Promise.all([
        loadManifest(info),
        invoke<PetLifeState>("get_pet_state", { petId: info.id }),
      ]);
      const preview = await loadPreview(info, manifest);
      return { info, manifest, preview, life };
    } catch (error) {
      return { info, manifest: null, preview: null, life: null, error: errorMessage(error) };
    }
  }));
  render();
  const valid = pets.filter((pet) => pet.manifest !== null).length;
  const visible = instances.filter((instance) => instance.visible).length;
  setStatus(`已安装 ${valid} 个资源 · 当前显示 ${visible} 只宠物`);
}

async function reloadAll(): Promise<void> {
  refreshButton.disabled = true;
  setStatus("正在读取宠物配置…");
  try {
    await loadAll();
  } catch (error) {
    setStatus(errorMessage(error), "error");
  } finally {
    refreshButton.disabled = false;
  }
}

function toBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunkSize = 0x8000;
  for (let index = 0; index < bytes.length; index += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(index, Math.min(index + chunkSize, bytes.length)));
  }
  return btoa(binary);
}

refreshButton.addEventListener("click", () => void reloadAll());
void listen<{
  petId: string;
  cancelled?: boolean;
  path?: string;
  error?: string;
}>("pet://pet-export-finished", ({ payload }) => {
  if (payload.error) {
    setStatus(payload.error, "error");
  } else if (payload.cancelled) {
    setStatus("已取消导出宠物包");
  } else if (payload.path) {
    const fileName = payload.path.split(/[\\/]/).pop() || `${payload.petId}.zip`;
    setStatus(`已导出 ${fileName}（不含记忆和聊天记录）`);
  }
}).catch((error) => setStatus(errorMessage(error), "error"));
openAiSettingsButton.addEventListener("click", async () => {
  setBusy(openAiSettingsButton, true);
  try {
    await invoke("open_ai_settings");
  } catch (error) {
    setStatus(errorMessage(error), "error");
  } finally {
    setBusy(openAiSettingsButton, false);
  }
});
openEnvironmentSettingsButton.addEventListener("click", async () => {
  setBusy(openEnvironmentSettingsButton, true);
  try {
    await invoke("open_environment_settings");
  } catch (error) {
    setStatus(errorMessage(error), "error");
  } finally {
    setBusy(openEnvironmentSettingsButton, false);
  }
});
openSocialSettingsButton.addEventListener("click", async () => {
  setBusy(openSocialSettingsButton, true);
  try {
    await invoke("open_social_settings");
  } catch (error) {
    setStatus(errorMessage(error), "error");
  } finally {
    setBusy(openSocialSettingsButton, false);
  }
});
openSocialLogButton.addEventListener("click", async () => {
  setBusy(openSocialLogButton, true);
  try {
    await invoke("open_social_log");
  } catch (error) {
    setStatus(errorMessage(error), "error");
  } finally {
    setBusy(openSocialLogButton, false);
  }
});
importButton.addEventListener("click", () => importInput.click());
importInput.addEventListener("change", async () => {
  const file = importInput.files?.[0];
  importInput.value = "";
  if (!file) return;
  importButton.disabled = true;
  setStatus(`正在导入 ${file.name}…`);
  try {
    const data = toBase64(new Uint8Array(await file.arrayBuffer()));
    await invoke("import_pet_package", { fileName: file.name, dataBase64: data });
    await reloadAll();
    setStatus("宠物资源导入成功");
  } catch (error) {
    setStatus(errorMessage(error), "error");
  } finally {
    importButton.disabled = false;
  }
});

closeSettingsDialogButton.addEventListener("click", () => settingsDialog.close());
settingsDialog.addEventListener("click", (event) => {
  if (event.target === settingsDialog) settingsDialog.close();
});

void waitForAppReady()
  .then(() => reloadAll())
  .catch((error) => setStatus(errorMessage(error), "error"));

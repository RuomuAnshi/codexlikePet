import { invoke } from "@tauri-apps/api/core";

interface SocialSettings {
  enabled: boolean;
  minIntervalMinutes: number;
  maxIntervalMinutes: number;
  proximityEnabled: boolean;
  manualEnabled: boolean;
  maxParticipants: number;
  propsEnabled: boolean;
}

const form = document.querySelector<HTMLFormElement>("#form")!;
const enabled = document.querySelector<HTMLInputElement>("#enabled")!;
const min = document.querySelector<HTMLInputElement>("#min")!;
const max = document.querySelector<HTMLInputElement>("#max")!;
const participants = document.querySelector<HTMLInputElement>("#participants")!;
const proximity = document.querySelector<HTMLInputElement>("#proximity")!;
const manual = document.querySelector<HTMLInputElement>("#manual")!;
const props = document.querySelector<HTMLInputElement>("#props")!;
const save = document.querySelector<HTMLButtonElement>("#save")!;
const status = document.querySelector<HTMLElement>("#status")!;

function showStatus(message: string, error = false): void {
  status.textContent = message;
  status.dataset.kind = error ? "error" : "normal";
}

function populate(settings: SocialSettings): void {
  enabled.checked = settings.enabled;
  min.value = String(settings.minIntervalMinutes);
  max.value = String(settings.maxIntervalMinutes);
  participants.value = String(settings.maxParticipants);
  proximity.checked = settings.proximityEnabled;
  manual.checked = settings.manualEnabled;
  props.checked = settings.propsEnabled;
}

async function load(): Promise<void> {
  try {
    populate(await invoke<SocialSettings>("get_social_settings"));
    showStatus("社交设置已加载");
  } catch (error) {
    showStatus(String(error), true);
  }
}

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  save.disabled = true;
  try {
    const settings = await invoke<SocialSettings>("update_social_settings", {
      settings: {
        enabled: enabled.checked,
        minIntervalMinutes: Number(min.value),
        maxIntervalMinutes: Number(max.value),
        proximityEnabled: proximity.checked,
        manualEnabled: manual.checked,
        maxParticipants: Number(participants.value),
        propsEnabled: props.checked,
      },
    });
    populate(settings);
    showStatus("社交设置已保存");
  } catch (error) {
    showStatus(String(error), true);
  } finally {
    save.disabled = false;
  }
});

void load();

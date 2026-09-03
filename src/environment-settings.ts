import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { waitForAppReady } from "./appReady";
import type { EnvironmentSettings } from "./pet/config";

const status = document.querySelector<HTMLElement>("#status")!;
const foreground = document.querySelector<HTMLInputElement>("#foreground")!;
const breakReminder = document.querySelector<HTMLInputElement>("#break-reminder")!;
const meetingQuiet = document.querySelector<HTMLInputElement>("#meeting-quiet")!;
const lowBattery = document.querySelector<HTMLInputElement>("#low-battery")!;
const notifications = document.querySelector<HTMLInputElement>("#notifications")!;
const breakMinutes = document.querySelector<HTMLInputElement>("#break-minutes")!;
const batteryThreshold = document.querySelector<HTMLInputElement>("#battery-threshold")!;
const codingApps = document.querySelector<HTMLTextAreaElement>("#coding-apps")!;
const meetingApps = document.querySelector<HTMLTextAreaElement>("#meeting-apps")!;
const save = document.querySelector<HTMLButtonElement>("#save")!;
const snapshot = document.querySelector<HTMLElement>("#snapshot")!;
const supportBadge = document.querySelector<HTMLElement>("#support-badge")!;
const notificationNote = document.querySelector<HTMLElement>("#notification-note")!;
const windowSupport = document.querySelector<HTMLElement>("#window-support")!;
const permissionActions = document.querySelector<HTMLElement>(".permission-actions")!;
const openScreenRecording = document.querySelector<HTMLButtonElement>("#open-screen-recording")!;
const openAccessibility = document.querySelector<HTMLButtonElement>("#open-accessibility")!;
const refreshPermissions = document.querySelector<HTMLButtonElement>("#refresh-permissions")!;

type DesktopWindowSupport = {
  platform: string;
  enumerationSupported: boolean;
  throwSupported: boolean;
  accessibilityRequired: boolean;
  accessibilityGranted: boolean;
  screenRecordingRequired: boolean;
  screenRecordingGranted: boolean;
  windowCount: number;
  enumerationError?: string | null;
};

function setStatus(message: string, error = false): void {
  status.textContent = message;
  status.dataset.kind = error ? "error" : "normal";
}

function setForm(settings: EnvironmentSettings): void {
  foreground.checked = settings.foregroundTrackingEnabled;
  breakReminder.checked = settings.breakReminderEnabled;
  meetingQuiet.checked = settings.meetingQuietEnabled;
  lowBattery.checked = settings.lowBatteryEnabled;
  notifications.checked = settings.notificationEventsEnabled;
  breakMinutes.value = String(settings.breakReminderMinutes);
  batteryThreshold.value = String(settings.lowBatteryThreshold);
  codingApps.value = settings.codingApps.join(", ");
  meetingApps.value = settings.meetingApps.join(", ");
}

function listValue(value: string): string[] {
  return value.split(",").map((item) => item.trim()).filter(Boolean);
}

async function load(): Promise<void> {
  try {
    setForm(await invoke<EnvironmentSettings>("get_environment_settings"));
    await loadWindowSupport();
    setStatus("环境设置已加载");
  } catch (error) {
    setStatus(String(error), true);
  }
}

async function loadWindowSupport(): Promise<void> {
  const support = await invoke<DesktopWindowSupport>("get_desktop_window_support");
  permissionActions.hidden = support.platform !== "macos";
  if (support.platform !== "macos") {
    openScreenRecording.disabled = true;
    openAccessibility.disabled = true;
  }
    if (!support.enumerationSupported) {
      windowSupport.textContent = "窗口枚举：当前平台不支持（Linux 首版安全关闭）";
    } else if (support.screenRecordingRequired && !support.screenRecordingGranted) {
      windowSupport.textContent = "未获得屏幕录制授权，无法发现其他应用窗口；请配置后完全退出并重启 SakiPet。";
    } else if (support.enumerationError) {
      windowSupport.textContent = `窗口枚举失败：${support.enumerationError}`;
    } else if (support.windowCount === 0) {
      windowSupport.textContent = "权限已就绪，但暂时没有发现可互动的普通窗口；请打开一个非全屏、非最小化窗口。";
    } else if (support.accessibilityRequired && !support.accessibilityGranted) {
      windowSupport.textContent = `已发现 ${support.windowCount} 个窗口；扔窗口还需要在系统设置 → 隐私与安全性 → 辅助功能中授权 SakiPet。`;
    } else {
      windowSupport.textContent = `窗口互动可用：已发现 ${support.windowCount} 个窗口，支持爬窗、坐窗沿${support.throwSupported ? "和受保护的窗口移动" : ""}。`;
    }
}

async function openPermission(kind: "screen-recording" | "accessibility", button: HTMLButtonElement): Promise<void> {
  button.disabled = true;
  try {
    await invoke("open_desktop_permission_settings", { kind });
    setStatus("已打开系统权限设置；授权后点击“重新检测”并重启 SakiPet");
  } catch (error) {
    setStatus(String(error), true);
  } finally {
    button.disabled = false;
  }
}

openScreenRecording.addEventListener("click", () => void openPermission("screen-recording", openScreenRecording));
openAccessibility.addEventListener("click", () => void openPermission("accessibility", openAccessibility));
refreshPermissions.addEventListener("click", async () => {
  refreshPermissions.disabled = true;
  try {
    await loadWindowSupport();
    setStatus("权限状态已重新检测");
  } catch (error) {
    setStatus(String(error), true);
  } finally {
    refreshPermissions.disabled = false;
  }
});

save.addEventListener("click", async () => {
  save.disabled = true;
  try {
    const next: EnvironmentSettings = {
      foregroundTrackingEnabled: foreground.checked,
      breakReminderEnabled: breakReminder.checked,
      breakReminderMinutes: Number(breakMinutes.value),
      meetingQuietEnabled: meetingQuiet.checked,
      lowBatteryEnabled: lowBattery.checked,
      lowBatteryThreshold: Number(batteryThreshold.value),
      notificationEventsEnabled: notifications.checked,
      codingApps: listValue(codingApps.value),
      meetingApps: listValue(meetingApps.value),
    };
    setForm(await invoke<EnvironmentSettings>("update_environment_settings", { settings: next }));
    setStatus("设置已保存，规则会立即生效");
  } catch (error) {
    setStatus(String(error), true);
  } finally {
    save.disabled = false;
  }
});

void listen<{ snapshot: { appName: string; category: string; isFullscreen: boolean; session: string; batteryPercent?: number }; autoQuiet: boolean }>(
  "environment://state",
  ({ payload }) => {
    const current = payload.snapshot;
    const app = current.appName || "未识别应用";
    snapshot.textContent = `${app} · ${current.category} · ${current.isFullscreen ? "全屏" : "普通窗口"} · ${current.session === "active" ? "活跃" : "休眠"}${payload.autoQuiet ? " · 当前自动安静" : ""}`;
    supportBadge.textContent = current.session === "unsupported" ? "平台不支持" : "感知已连接";
  },
);

void waitForAppReady().then(load);

void listen<{ snapshot: { notificationSupported: boolean } }>("environment://state", ({ payload }) => {
  if (payload.snapshot.notificationSupported) {
    notificationNote.textContent = "仅记录通知来源和时间，不读取标题、正文或附件。";
  } else {
    notificationNote.textContent = "当前构建不支持读取其他应用通知正文；不会使用辅助功能抓取。";
    notifications.checked = false;
  }
});

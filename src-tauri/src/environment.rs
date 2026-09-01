use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager};

use crate::{config_snapshot, AppState};

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnvironmentSnapshot {
    pub app_name: String,
    pub app_id: String,
    pub category: String,
    pub is_fullscreen: bool,
    pub session: String,
    pub battery_percent: Option<u8>,
    pub on_battery: Option<bool>,
    pub notification_supported: bool,
}

impl Default for EnvironmentSnapshot {
    fn default() -> Self {
        Self {
            app_name: String::new(),
            app_id: String::new(),
            category: "unknown".to_string(),
            is_fullscreen: false,
            session: "active".to_string(),
            battery_percent: None,
            on_battery: None,
            notification_supported: false,
        }
    }
}

#[derive(Default)]
struct EnvironmentState {
    snapshot: EnvironmentSnapshot,
    app_key: String,
    app_since: u64,
    break_reminded_at: u64,
    low_battery_reminded: bool,
    auto_quiet: bool,
}

pub(crate) struct EnvironmentRuntime {
    state: Mutex<EnvironmentState>,
    started: AtomicBool,
}

impl Default for EnvironmentRuntime {
    fn default() -> Self {
        Self {
            state: Mutex::new(EnvironmentState::default()),
            started: AtomicBool::new(false),
        }
    }
}

pub(crate) fn is_auto_quiet(app: &tauri::AppHandle) -> bool {
    app.state::<AppState>()
        .environment
        .state
        .lock()
        .map(|state| state.auto_quiet)
        .unwrap_or(false)
}

pub(crate) fn start_monitor(app: &tauri::AppHandle) {
    let runtime = &app.state::<AppState>().environment;
    if runtime.started.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let Ok(config) = config_snapshot(&app) else {
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            };
            let snapshot = platform_snapshot();
            process_snapshot(&app, &config, snapshot);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn classify(snapshot: &mut EnvironmentSnapshot, config: &crate::AppConfig) {
    let key = if snapshot.app_id.is_empty() {
        snapshot.app_name.clone()
    } else {
        snapshot.app_id.clone()
    };
    let matches = |items: &[String]| {
        items.iter().any(|item| {
            let item = item.to_ascii_lowercase();
            key.to_ascii_lowercase().contains(&item)
                || snapshot.app_name.to_ascii_lowercase().contains(&item)
        })
    };
    if matches(&config.environment.meeting_apps) {
        snapshot.category = "meeting".to_string();
    } else if matches(&config.environment.coding_apps) {
        snapshot.category = "coding".to_string();
    } else {
        snapshot.category = "other".to_string();
    }
}

fn process_snapshot(
    app: &tauri::AppHandle,
    config: &crate::AppConfig,
    mut snapshot: EnvironmentSnapshot,
) {
    classify(&mut snapshot, config);
    let now = now_ms();
    let key = format!(
        "{}:{}:{}",
        snapshot.app_id, snapshot.app_name, snapshot.category
    );
    let mut break_due = false;
    let mut low_battery_due = false;
    let mut session_changed = false;
    let mut should_sleep = false;
    let mut changed = false;
    {
        let runtime = &app.state::<AppState>().environment;
        let Ok(mut state) = runtime.state.lock() else {
            return;
        };
        if state.app_key != key {
            state.app_key = key;
            state.app_since = now;
            state.break_reminded_at = 0;
            changed = true;
        }
        if state.snapshot.session != snapshot.session {
            session_changed = true;
            should_sleep = snapshot.session != "active";
            changed = true;
        }
        if state.snapshot != snapshot {
            changed = true;
        }
        if config.environment.foreground_tracking_enabled
            && config.environment.break_reminder_enabled
            && snapshot.category == "coding"
            && now.saturating_sub(state.app_since)
                >= config.environment.break_reminder_minutes as u64 * 60_000
            && now.saturating_sub(state.break_reminded_at) >= 2 * 60 * 60_000
        {
            state.break_reminded_at = now;
            break_due = true;
        }
        let battery_low = config.environment.low_battery_enabled
            && snapshot.on_battery == Some(true)
            && snapshot
                .battery_percent
                .is_some_and(|value| value <= config.environment.low_battery_threshold);
        if battery_low && !state.low_battery_reminded {
            state.low_battery_reminded = true;
            low_battery_due = true;
        } else if !battery_low
            && snapshot.battery_percent.is_some_and(|value| {
                value >= config.environment.low_battery_threshold.saturating_add(5)
            })
        {
            state.low_battery_reminded = false;
        }
        state.auto_quiet = (config.environment.meeting_quiet_enabled
            && snapshot.category == "meeting"
            && snapshot.is_fullscreen)
            || snapshot.session != "active";
        state.snapshot = snapshot.clone();
    }

    if session_changed {
        crate::ai::set_all_pets_sleeping(app, should_sleep);
    }
    if session_changed || is_auto_quiet(app) {
        crate::social::cancel_all_scenes(app);
    }
    if changed || break_due || low_battery_due {
        let _ = app.emit(
            "environment://state",
            serde_json::json!({
                "snapshot": snapshot,
                "autoQuiet": is_auto_quiet(app),
                "breakReminder": break_due,
                "lowBattery": low_battery_due,
            }),
        );
    }
}

#[cfg(target_os = "macos")]
fn platform_snapshot() -> EnvironmentSnapshot {
    use objc2_app_kit::NSWorkspace;

    let workspace = NSWorkspace::sharedWorkspace();
    let running = workspace.frontmostApplication();
    if running.is_none() {
        return EnvironmentSnapshot {
            session: "inactive".to_string(),
            ..EnvironmentSnapshot::default()
        };
    }
    let app_name = running
        .as_ref()
        .and_then(|app| app.localizedName())
        .map(|value| value.to_string())
        .unwrap_or_default();
    let app_id = running
        .as_ref()
        .and_then(|app| app.bundleIdentifier())
        .map(|value| value.to_string())
        .unwrap_or_default();
    let pid = running.as_ref().map(|app| app.processIdentifier());
    let is_fullscreen = pid.is_some_and(|pid| {
        xcap::Window::all()
            .unwrap_or_default()
            .into_iter()
            .filter(|window| window.pid().ok() == u32::try_from(pid).ok())
            .any(|window| {
                let Ok(monitor) = window.current_monitor() else {
                    return false;
                };
                let Ok(minimized) = window.is_minimized() else {
                    return false;
                };
                !minimized
                    && window.x().ok() == monitor.x().ok()
                    && window.y().ok() == monitor.y().ok()
                    && window.width().ok() == monitor.width().ok()
                    && window.height().ok() == monitor.height().ok()
            })
    });
    EnvironmentSnapshot {
        app_name,
        app_id,
        is_fullscreen,
        notification_supported: false,
        ..EnvironmentSnapshot::default()
    }
}

#[cfg(target_os = "windows")]
fn platform_snapshot() -> EnvironmentSnapshot {
    use std::path::Path;
    use windows::Win32::Foundation::{CloseHandle, HWND, RECT};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId,
    };
    use windows_core::PWSTR;

    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd == HWND::default() {
        return EnvironmentSnapshot {
            session: "inactive".to_string(),
            ..EnvironmentSnapshot::default()
        };
    }
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let mut process_name = String::new();
    if pid != 0 {
        if let Ok(handle) = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
            let mut buffer = vec![0u16; 512];
            let mut length = buffer.len() as u32;
            if unsafe {
                QueryFullProcessImageNameW(
                    handle,
                    PROCESS_NAME_WIN32,
                    PWSTR(buffer.as_mut_ptr()),
                    &mut length,
                )
            }
            .is_ok()
            {
                process_name = String::from_utf16_lossy(&buffer[..length as usize]);
            }
            let _ = unsafe { CloseHandle(handle) };
        }
    }
    let app_name = Path::new(&process_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_string();
    let mut window_rect = RECT::default();
    let mut monitor_rect = RECT::default();
    let is_fullscreen = unsafe {
        GetWindowRect(hwnd, &mut window_rect).is_ok()
            && {
                let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                let mut info = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                let ok = GetMonitorInfoW(monitor, &mut info).as_bool();
                monitor_rect = info.rcMonitor;
                ok
            }
            && window_rect == monitor_rect
    };
    let mut power = SYSTEM_POWER_STATUS::default();
    let power_ok = unsafe { GetSystemPowerStatus(&mut power).is_ok() };
    EnvironmentSnapshot {
        app_id: app_name.clone(),
        app_name,
        is_fullscreen,
        battery_percent: power_ok
            .then_some(power.BatteryLifePercent)
            .filter(|value| *value <= 100),
        on_battery: power_ok.then_some(power.ACLineStatus == 0),
        notification_supported: false,
        ..EnvironmentSnapshot::default()
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_snapshot() -> EnvironmentSnapshot {
    EnvironmentSnapshot {
        session: "unsupported".to_string(),
        ..EnvironmentSnapshot::default()
    }
}

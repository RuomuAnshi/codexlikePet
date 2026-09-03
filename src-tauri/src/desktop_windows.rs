//! Desktop-window awareness and the small, explicit window-interaction
//! choreography.  This module deliberately keeps operating-system handles out
//! of the pet protocol: the UI receives a stable id and a safe animation phase
//! while Rust owns enumeration, validation and native window movement.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, LogicalPosition, Manager};

use super::{config_snapshot, instance_label, is_safe_id, AppState, PetPosition};

const WINDOW_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const SCENE_TICK: Duration = Duration::from_millis(40);
const TARGET_MOVE_TOLERANCE: f64 = 120.0;
const WINDOW_TARGET_MAX_DISTANCE: f64 = 420.0;
const WINDOW_SCENE_MAX_WINDOWS: usize = 96;
static SCENE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopWindowRect {
    pub id: u64,
    pub pid: u32,
    pub app_name: String,
    pub title: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub monitor_key: String,
    pub minimized: bool,
    pub focused: bool,
    #[serde(skip)]
    #[allow(dead_code)]
    pub scale_factor: f64,
    #[serde(skip)]
    #[allow(dead_code)]
    pub monitor_x: f64,
    #[serde(skip)]
    #[allow(dead_code)]
    pub monitor_y: f64,
    #[serde(skip)]
    #[allow(dead_code)]
    pub monitor_width: f64,
    #[serde(skip)]
    #[allow(dead_code)]
    pub monitor_height: f64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopWindowSupport {
    pub platform: String,
    pub enumeration_supported: bool,
    pub throw_supported: bool,
    pub accessibility_required: bool,
    pub accessibility_granted: bool,
    pub screen_recording_required: bool,
    pub screen_recording_granted: bool,
    pub window_count: usize,
    pub enumeration_error: Option<String>,
}

#[derive(Default)]
pub(crate) struct DesktopWindowRuntime {
    pub(crate) windows: Mutex<Vec<DesktopWindowRect>>,
    last_error: Mutex<Option<String>>,
    started: AtomicBool,
    active_scenes: Mutex<HashMap<String, ActiveWindowScene>>,
}

#[derive(Clone)]
struct ActiveWindowScene {
    scene_id: String,
    instance_id: String,
    cancel: Arc<AtomicBool>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn next_scene_id() -> String {
    format!(
        "window-scene-{}-{}",
        now_ms(),
        SCENE_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// True when the native window nearly fills the monitor.  Fullscreen and
/// exclusive-output windows are intentionally never targets for interaction.
fn is_fullscreen_like(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    monitor_x: f64,
    monitor_y: f64,
    monitor_width: f64,
    monitor_height: f64,
) -> bool {
    let touches_origin = (x - monitor_x).abs() <= 3.0 && (y - monitor_y).abs() <= 3.0;
    touches_origin && width >= monitor_width * 0.96 && height >= monitor_height * 0.96
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn enumerate_platform_windows(self_pid: u32) -> Result<Vec<DesktopWindowRect>, String> {
    let mut result = Vec::new();
    let windows = xcap::Window::all().map_err(|error| format!("无法枚举桌面窗口: {error}"))?;
    for window in windows {
        let Ok(pid) = window.pid() else { continue };
        if pid == self_pid {
            continue;
        }
        let Ok(raw_x) = window.x() else { continue };
        let Ok(raw_y) = window.y() else { continue };
        let Ok(raw_width) = window.width() else {
            continue;
        };
        let Ok(raw_height) = window.height() else {
            continue;
        };
        if raw_width < 80 || raw_height < 48 || window.is_minimized().unwrap_or(false) {
            continue;
        }
        let Ok(monitor) = window.current_monitor() else {
            continue;
        };
        let monitor_x = monitor.x().unwrap_or(0) as f64;
        let monitor_y = monitor.y().unwrap_or(0) as f64;
        let monitor_width = monitor.width().unwrap_or(raw_width) as f64;
        let monitor_height = monitor.height().unwrap_or(raw_height) as f64;
        // CGWindowList already reports points on macOS. xcap reports native
        // pixels on Windows, so convert both the origin and size consistently
        // with Tauri's LogicalPosition there.
        #[cfg(target_os = "windows")]
        let coordinate_scale = monitor.scale_factor().unwrap_or(1.0).max(1.0) as f64;
        #[cfg(target_os = "macos")]
        let coordinate_scale = 1.0;
        let x = raw_x as f64 / coordinate_scale;
        let y = raw_y as f64 / coordinate_scale;
        let width = raw_width as f64 / coordinate_scale;
        let height = raw_height as f64 / coordinate_scale;
        let logical_monitor_x = monitor_x / coordinate_scale;
        let logical_monitor_y = monitor_y / coordinate_scale;
        let logical_monitor_width = monitor_width / coordinate_scale;
        let logical_monitor_height = monitor_height / coordinate_scale;
        if is_fullscreen_like(
            x,
            y,
            width,
            height,
            logical_monitor_x,
            logical_monitor_y,
            logical_monitor_width,
            logical_monitor_height,
        ) {
            continue;
        }
        let app_name = window.app_name().unwrap_or_default().trim().to_string();
        if app_name.is_empty() || app_name.eq_ignore_ascii_case("sakipet") {
            continue;
        }
        result.push(DesktopWindowRect {
            id: u64::from(window.id().unwrap_or_default()),
            pid,
            app_name,
            title: window.title().unwrap_or_default().trim().to_string(),
            x,
            y,
            width,
            height,
            monitor_key: format!(
                "{}:{}",
                logical_monitor_x.round(),
                logical_monitor_y.round()
            ),
            minimized: false,
            focused: window.is_focused().unwrap_or(false),
            scale_factor: coordinate_scale,
            monitor_x: logical_monitor_x,
            monitor_y: logical_monitor_y,
            monitor_width: logical_monitor_width,
            monitor_height: logical_monitor_height,
        });
    }
    result.truncate(WINDOW_SCENE_MAX_WINDOWS);
    Ok(result)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn enumerate_platform_windows(_self_pid: u32) -> Result<Vec<DesktopWindowRect>, String> {
    Ok(Vec::new())
}

fn changed_significantly(previous: &[DesktopWindowRect], next: &[DesktopWindowRect]) -> bool {
    if previous.len() != next.len() {
        return true;
    }
    next.iter().any(|candidate| {
        let Some(old) = previous.iter().find(|item| item.id == candidate.id) else {
            return true;
        };
        (old.x - candidate.x).abs() > 2.0
            || (old.y - candidate.y).abs() > 2.0
            || (old.width - candidate.width).abs() > 2.0
            || (old.height - candidate.height).abs() > 2.0
    })
}

pub(crate) fn refresh(app: &tauri::AppHandle) -> Result<Vec<DesktopWindowRect>, String> {
    let next = match enumerate_platform_windows(std::process::id()) {
        Ok(next) => next,
        Err(error) => {
            let runtime = &app.state::<AppState>().desktop_windows;
            if let Ok(mut windows) = runtime.windows.lock() {
                windows.clear();
            }
            if let Ok(mut last_error) = runtime.last_error.lock() {
                *last_error = Some(error.clone());
            }
            return Err(error);
        }
    };
    let runtime = &app.state::<AppState>().desktop_windows;
    if let Ok(mut last_error) = runtime.last_error.lock() {
        *last_error = None;
    }
    let changed = runtime
        .windows
        .lock()
        .map(|mut previous| {
            let changed = changed_significantly(&previous, &next);
            *previous = next.clone();
            changed
        })
        .unwrap_or(false);
    if changed {
        let _ = app.emit("desktop://windows-changed", &next);
    }
    Ok(next)
}

pub(crate) fn cached_windows(app: &tauri::AppHandle) -> Vec<DesktopWindowRect> {
    app.state::<AppState>()
        .desktop_windows
        .windows
        .lock()
        .map(|windows| windows.clone())
        .unwrap_or_default()
}

pub(crate) fn start_monitor(app: &tauri::AppHandle) {
    let runtime = &app.state::<AppState>().desktop_windows;
    if runtime.started.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(error) = refresh(&app) {
                // Linux intentionally returns an empty safe result. Other
                // failures are logged but never interrupt the pet runtime.
                eprintln!("[desktop] window enumeration unavailable: {error}");
            }
            tokio::time::sleep(WINDOW_REFRESH_INTERVAL).await;
        }
    });
}

#[tauri::command]
pub(crate) fn list_desktop_windows(
    app: tauri::AppHandle,
) -> Result<Vec<DesktopWindowRect>, String> {
    refresh(&app).or_else(|_| Ok(cached_windows(&app)))
}

#[tauri::command]
pub(crate) fn get_desktop_window_support(app: tauri::AppHandle) -> DesktopWindowSupport {
    support_snapshot(&app)
}

pub(crate) fn support_snapshot(app: &tauri::AppHandle) -> DesktopWindowSupport {
    let refresh_error = refresh(app).err();
    let runtime = &app.state::<AppState>().desktop_windows;
    let enumeration_error = refresh_error.or_else(|| {
        runtime
            .last_error
            .lock()
            .ok()
            .and_then(|error| error.clone())
    });
    let screen_recording_required = cfg!(target_os = "macos");
    DesktopWindowSupport {
        platform: std::env::consts::OS.to_string(),
        enumeration_supported: cfg!(any(target_os = "macos", target_os = "windows")),
        throw_supported: cfg!(any(target_os = "macos", target_os = "windows")),
        accessibility_required: cfg!(target_os = "macos"),
        accessibility_granted: accessibility_granted(),
        screen_recording_required,
        screen_recording_granted: screen_recording_granted() || !screen_recording_required,
        window_count: cached_windows(app).len(),
        enumeration_error,
    }
}

#[tauri::command]
pub(crate) fn open_desktop_permission_settings(kind: String) -> Result<(), String> {
    if !matches!(kind.as_str(), "screen-recording" | "accessibility") {
        return Err("无效的权限类型".to_string());
    }

    #[cfg(target_os = "macos")]
    {
        if kind == "screen-recording" && !screen_recording_granted() {
            // This asks macOS to register the current, installed app as a
            // screen-capture client before opening the matching pane.
            let _ = macos_accessibility::request_screen_recording();
        }
        let url = if kind == "screen-recording" {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
        } else {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
        };
        std::process::Command::new("/usr/bin/open")
            .arg(url)
            .spawn()
            .map_err(|error| format!("无法打开系统权限设置: {error}"))?;
        return Ok(());
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = kind;
        Err("当前平台不需要这组 macOS 权限".to_string())
    }
}

#[cfg(target_os = "macos")]
fn accessibility_granted() -> bool {
    macos_accessibility::is_trusted()
}

#[cfg(not(target_os = "macos"))]
fn accessibility_granted() -> bool {
    true
}

#[cfg(target_os = "macos")]
fn screen_recording_granted() -> bool {
    macos_accessibility::screen_recording_granted()
}

#[cfg(not(target_os = "macos"))]
fn screen_recording_granted() -> bool {
    true
}

fn target_window(app: &tauri::AppHandle, id: u64) -> Option<DesktopWindowRect> {
    cached_windows(app)
        .into_iter()
        .find(|window| window.id == id)
}

fn throw_destination(target: &DesktopWindowRect) -> (f64, f64) {
    let max_x = (target.monitor_x + target.monitor_width - target.width).max(target.monitor_x);
    let max_y = (target.monitor_y + target.monitor_height - target.height).max(target.monitor_y);
    (
        (target.x + 220.0).clamp(target.monitor_x, max_x),
        (target.y + 24.0).clamp(target.monitor_y, max_y),
    )
}

fn pet_position_and_size(
    app: &tauri::AppHandle,
    instance_id: &str,
) -> Result<(tauri::WebviewWindow, PetPosition, (f64, f64)), String> {
    let label = instance_label(instance_id)?;
    let window = app
        .get_webview_window(&label)
        .ok_or_else(|| "宠物窗口不可用".to_string())?;
    let scale = window
        .scale_factor()
        .map_err(|error| error.to_string())?
        .max(1.0);
    let position = window
        .outer_position()
        .map_err(|error| error.to_string())?
        .to_logical(scale);
    let size = window
        .outer_size()
        .map_err(|error| error.to_string())?
        .to_logical(scale);
    Ok((
        window,
        PetPosition {
            x: position.x,
            y: position.y,
        },
        (size.width, size.height),
    ))
}

fn emit_phase(
    app: &tauri::AppHandle,
    scene_id: &str,
    instance_id: &str,
    pet_id: &str,
    phase: &str,
    animation: &str,
    look: &str,
    position: &PetPosition,
    on_window: bool,
) {
    let _ = app.emit(
        "desktop://window-scene-phase",
        serde_json::json!({
            "sceneId": scene_id,
            "instanceId": instance_id,
            "petId": pet_id,
            "phase": phase,
            "animation": animation,
            "look": look,
            "x": position.x,
            "y": position.y,
            "onWindow": on_window,
        }),
    );
}

fn set_pet_position(
    app: &tauri::AppHandle,
    window: &tauri::WebviewWindow,
    instance_id: &str,
    position: PetPosition,
) -> Result<(), String> {
    window
        .set_position(LogicalPosition::new(position.x, position.y))
        .map_err(|error| error.to_string())?;
    let _ = super::reposition_pet_speech(app, instance_id);
    Ok(())
}

async fn tween_pet(
    app: &tauri::AppHandle,
    scene: &ActiveWindowScene,
    pet_id: &str,
    window: &tauri::WebviewWindow,
    start: PetPosition,
    end: PetPosition,
    duration: Duration,
    phase: &str,
    animation: &str,
    look: &str,
    on_window: bool,
) -> bool {
    let steps = (duration.as_millis() / SCENE_TICK.as_millis()).max(1) as u32;
    for step in 0..=steps {
        if scene.cancel.load(Ordering::Relaxed) {
            return false;
        }
        let progress = step as f64 / steps as f64;
        let eased = progress * progress * (3.0 - 2.0 * progress);
        let position = PetPosition {
            x: start.x + (end.x - start.x) * eased,
            y: start.y + (end.y - start.y) * eased,
        };
        if set_pet_position(app, window, &scene.instance_id, position.clone()).is_err() {
            return false;
        }
        emit_phase(
            app,
            &scene.scene_id,
            &scene.instance_id,
            pet_id,
            phase,
            animation,
            look,
            &position,
            on_window,
        );
        tokio::time::sleep(SCENE_TICK).await;
    }
    true
}

async fn run_scene(
    app: tauri::AppHandle,
    scene: ActiveWindowScene,
    pet_id: String,
    target_id: u64,
    mode: String,
) {
    let Ok((pet_window, start, (pet_width, pet_height))) =
        pet_position_and_size(&app, &scene.instance_id)
    else {
        finish_scene(&app, &scene, true);
        return;
    };
    let Some(target) = target_window(&app, target_id) else {
        finish_scene(&app, &scene, true);
        return;
    };
    let approach = PetPosition {
        x: target.x + (target.width - pet_width) / 2.0,
        y: target.y + target.height + 14.0,
    };
    let left_edge = PetPosition {
        x: target.x - pet_width - 5.0,
        y: target.y + target.height - pet_height,
    };
    let top_edge = PetPosition {
        x: (target.x + (target.width - pet_width) * 0.5).max(target.x),
        y: target.y - pet_height,
    };

    let _ = app.emit(
        "desktop://window-scene-start",
        serde_json::json!({
            "sceneId": scene.scene_id,
            "instanceId": scene.instance_id,
            "petId": pet_id,
            "windowId": target_id,
            "mode": mode,
        }),
    );
    if !tween_pet(
        &app,
        &scene,
        &pet_id,
        &pet_window,
        start,
        approach.clone(),
        Duration::from_millis(700),
        "approach",
        "running",
        "up",
        false,
    )
    .await
        || !tween_pet(
            &app,
            &scene,
            &pet_id,
            &pet_window,
            approach,
            left_edge.clone(),
            Duration::from_millis(420),
            "climb",
            "running",
            "up",
            false,
        )
        .await
        || !tween_pet(
            &app,
            &scene,
            &pet_id,
            &pet_window,
            left_edge,
            top_edge.clone(),
            Duration::from_millis(900),
            "climb",
            "running",
            "up",
            true,
        )
        .await
    {
        finish_scene(&app, &scene, true);
        return;
    }

    let sit_until = now_ms() + if mode == "sit" { 8_000 } else { 3_000 };
    let initial_target = target.clone();
    while now_ms() < sit_until && !scene.cancel.load(Ordering::Relaxed) {
        let _ = refresh(&app);
        let Some(current_target) = target_window(&app, target_id) else {
            break;
        };
        if (current_target.x - initial_target.x).hypot(current_target.y - initial_target.y)
            > TARGET_MOVE_TOLERANCE
        {
            break;
        }
        let x = (current_target.x + (current_target.width - pet_width) * 0.5).clamp(
            current_target.x,
            (current_target.x + current_target.width - pet_width).max(current_target.x),
        );
        let position = PetPosition {
            x,
            y: current_target.y - pet_height,
        };
        if set_pet_position(&app, &pet_window, &scene.instance_id, position.clone()).is_err() {
            break;
        }
        emit_phase(
            &app,
            &scene.scene_id,
            &scene.instance_id,
            &pet_id,
            "sit",
            "waiting",
            if current_target.x > position.x {
                "right"
            } else {
                "left"
            },
            &position,
            true,
        );
        tokio::time::sleep(Duration::from_millis(180)).await;
    }

    let Some(final_target) = target_window(&app, target_id) else {
        finish_scene(&app, &scene, true);
        return;
    };
    let fall = PetPosition {
        x: final_target.x + (final_target.width - pet_width) * 0.5,
        y: final_target.y + final_target.height + 18.0,
    };
    let current = pet_position_and_size(&app, &scene.instance_id)
        .map(|(_, position, _)| position)
        .unwrap_or(top_edge);
    if !scene.cancel.load(Ordering::Relaxed) {
        let _ = tween_pet(
            &app,
            &scene,
            &pet_id,
            &pet_window,
            current,
            fall,
            Duration::from_millis(500),
            "jump-off",
            "jumping",
            "down",
            false,
        )
        .await;
    }
    finish_scene(&app, &scene, scene.cancel.load(Ordering::Relaxed));
}

fn finish_scene(app: &tauri::AppHandle, scene: &ActiveWindowScene, cancelled: bool) {
    let runtime = &app.state::<AppState>().desktop_windows;
    if let Ok(mut active) = runtime.active_scenes.lock() {
        if active
            .get(&scene.instance_id)
            .is_some_and(|current| current.scene_id == scene.scene_id)
        {
            active.remove(&scene.instance_id);
        }
    }
    let _ = app.emit(
        "desktop://window-scene-end",
        serde_json::json!({
            "sceneId": scene.scene_id,
            "instanceId": scene.instance_id,
            "cancelled": cancelled,
        }),
    );
}

#[tauri::command]
pub(crate) fn start_window_scene(
    app: tauri::AppHandle,
    instance_id: String,
    window_id: u64,
    mode: String,
) -> Result<String, String> {
    if !is_safe_id(&instance_id) || !matches!(mode.as_str(), "crawl" | "sit") {
        return Err("无效的窗口互动参数".to_string());
    }
    let config = config_snapshot(&app)?;
    let instance = config
        .instances
        .iter()
        .find(|instance| instance.id == instance_id && instance.visible)
        .ok_or_else(|| "宠物实例不可用".to_string())?;
    let settings = super::settings_for_pet(&config, &instance.pet_id);
    if !settings.window_interaction_enabled {
        return Err("请先在这只宠物的独立设置中开启窗口互动".to_string());
    }
    if settings.paused || settings.quiet_mode {
        return Err("暂停或安静模式下不能进行窗口互动".to_string());
    }
    let Some(target) = target_window(&app, window_id) else {
        return Err("目标窗口已经不存在".to_string());
    };
    let (_, position, _) = pet_position_and_size(&app, &instance_id)?;
    if !position.x.is_finite() || !position.y.is_finite() {
        return Err("宠物位置不可用".to_string());
    }
    let target_center = PetPosition {
        x: target.x + target.width / 2.0,
        y: target.y + target.height / 2.0,
    };
    if (target_center.x - position.x).hypot(target_center.y - position.y)
        > WINDOW_TARGET_MAX_DISTANCE + target.width.min(600.0) / 2.0
    {
        return Err("目标窗口离宠物太远".to_string());
    }
    let runtime = &app.state::<AppState>().desktop_windows;
    let mut active = runtime
        .active_scenes
        .lock()
        .map_err(|_| "窗口互动状态不可用".to_string())?;
    if active.contains_key(&instance_id) {
        return Err("这只宠物正在进行窗口互动".to_string());
    }
    let scene = ActiveWindowScene {
        scene_id: next_scene_id(),
        instance_id: instance_id.clone(),
        cancel: Arc::new(AtomicBool::new(false)),
    };
    let scene_id = scene.scene_id.clone();
    active.insert(instance_id, scene.clone());
    drop(active);
    tauri::async_runtime::spawn(run_scene(
        app,
        scene,
        instance.pet_id.clone(),
        target.id,
        mode,
    ));
    Ok(scene_id)
}

#[tauri::command]
pub(crate) fn cancel_window_scene(app: tauri::AppHandle, scene_id: String) -> Result<(), String> {
    if let Ok(active) = app.state::<AppState>().desktop_windows.active_scenes.lock() {
        for scene in active.values() {
            if scene.scene_id == scene_id {
                scene.cancel.store(true, Ordering::Release);
                return Ok(());
            }
        }
    }
    Ok(())
}

pub(crate) fn cancel_for_instance(app: &tauri::AppHandle, instance_id: &str) {
    if let Ok(active) = app.state::<AppState>().desktop_windows.active_scenes.lock() {
        if let Some(scene) = active.get(instance_id) {
            scene.cancel.store(true, Ordering::Release);
        }
    }
}

#[tauri::command]
pub(crate) fn throw_desktop_window(
    app: tauri::AppHandle,
    instance_id: String,
    window_id: u64,
) -> Result<(), String> {
    if !is_safe_id(&instance_id) {
        return Err("无效的宠物实例".to_string());
    }
    let config = config_snapshot(&app)?;
    let instance = config
        .instances
        .iter()
        .find(|instance| instance.id == instance_id && instance.visible)
        .ok_or_else(|| "宠物实例不可用".to_string())?;
    if !super::settings_for_pet(&config, &instance.pet_id).window_interaction_enabled {
        return Err("请先开启窗口互动".to_string());
    }
    let target = target_window(&app, window_id).ok_or_else(|| "目标窗口已经不存在".to_string())?;
    cancel_for_instance(&app, &instance_id);
    platform_throw_window(&target)
}

#[cfg(target_os = "windows")]
fn platform_throw_window(target: &DesktopWindowRect) -> Result<(), String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetAncestor, SetWindowPos, GA_ROOT, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    };
    let hwnd = unsafe { GetAncestor(HWND(target.id as isize), GA_ROOT) };
    if hwnd.0 == 0 {
        return Err("无法取得目标窗口句柄".to_string());
    }
    let (destination_x, destination_y) = throw_destination(target);
    let x = (destination_x * target.scale_factor).round() as i32;
    let y = (destination_y * target.scale_factor).round() as i32;
    let moved = unsafe {
        SetWindowPos(
            hwnd,
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
    if moved.is_ok() {
        Ok(())
    } else {
        Err("系统拒绝移动该窗口（可能是管理员权限或受保护窗口）".to_string())
    }
}

#[cfg(target_os = "macos")]
fn platform_throw_window(target: &DesktopWindowRect) -> Result<(), String> {
    macos_accessibility::move_matching_window(target)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_throw_window(_target: &DesktopWindowRect) -> Result<(), String> {
    Err("当前平台不支持移动其他应用窗口".to_string())
}

#[cfg(target_os = "macos")]
mod macos_accessibility {
    use super::DesktopWindowRect;
    use objc2_core_graphics::{CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};
    use std::ffi::{c_char, c_void, CString};
    use std::ptr;

    type CFTypeRef = *const c_void;
    type CFStringRef = CFTypeRef;
    type CFArrayRef = CFTypeRef;
    type AXUIElementRef = CFTypeRef;
    type AXValueRef = CFTypeRef;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> u8;
        fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> i32;
        fn AXUIElementSetAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: CFTypeRef,
        ) -> i32;
        fn AXValueCreate(value_type: u32, value_ptr: *const c_void) -> AXValueRef;
        fn AXValueGetValue(value: AXValueRef, value_type: u32, value_ptr: *mut c_void) -> u8;
        fn CFStringCreateWithCString(
            allocator: CFTypeRef,
            c_str: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFArrayGetCount(array: CFArrayRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFArrayRef, index: isize) -> CFTypeRef;
        fn CFRelease(value: CFTypeRef);
    }

    pub(super) fn is_trusted() -> bool {
        unsafe { AXIsProcessTrusted() != 0 }
    }

    pub(super) fn screen_recording_granted() -> bool {
        CGPreflightScreenCaptureAccess()
    }

    pub(super) fn request_screen_recording() -> bool {
        CGRequestScreenCaptureAccess()
    }

    fn attribute(name: &str) -> Option<CFStringRef> {
        let name = CString::new(name).ok()?;
        // kCFStringEncodingUTF8
        let value = unsafe { CFStringCreateWithCString(ptr::null(), name.as_ptr(), 0x0800_0100) };
        (!value.is_null()).then_some(value)
    }

    pub(super) fn move_matching_window(target: &DesktopWindowRect) -> Result<(), String> {
        if !is_trusted() {
            return Err(
                "需要在系统设置的“隐私与安全性 → 辅助功能”中允许 SakiPet 控制窗口".to_string(),
            );
        }
        let app = unsafe { AXUIElementCreateApplication(target.pid as i32) };
        if app.is_null() {
            return Err("无法连接到目标应用的辅助功能接口".to_string());
        }
        let windows_attr =
            attribute("AXWindows").ok_or_else(|| "无法创建辅助功能属性".to_string())?;
        let mut windows: CFTypeRef = ptr::null();
        let error = unsafe { AXUIElementCopyAttributeValue(app, windows_attr, &mut windows) };
        unsafe { CFRelease(windows_attr) };
        if error != 0 || windows.is_null() {
            unsafe { CFRelease(app) };
            return Err("目标应用不允许枚举窗口".to_string());
        }
        let count = unsafe { CFArrayGetCount(windows) };
        let mut matched = false;
        for index in 0..count {
            let element = unsafe { CFArrayGetValueAtIndex(windows, index) };
            let Some(position_attr) = attribute("AXPosition") else {
                continue;
            };
            let Some(size_attr) = attribute("AXSize") else {
                unsafe { CFRelease(position_attr) };
                continue;
            };
            let mut position_value: CFTypeRef = ptr::null();
            let mut size_value: CFTypeRef = ptr::null();
            let position_ok = unsafe {
                AXUIElementCopyAttributeValue(element, position_attr, &mut position_value) == 0
            };
            let size_ok =
                unsafe { AXUIElementCopyAttributeValue(element, size_attr, &mut size_value) == 0 };
            let mut position = CGPoint { x: 0.0, y: 0.0 };
            let mut size = CGSize {
                width: 0.0,
                height: 0.0,
            };
            let matches = position_ok
                && size_ok
                && unsafe {
                    AXValueGetValue(position_value, 1, &mut position as *mut _ as *mut c_void) != 0
                        && AXValueGetValue(size_value, 2, &mut size as *mut _ as *mut c_void) != 0
                }
                && (position.x - target.x).abs() <= 8.0
                && (position.y - target.y).abs() <= 8.0
                && (size.width - target.width).abs() <= 8.0
                && (size.height - target.height).abs() <= 8.0;
            if matches {
                let (destination_x, destination_y) = super::throw_destination(target);
                let destination = CGPoint {
                    x: destination_x,
                    y: destination_y,
                };
                let value = unsafe { AXValueCreate(1, &destination as *const _ as *const c_void) };
                if !value.is_null() {
                    matched =
                        unsafe { AXUIElementSetAttributeValue(element, position_attr, value) == 0 };
                    unsafe { CFRelease(value) };
                }
            }
            unsafe {
                CFRelease(position_attr);
                CFRelease(size_attr);
                if !position_value.is_null() {
                    CFRelease(position_value);
                }
                if !size_value.is_null() {
                    CFRelease(size_value);
                }
            }
            if matched {
                break;
            }
        }
        unsafe {
            CFRelease(windows);
            CFRelease(app);
        }
        if matched {
            Ok(())
        } else {
            Err("没有找到与桌面窗口对应的可移动窗口".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{changed_significantly, is_fullscreen_like, DesktopWindowRect};

    fn window(id: u64, x: f64, y: f64) -> DesktopWindowRect {
        DesktopWindowRect {
            id,
            pid: 1,
            app_name: "Test".to_string(),
            title: "Window".to_string(),
            x,
            y,
            width: 500.0,
            height: 400.0,
            monitor_key: "0:0".to_string(),
            minimized: false,
            focused: false,
            scale_factor: 1.0,
            monitor_x: 0.0,
            monitor_y: 0.0,
            monitor_width: 1920.0,
            monitor_height: 1080.0,
        }
    }

    #[test]
    fn full_screen_like_windows_are_rejected() {
        assert!(is_fullscreen_like(
            0.0, 0.0, 1920.0, 1080.0, 0.0, 0.0, 1920.0, 1080.0
        ));
        assert!(!is_fullscreen_like(
            20.0, 20.0, 500.0, 400.0, 0.0, 0.0, 1920.0, 1080.0
        ));
    }

    #[test]
    fn small_window_moves_are_not_ipc_noise() {
        let old = vec![window(1, 10.0, 10.0)];
        assert!(!changed_significantly(&old, &[window(1, 11.0, 11.0)]));
        assert!(changed_significantly(&old, &[window(1, 14.0, 10.0)]));
    }
}

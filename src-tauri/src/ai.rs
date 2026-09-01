use base64::Engine;
use futures_util::StreamExt;
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, RgbaImage};
use rand::Rng;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager};

use super::{config_snapshot, AppState, CharacterCard};

const SERVICE_NAME: &str = "com.sakipet.desktop";
const AI_DIRECTORY: &str = "ai";
const MAX_MESSAGE_CHARS: usize = 4_000;
const MAX_HISTORY_MESSAGES: usize = 200;
static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);
static LAST_HEARTBEAT_MS: AtomicU64 = AtomicU64::new(0);
static LAST_VISION_MS: AtomicU64 = AtomicU64::new(0);
static LAST_PET_CONVERSATION_MS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ProviderKind {
    OpenaiResponses,
    AnthropicMessages,
    OpenaiCompatible,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct ModelEndpointConfig {
    pub provider: ProviderKind,
    pub base_url: String,
    pub model: String,
    pub credential_ref: Option<String>,
    pub max_output_tokens: u32,
}

impl Default for ModelEndpointConfig {
    fn default() -> Self {
        Self {
            provider: ProviderKind::OpenaiResponses,
            base_url: "https://api.openai.com/v1".to_string(),
            model: String::new(),
            credential_ref: None,
            max_output_tokens: 300,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct AiSettings {
    pub enabled: bool,
    pub chat_model: Option<ModelEndpointConfig>,
    pub vision_model: Option<ModelEndpointConfig>,
    pub memory_enabled: bool,
    pub max_recent_messages: usize,
    pub heartbeat_enabled: bool,
    pub heartbeat_min_minutes: u32,
    pub heartbeat_max_minutes: u32,
    pub heartbeat_vision_chance: f64,
    pub desktop_vision_enabled: bool,
    pub pet_conversation_enabled: bool,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            chat_model: None,
            vision_model: None,
            memory_enabled: true,
            max_recent_messages: 12,
            heartbeat_enabled: true,
            heartbeat_min_minutes: 20,
            heartbeat_max_minutes: 60,
            heartbeat_vision_chance: 0.3,
            desktop_vision_enabled: false,
            pet_conversation_enabled: true,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    pub timestamp: u64,
    pub source: String,
    pub vision_summary: Option<String>,
    #[serde(default)]
    pub speaker_pet_id: Option<String>,
    #[serde(default)]
    pub speaker_name: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct PetLifeState {
    pub mood: String,
    pub energy: u8,
    pub attention: u8,
    pub bond: u8,
    pub activity: String,
    pub last_interaction_at: u64,
    pub last_spoke_at: u64,
    pub known_since: u64,
    pub interaction_count: u64,
    pub chat_count: u64,
    pub pet_interaction_count: u64,
    pub next_action_at: u64,
}

impl Default for PetLifeState {
    fn default() -> Self {
        let now = now_ms();
        Self {
            mood: "calm".to_string(),
            energy: 78,
            attention: 55,
            bond: 0,
            activity: "idle".to_string(),
            last_interaction_at: 0,
            last_spoke_at: 0,
            known_since: now,
            interaction_count: 0,
            chat_count: 0,
            pet_interaction_count: 0,
            next_action_at: 0,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct PetBehavior {
    #[serde(alias = "text")]
    pub say: String,
    #[serde(alias = "animation")]
    pub action: String,
    #[serde(alias = "emotion")]
    pub mood: String,
    pub duration: u64,
    pub next_action_after: u64,
    pub look: Option<String>,
}

impl Default for PetBehavior {
    fn default() -> Self {
        Self {
            say: String::new(),
            action: "idle".to_string(),
            mood: "calm".to_string(),
            duration: 5_200,
            next_action_after: 1_800,
            look: None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct MemoryFact {
    pub id: String,
    pub content: String,
    pub kind: String,
    pub scope: String,
    pub importance: f64,
    pub confidence: f64,
    pub created_at: u64,
    pub updated_at: u64,
    pub status: String,
    pub expires_at: Option<u64>,
}

impl Default for MemoryFact {
    fn default() -> Self {
        Self {
            id: String::new(),
            content: String::new(),
            kind: "fact".to_string(),
            scope: "pet".to_string(),
            importance: 0.5,
            confidence: 0.5,
            created_at: now_ms(),
            updated_at: now_ms(),
            status: "active".to_string(),
            expires_at: None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct ChatHistoryResponse {
    pub pet_id: String,
    pub messages: Vec<ChatMessage>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatStarted {
    pub request_id: String,
}

#[derive(Default)]
pub(crate) struct AiRuntime {
    pub tasks: Mutex<HashMap<String, (String, tauri::async_runtime::JoinHandle<()>)>>,
    pub active_pets: Mutex<HashMap<String, String>>,
    life_states: Mutex<HashMap<String, PetLifeState>>,
    screen_observation: Mutex<Option<ScreenObservation>>,
}

#[derive(Clone, Debug)]
struct ScreenObservation {
    fingerprint: u64,
    summary: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatDeltaEvent {
    request_id: String,
    pet_id: String,
    delta: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatCompleteEvent {
    request_id: String,
    pet_id: String,
    message: ChatMessage,
    behavior: Option<PetBehavior>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatErrorEvent {
    request_id: String,
    pet_id: String,
    message: String,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn request_id() -> String {
    format!(
        "req-{}-{}",
        now_ms(),
        REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Capture only the display containing the cursor. The screenshot is encoded
/// directly into memory and is never written to a temporary file.
pub(crate) async fn capture_desktop_data_url(app: &tauri::AppHandle) -> Result<String, String> {
    Ok(capture_desktop_observation(app).await?.0)
}

async fn capture_desktop_observation(app: &tauri::AppHandle) -> Result<(String, u64), String> {
    let cursor = app
        .cursor_position()
        .map_err(|error| format!("无法读取鼠标所在显示器: {error}"))?;
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || capture_desktop_sync(&app, cursor.x, cursor.y))
        .await
        .map_err(|error| format!("桌面截图任务失败: {error}"))?
}

#[allow(unused_mut, unused_variables)]
fn encode_screenshot(
    mut image: RgbaImage,
    monitor_origin: (i32, i32),
    pixel_scale: f64,
    app: &tauri::AppHandle,
) -> Result<(String, u64), String> {
    #[cfg(target_os = "macos")]
    mask_sakipet_macos(&mut image, monitor_origin, pixel_scale, app);

    #[cfg(target_os = "windows")]
    mask_sakipet_windows(&mut image, monitor_origin, pixel_scale, app);

    let fingerprint = screen_fingerprint(&image);
    let dynamic = DynamicImage::ImageRgba8(image);
    let longest = dynamic.width().max(dynamic.height());
    let resized = if longest > 1280 {
        dynamic.thumbnail(1280, 1280)
    } else {
        dynamic
    };
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, 70)
        .encode_image(&resized)
        .map_err(|error| format!("压缩桌面截图失败: {error}"))?;
    Ok((
        format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ),
        fingerprint,
    ))
}

fn screen_fingerprint(image: &RgbaImage) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let width = image.width().max(1);
    let height = image.height().max(1);
    for row in 0..18u32 {
        for column in 0..32u32 {
            let x = column.saturating_mul(width - 1) / 31;
            let y = row.saturating_mul(height - 1) / 17;
            let pixel = image.get_pixel(x, y).0;
            // Quantization ignores cursors, blinking carets and tiny animation
            // changes while still noticing a different application or scene.
            (pixel[0] / 32, pixel[1] / 32, pixel[2] / 32).hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn parse_visual_observation(raw: String, fallback_changed: bool) -> (bool, String) {
    if let Some(value) = extract_json(&raw) {
        let changed = value
            .get("changed")
            .and_then(Value::as_bool)
            .unwrap_or(fallback_changed);
        let summary = value
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return (changed, clean_reply(summary.to_string(), 600));
    }
    (fallback_changed, clean_reply(raw, 600))
}

#[cfg(target_os = "macos")]
fn capture_desktop_sync(
    app: &tauri::AppHandle,
    cursor_x: f64,
    cursor_y: f64,
) -> Result<(String, u64), String> {
    let monitor = xcap::Monitor::from_point(cursor_x.round() as i32, cursor_y.round() as i32)
        .map_err(|error| format!("无法找到鼠标所在显示器: {error}"))?;
    let origin = (
        monitor.x().map_err(|error| error.to_string())?,
        monitor.y().map_err(|error| error.to_string())?,
    );
    let pixel_scale = monitor.scale_factor().unwrap_or(1.0).max(1.0) as f64;
    let image = monitor
        .capture_image()
        .map_err(|error| format!("捕获桌面失败，请检查屏幕录制权限: {error}"))?;
    encode_screenshot(image, origin, pixel_scale, app)
}

#[cfg(target_os = "windows")]
fn capture_desktop_sync(
    app: &tauri::AppHandle,
    cursor_x: f64,
    cursor_y: f64,
) -> Result<(String, u64), String> {
    let monitor = xcap::Monitor::from_point(cursor_x.round() as i32, cursor_y.round() as i32)
        .map_err(|error| format!("无法找到鼠标所在显示器: {error}"))?;
    let origin = (
        monitor.x().map_err(|error| error.to_string())?,
        monitor.y().map_err(|error| error.to_string())?,
    );
    let image = monitor
        .capture_image()
        .map_err(|error| format!("捕获桌面失败，请检查屏幕捕获权限: {error}"))?;
    encode_screenshot(image, origin, 1.0, app)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn capture_desktop_sync(
    app: &tauri::AppHandle,
    _cursor_x: f64,
    _cursor_y: f64,
) -> Result<(String, u64), String> {
    let _ = app;
    Err("当前平台暂不支持桌面视觉".to_string())
}

#[cfg(target_os = "windows")]
fn mask_sakipet_windows(
    image: &mut RgbaImage,
    monitor_origin: (i32, i32),
    pixel_scale: f64,
    app: &tauri::AppHandle,
) {
    for (label, window) in app.webview_windows() {
        if label != "main"
            && !label.starts_with("pet-")
            && label != "pet-manager"
            && label != "ai-settings"
        {
            continue;
        }
        if !window.is_visible().unwrap_or(false) {
            continue;
        }
        let Ok(position) = window.outer_position() else {
            continue;
        };
        let Ok(size) = window.outer_size() else {
            continue;
        };
        let left = ((position.x - monitor_origin.0) as f64 * pixel_scale).floor() as i32;
        let top = ((position.y - monitor_origin.1) as f64 * pixel_scale).floor() as i32;
        let right = ((position.x - monitor_origin.0 + size.width as i32) as f64 * pixel_scale)
            .ceil() as i32;
        let bottom = ((position.y - monitor_origin.1 + size.height as i32) as f64 * pixel_scale)
            .ceil() as i32;
        let x_start = left.max(0) as u32;
        let y_start = top.max(0) as u32;
        let x_end = right.min(image.width() as i32).max(0) as u32;
        let y_end = bottom.min(image.height() as i32).max(0) as u32;
        for y in y_start..y_end {
            for x in x_start..x_end {
                *image.get_pixel_mut(x, y) = image::Rgba([0, 0, 0, 255]);
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn mask_sakipet_macos(
    image: &mut RgbaImage,
    monitor_origin: (i32, i32),
    pixel_scale: f64,
    _app: &tauri::AppHandle,
) {
    let process_id = std::process::id();
    let windows = xcap::Window::all().unwrap_or_default();
    for window in windows {
        if window.pid().ok() != Some(process_id) || window.is_minimized().unwrap_or(true) {
            continue;
        }
        let Ok(x) = window.x() else { continue };
        let Ok(y) = window.y() else { continue };
        let Ok(width) = window.width() else { continue };
        let Ok(height) = window.height() else {
            continue;
        };
        let left = ((x - monitor_origin.0) as f64 * pixel_scale).floor() as i32;
        let top = ((y - monitor_origin.1) as f64 * pixel_scale).floor() as i32;
        let right = ((x - monitor_origin.0 + width as i32) as f64 * pixel_scale).ceil() as i32;
        let bottom = ((y - monitor_origin.1 + height as i32) as f64 * pixel_scale).ceil() as i32;
        let x_start = left.max(0) as u32;
        let y_start = top.max(0) as u32;
        let x_end = right.min(image.width() as i32).max(0) as u32;
        let y_end = bottom.min(image.height() as i32).max(0) as u32;
        for y in y_start..y_end {
            for x in x_start..x_end {
                *image.get_pixel_mut(x, y) = image::Rgba([0, 0, 0, 255]);
            }
        }
    }
}

fn app_ai_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join(AI_DIRECTORY))
        .map_err(|error| format!("无法定位 AI 数据目录: {error}"))
}

fn pet_ai_path(app: &tauri::AppHandle, pet_id: &str) -> Result<PathBuf, String> {
    if !super::is_safe_id(pet_id) {
        return Err("宠物 id 无效".to_string());
    }
    Ok(app_ai_path(app)?.join("pets").join(pet_id))
}

fn life_state_path(app: &tauri::AppHandle, pet_id: &str) -> Result<PathBuf, String> {
    Ok(pet_ai_path(app, pet_id)?.join("state.json"))
}

fn load_pet_life_state_from_disk(app: &tauri::AppHandle, pet_id: &str) -> PetLifeState {
    let Ok(path) = life_state_path(app, pet_id) else {
        return PetLifeState::default();
    };
    let Ok(bytes) = fs::read(path) else {
        return PetLifeState::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn save_pet_life_state(
    app: &tauri::AppHandle,
    pet_id: &str,
    state: &PetLifeState,
) -> Result<(), String> {
    let path = life_state_path(app, pet_id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建宠物状态目录: {error}"))?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("无法保存宠物状态: {error}"))?;
    #[cfg(target_os = "windows")]
    if path.exists() {
        fs::remove_file(&path).map_err(|error| format!("无法替换宠物状态: {error}"))?;
    }
    fs::rename(temporary, path).map_err(|error| format!("无法替换宠物状态: {error}"))
}

fn advance_pet_life_state(state: &mut PetLifeState, now: u64) {
    if state.known_since == 0 {
        state.known_since = now;
    }
    if state.last_interaction_at == 0 {
        return;
    }
    let hours = now
        .saturating_sub(state.last_interaction_at)
        .checked_div(3_600_000)
        .unwrap_or(0);
    if hours > 0 {
        state.attention = state
            .attention
            .saturating_sub((hours.min(10) as u8).saturating_mul(2));
        if state.activity == "sleeping" || state.mood == "sleepy" {
            state.energy = state
                .energy
                .saturating_add((hours.min(12) as u8).saturating_mul(6))
                .min(100);
        }
    }
    if state.energy <= 20 {
        state.mood = "sleepy".to_string();
        state.activity = "sleeping".to_string();
    } else if state.energy >= 55 && state.activity == "sleeping" {
        state.mood = "calm".to_string();
        state.activity = "idle".to_string();
    } else if state.attention <= 20 {
        state.mood = "lonely".to_string();
    }
}

fn update_pet_life_state<F>(
    app: &tauri::AppHandle,
    pet_id: &str,
    update: F,
) -> Result<PetLifeState, String>
where
    F: FnOnce(&mut PetLifeState),
{
    let now = now_ms();
    let state = app.state::<AppState>();
    let mut states = state
        .ai
        .life_states
        .lock()
        .map_err(|_| "宠物状态锁失败".to_string())?;
    let life = states
        .entry(pet_id.to_string())
        .or_insert_with(|| load_pet_life_state_from_disk(app, pet_id));
    advance_pet_life_state(life, now);
    update(life);
    life.energy = life.energy.min(100);
    life.attention = life.attention.min(100);
    life.bond = life.bond.min(100);
    let snapshot = life.clone();
    save_pet_life_state(app, pet_id, &snapshot)?;
    Ok(snapshot)
}

fn pet_life_state(app: &tauri::AppHandle, pet_id: &str) -> Result<PetLifeState, String> {
    update_pet_life_state(app, pet_id, |_| {})
}

fn record_pet_interaction_internal(
    app: &tauri::AppHandle,
    pet_id: &str,
    kind: &str,
) -> Result<PetLifeState, String> {
    update_pet_life_state(app, pet_id, |state| {
        let now = now_ms();
        let normalized = kind.to_ascii_lowercase();
        state.last_interaction_at = now;
        state.interaction_count = state.interaction_count.saturating_add(1);
        match normalized.as_str() {
            "doubleclick" | "double_click" | "chat" => {
                state.attention = state.attention.saturating_add(16).min(100);
                state.bond = state.bond.saturating_add(1).min(100);
                state.mood = "happy".to_string();
                state.activity = "chatting".to_string();
                if normalized == "chat" {
                    state.chat_count = state.chat_count.saturating_add(1);
                }
            }
            "drag" => {
                state.attention = state.attention.saturating_add(6).min(100);
                state.energy = state.energy.saturating_sub(1);
                state.mood = "curious".to_string();
                state.activity = "playing".to_string();
            }
            "walk" => {
                state.attention = state.attention.saturating_add(3).min(100);
                state.energy = state.energy.saturating_sub(2);
                state.mood = "content".to_string();
                state.activity = "walking".to_string();
            }
            "speak" | "heartbeat" => {
                state.last_spoke_at = now;
                state.activity = "speaking".to_string();
            }
            "vision-change" | "vision_change" => {
                state.attention = state.attention.saturating_add(4).min(100);
                state.mood = "curious".to_string();
                state.activity = "watching".to_string();
            }
            "pet-conversation" | "pet_conversation" => {
                state.attention = state.attention.saturating_add(5).min(100);
                state.bond = state.bond.saturating_add(1).min(100);
                state.pet_interaction_count = state.pet_interaction_count.saturating_add(1);
                state.mood = "social".to_string();
                state.activity = "talking".to_string();
            }
            _ => {
                state.attention = state.attention.saturating_add(8).min(100);
                state.bond = state.bond.saturating_add(1).min(100);
                state.mood = "happy".to_string();
                state.activity = "playing".to_string();
            }
        }
    })
}

fn record_pet_behavior_internal(
    app: &tauri::AppHandle,
    pet_id: &str,
    behavior: &PetBehavior,
) -> Result<PetLifeState, String> {
    update_pet_life_state(app, pet_id, |state| {
        let now = now_ms();
        if !behavior.mood.trim().is_empty() {
            state.mood = behavior.mood.clone();
        }
        state.activity = match behavior.action.as_str() {
            "walk" => "walking",
            "sleep" => "sleeping",
            "waving" | "jumping" => "playing",
            _ if !behavior.say.trim().is_empty() => "speaking",
            _ => "idle",
        }
        .to_string();
        if !behavior.say.trim().is_empty() {
            state.last_spoke_at = now;
        }
        if behavior.next_action_after > 0 {
            state.next_action_at = now.saturating_add(behavior.next_action_after * 1_000);
        }
    })
}

fn settle_pet_activity_internal(
    app: &tauri::AppHandle,
    pet_id: &str,
) -> Result<PetLifeState, String> {
    update_pet_life_state(app, pet_id, |state| {
        state.activity = "idle".to_string();
    })
}

fn append_jsonl<T: Serialize>(path: PathBuf, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建 AI 数据目录: {error}"))?;
    }
    let line = serde_json::to_string(value).map_err(|error| error.to_string())?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("无法写入 AI 数据: {error}"))?;
    writeln!(file, "{line}").map_err(|error| format!("无法写入 AI 数据: {error}"))
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: PathBuf) -> Vec<T> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn messages_path(app: &tauri::AppHandle, pet_id: &str) -> Result<PathBuf, String> {
    Ok(pet_ai_path(app, pet_id)?.join("messages.jsonl"))
}

fn memories_path(app: &tauri::AppHandle, pet_id: &str) -> Result<PathBuf, String> {
    Ok(pet_ai_path(app, pet_id)?.join("memories.jsonl"))
}

fn load_messages(app: &tauri::AppHandle, pet_id: &str) -> Result<Vec<ChatMessage>, String> {
    Ok(read_jsonl(messages_path(app, pet_id)?))
}

fn append_message(
    app: &tauri::AppHandle,
    pet_id: &str,
    message: &ChatMessage,
) -> Result<(), String> {
    append_jsonl(messages_path(app, pet_id)?, message)
}

fn load_memories(app: &tauri::AppHandle, pet_id: &str) -> Result<Vec<MemoryFact>, String> {
    let now = now_ms();
    let mut values: HashMap<String, MemoryFact> = HashMap::new();
    for fact in read_jsonl::<MemoryFact>(memories_path(app, pet_id)?) {
        if !fact.id.is_empty() {
            values.insert(fact.id.clone(), fact);
        }
    }
    let mut facts = values
        .into_values()
        .filter(|fact| {
            fact.status == "active" && fact.expires_at.is_none_or(|expires_at| expires_at > now)
        })
        .collect::<Vec<_>>();
    facts.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(facts)
}

fn get_secret(reference: &Option<String>) -> Result<Option<String>, String> {
    let Some(reference) = reference else {
        return Ok(None);
    };
    let entry = keyring::Entry::new(SERVICE_NAME, reference)
        .map_err(|error| format!("无法访问系统密钥环: {error}"))?;
    match entry.get_password() {
        Ok(secret) if !secret.is_empty() => Ok(Some(secret)),
        Ok(_) => Ok(None),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(format!("无法读取 API Key: {error}")),
    }
}

fn normalized_endpoint(config: &ModelEndpointConfig) -> Result<(String, Option<String>), String> {
    let base = config.base_url.trim().trim_end_matches('/');
    if base.is_empty() || config.model.trim().is_empty() {
        return Err("请先填写模型地址和模型名称".to_string());
    }
    let endpoint = match config.provider {
        ProviderKind::OpenaiResponses => format!("{base}/responses"),
        ProviderKind::AnthropicMessages => format!("{base}/messages"),
        ProviderKind::OpenaiCompatible => format!("{base}/chat/completions"),
    };
    Ok((endpoint, get_secret(&config.credential_ref)?))
}

fn normalize_endpoint_config(mut config: ModelEndpointConfig) -> ModelEndpointConfig {
    config.base_url = config.base_url.trim().trim_end_matches('/').to_string();
    config.model = config.model.trim().to_string();
    config.max_output_tokens = config.max_output_tokens.clamp(1, 8_192);
    config
}

fn auth_headers(config: &ModelEndpointConfig, secret: Option<&str>) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    match config.provider {
        ProviderKind::AnthropicMessages => {
            if let Some(secret) = secret {
                headers.insert(
                    HeaderName::from_static("x-api-key"),
                    HeaderValue::from_str(secret).map_err(|error| error.to_string())?,
                );
            }
            headers.insert(
                HeaderName::from_static("anthropic-version"),
                HeaderValue::from_static("2023-06-01"),
            );
        }
        ProviderKind::OpenaiResponses | ProviderKind::OpenaiCompatible => {
            if let Some(secret) = secret {
                let value = format!("Bearer {secret}");
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&value).map_err(|error| error.to_string())?,
                );
            }
        }
    }
    Ok(headers)
}

fn message_value(message: &ChatMessage) -> Value {
    json!({"role": message.role, "content": message.content})
}

fn prompt_for(
    card: &CharacterCard,
    pet_id: &str,
    profile: &str,
    memories: &[MemoryFact],
    summary: &str,
    state: &PetLifeState,
    query: &str,
) -> String {
    let mut prompt = String::from(
        "你是 SakiPet 桌面宠物。你只能进行聊天和陪伴，不执行文件、Shell、系统控制或网络工具。\n\n",
    );
    if !card.system_prompt.is_empty() {
        prompt.push_str(&card.system_prompt);
        prompt.push('\n');
    }
    for (label, value) in [
        ("角色描述", &card.description),
        ("性格", &card.personality),
        ("场景", &card.scenario),
        ("对话示例", &card.mes_example),
        ("初始问候", &card.first_mes),
    ] {
        if !value.is_empty() {
            prompt.push_str(&format!("\n{label}：\n{value}\n"));
        }
    }
    let lore = card.relevant_lorebook(&format!("{query}\n{summary}"));
    if !lore.is_empty() {
        prompt.push_str("\n相关世界书：\n");
        prompt.push_str(&lore);
    }
    if !profile.is_empty() {
        prompt.push_str(&format!("\n共享用户资料：\n{profile}\n"));
    }
    if !memories.is_empty() {
        prompt.push_str("\n关于你们共同经历的记忆（关系记忆优先）：\n");
        for memory in memories.iter().take(8) {
            prompt.push_str(&format!("- [{}] {}\n", memory.kind, memory.content));
        }
    }
    if !summary.is_empty() {
        prompt.push_str(&format!("\n较早对话摘要：\n{summary}\n"));
    }
    let known_days = now_ms()
        .saturating_sub(state.known_since)
        .checked_div(86_400_000)
        .unwrap_or(0)
        .saturating_add(1);
    prompt.push_str(&format!(
        "\n当前宠物 ID：{pet_id}\n当前宠物状态：\n- 心情：{}\n- 精力：{} / 100\n- 注意力：{} / 100\n- 亲密度：{} / 100\n- 当前活动：{}\n- 认识用户：{} 天\n- 总互动次数：{}\n- 聊天次数：{}\n- 与其他宠物互动次数：{}\n",
        state.mood,
        state.energy,
        state.attention,
        state.bond,
        state.activity,
        known_days,
        state.interaction_count,
        state.chat_count,
        state.pet_interaction_count,
    ));
    prompt.push_str(
        "回复要求：使用自然简短的中文，保持角色语气。只返回 JSON，不要 Markdown 或解释，格式为 {\"say\":\"要说的话\",\"action\":\"idle|waving|jumping|waiting|review|walk|sleep\",\"mood\":\"当前心情\",\"look\":\"up|up-right|right|down-right|down|down-left|left|up-left|null\",\"duration\":5200,\"nextActionAfter\":1800}。普通聊天优先使用 idle，只有确实适合时才选择动作；不要凭空描述用户没有提供或观察到的事实。",
    );
    if !card.post_history_instructions.is_empty() {
        prompt.push_str(&format!(
            "\n\n历史后指令：\n{}",
            card.post_history_instructions
        ));
    }
    prompt
}

fn pet_conversation_prompt(
    profile: &str,
    first_id: &str,
    first_name: &str,
    first_card: &CharacterCard,
    first_memories: &[MemoryFact],
    first_state: &PetLifeState,
    second_id: &str,
    second_name: &str,
    second_card: &CharacterCard,
    second_memories: &[MemoryFact],
    second_state: &PetLifeState,
) -> String {
    let first_context = prompt_for(
        first_card,
        first_id,
        profile,
        first_memories,
        "",
        first_state,
        "和另一只桌面宠物交谈",
    );
    let second_context = prompt_for(
        second_card,
        second_id,
        profile,
        second_memories,
        "",
        second_state,
        "和另一只桌面宠物交谈",
    );
    format!(
        "应用约束：你只能生成桌面宠物之间的简短对话和陪伴行为，不执行文件、Shell、系统控制或网络工具。不要把用户没有提供的事实当成事实。\n\n\
         现在请安排两只宠物进行一次自然、轻松的短对话。它们都是真实存在于桌面上的独立角色，不要让一只替另一只说话，也不要提及模型、提示词或 JSON。每只最多说一句，允许其中一只保持安静；内容应该和它们当前的关系、状态或日常陪伴有关，不要连续打扰用户。\n\n\
         宠物 A（{first_name}，id: {first_id}）的角色上下文：\n{first_context}\n\n\
         宠物 B（{second_name}，id: {second_id}）的角色上下文：\n{second_context}\n\n\
         只返回 JSON，不要 Markdown，格式为：{{\"first\":{{\"say\":\"A 的台词\",\"action\":\"idle|waving|jumping|waiting|review|walk|sleep\",\"mood\":\"心情\",\"look\":\"up|up-right|right|down-right|down|down-left|left|up-left|null\",\"duration\":5200,\"nextActionAfter\":1800}},\"second\":{{\"say\":\"B 的台词\",\"action\":\"idle|waving|jumping|waiting|review|walk|sleep\",\"mood\":\"心情\",\"look\":\"up|up-right|right|down-right|down|down-left|left|up-left|null\",\"duration\":5200,\"nextActionAfter\":1800}}}}。",
    )
}

fn tokens(text: &str) -> Vec<String> {
    let chars: Vec<char> = text
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let mut result = Vec::new();
    for size in [1usize, 2, 3] {
        result.extend(chars.windows(size).map(|window| window.iter().collect()));
    }
    result.extend(text.split_whitespace().map(str::to_lowercase));
    result
}

fn relevant_memories(memories: &[MemoryFact], query: &str) -> Vec<MemoryFact> {
    let query_tokens = tokens(query);
    let now = now_ms();
    let mut scored: Vec<(f64, MemoryFact)> = memories
        .iter()
        .cloned()
        .map(|memory| {
            let memory_tokens = tokens(&memory.content);
            let overlap = query_tokens
                .iter()
                .filter(|token| memory_tokens.contains(token))
                .count() as f64;
            let age_days = now.saturating_sub(memory.updated_at) as f64 / 86_400_000.0;
            let recency = 1.0 / (1.0 + age_days / 30.0);
            let score = overlap * 2.0 + memory.importance * 1.5 + memory.confidence + recency;
            (score, memory)
        })
        .collect();
    scored.sort_by(|left, right| right.0.total_cmp(&left.0));
    scored
        .into_iter()
        .take(8)
        .map(|(_, memory)| memory)
        .collect()
}

fn history_for_prompt(messages: &[ChatMessage], max_recent: usize) -> Vec<ChatMessage> {
    let start = messages.len().saturating_sub(max_recent.max(2));
    messages[start..].to_vec()
}

fn build_payload(
    config: &ModelEndpointConfig,
    prompt: &str,
    messages: &[ChatMessage],
    image_data_url: Option<&str>,
    stream: bool,
) -> Value {
    match config.provider {
        ProviderKind::OpenaiResponses => {
            let mut input: Vec<Value> = messages
                .iter()
                .filter(|message| image_data_url.is_none() || message.id != "__vision__")
                .map(message_value)
                .collect();
            if let Some(image) = image_data_url {
                let instruction = messages
                    .iter()
                    .find(|message| message.id == "__vision__")
                    .map(|message| message.content.as_str())
                    .unwrap_or("请只描述你观察到的桌面内容，不要复述密码、令牌或联系方式。");
                input.push(json!({
                    "role":"user",
                    "content":[
                        {"type":"input_text","text":instruction},
                        {"type":"input_image","image_url":image,"detail":"low"}
                    ]
                }));
            }
            json!({
                "model": config.model,
                "instructions": prompt,
                "input": input,
                "max_output_tokens": config.max_output_tokens,
                "stream": stream,
                "store": false
            })
        }
        ProviderKind::AnthropicMessages => {
            let mut history: Vec<Value> = messages
                .iter()
                .filter(|message| image_data_url.is_none() || message.id != "__vision__")
                .map(message_value)
                .collect();
            if let Some(image) = image_data_url {
                let encoded = image
                    .split_once(',')
                    .map(|(_, value)| value)
                    .unwrap_or(image);
                let instruction = messages
                    .iter()
                    .find(|message| message.id == "__vision__")
                    .map(|message| message.content.as_str())
                    .unwrap_or("请只描述你观察到的桌面内容，不要复述密码、令牌或联系方式。");
                history.push(json!({"role":"user","content":[
                    {"type":"text","text":instruction},
                    {"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":encoded}}
                ]}));
            }
            json!({
                "model": config.model,
                "system": prompt,
                "messages": history,
                "max_tokens": config.max_output_tokens,
                "stream": stream
            })
        }
        ProviderKind::OpenaiCompatible => {
            let mut history = vec![json!({"role":"system","content":prompt})];
            history.extend(messages.iter().map(|message| {
                if image_data_url.is_some() && message.id == "__vision__" {
                    let instruction = message.content.as_str();
                    json!({"role":"user","content":[
                        {"type":"text","text":instruction},
                        {"type":"image_url","image_url":{"url":image_data_url.unwrap()}}
                    ]})
                } else {
                    message_value(message)
                }
            }));
            json!({
                "model": config.model,
                "messages": history,
                "max_tokens": config.max_output_tokens,
                "stream": stream
            })
        }
    }
}

fn extract_text(value: &Value, provider: &ProviderKind) -> String {
    match provider {
        ProviderKind::OpenaiResponses => value
            .get("output_text")
            .and_then(Value::as_str)
            .or_else(|| {
                value
                    .pointer("/output/0/content/0/text")
                    .and_then(Value::as_str)
            })
            .unwrap_or_default()
            .to_string(),
        ProviderKind::AnthropicMessages => value
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        ProviderKind::OpenaiCompatible => value
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    }
}

fn stream_delta(value: &Value, provider: &ProviderKind) -> Option<String> {
    match provider {
        ProviderKind::OpenaiResponses
            if value.get("type").and_then(Value::as_str) == Some("response.output_text.delta") =>
        {
            value
                .get("delta")
                .and_then(Value::as_str)
                .map(str::to_string)
        }
        ProviderKind::AnthropicMessages
            if value.get("type").and_then(Value::as_str) == Some("content_block_delta") =>
        {
            value
                .pointer("/delta/text")
                .and_then(Value::as_str)
                .map(str::to_string)
        }
        ProviderKind::OpenaiCompatible => value
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

async fn response_text(
    response: reqwest::Response,
    provider: &ProviderKind,
    mut on_delta: impl FnMut(String),
) -> Result<String, String> {
    let mut bytes = response.bytes_stream();
    let mut buffer = String::new();
    let mut text = String::new();
    while let Some(chunk) = bytes.next().await {
        let chunk = chunk.map_err(|error| format!("读取模型响应失败: {error}"))?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some((index, separator_len)) = sse_boundary(&buffer) {
            let event = buffer[..index].to_string();
            buffer.drain(..index + separator_len);
            for line in event.lines().filter_map(|line| line.strip_prefix("data:")) {
                let data = line.trim();
                if data == "[DONE]" {
                    continue;
                }
                let value: Value = match serde_json::from_str(data) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                if let Some(delta) = stream_delta(&value, provider) {
                    text.push_str(&delta);
                    on_delta(delta);
                }
            }
        }
    }
    for line in buffer.lines().filter_map(|line| line.strip_prefix("data:")) {
        let data = line.trim();
        if data == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(delta) = stream_delta(&value, provider) {
            text.push_str(&delta);
            on_delta(delta);
        } else if text.is_empty() {
            let fallback = extract_text(&value, provider);
            if !fallback.is_empty() {
                text = fallback;
            }
        }
    }
    if text.is_empty() && !buffer.trim().is_empty() {
        if let Ok(value) = serde_json::from_str::<Value>(buffer.trim()) {
            text = extract_text(&value, provider);
        }
    }
    Ok(text)
}

fn sse_boundary(buffer: &str) -> Option<(usize, usize)> {
    match (buffer.find("\r\n\r\n"), buffer.find("\n\n")) {
        (Some(crlf), Some(lf)) if crlf < lf => Some((crlf, 4)),
        (Some(crlf), _) => Some((crlf, 4)),
        (_, Some(lf)) => Some((lf, 2)),
        _ => None,
    }
}

async fn call_stream(
    client: &reqwest::Client,
    config: &ModelEndpointConfig,
    prompt: &str,
    messages: &[ChatMessage],
    image_data_url: Option<&str>,
    stream: bool,
    on_delta: impl FnMut(String),
) -> Result<String, String> {
    let (endpoint, secret) = normalized_endpoint(config)?;
    if matches!(
        config.provider,
        ProviderKind::OpenaiResponses | ProviderKind::OpenaiCompatible
    ) && config.base_url.starts_with("https://api.openai.com")
        && secret.is_none()
    {
        return Err("未配置 OpenAI API Key".to_string());
    }
    if matches!(config.provider, ProviderKind::AnthropicMessages) && secret.is_none() {
        return Err("未配置 Anthropic API Key".to_string());
    }
    let mut request = client
        .post(endpoint)
        .headers(auth_headers(config, secret.as_deref())?)
        .json(&build_payload(
            config,
            prompt,
            messages,
            image_data_url,
            stream,
        ));
    if !stream {
        request = request.header(ACCEPT, "application/json");
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("连接模型失败: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "模型返回 HTTP {}: {}",
            status.as_u16(),
            body.chars().take(400).collect::<String>()
        ));
    }
    if stream {
        response_text(response, &config.provider, on_delta).await
    } else {
        let value = response
            .json::<Value>()
            .await
            .map_err(|error| format!("解析模型响应失败: {error}"))?;
        Ok(extract_text(&value, &config.provider))
    }
}

fn card_for_pet(app: &tauri::AppHandle, pet_id: &str) -> Result<CharacterCard, String> {
    super::load_pet_character(app, pet_id)
}

fn profile_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app_ai_path(app)?.join("profile.json"))
}

fn load_profile(app: &tauri::AppHandle) -> String {
    load_shared_memories(app)
        .into_iter()
        .filter(|fact| fact.status == "active")
        .map(|fact| format!("- {}", fact.content))
        .collect::<Vec<_>>()
        .join("\n")
}

fn load_shared_memories(app: &tauri::AppHandle) -> Vec<MemoryFact> {
    let now = now_ms();
    let path = profile_path(app).unwrap_or_default();
    fs::read_to_string(path)
        .ok()
        .and_then(|value| serde_json::from_str::<Vec<MemoryFact>>(&value).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|fact| {
            !fact.id.is_empty() && fact.expires_at.is_none_or(|expires_at| expires_at > now)
        })
        .collect::<Vec<_>>()
}

fn write_shared_memories(app: &tauri::AppHandle, facts: &[MemoryFact]) -> Result<(), String> {
    let path = profile_path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(facts).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    #[cfg(target_os = "windows")]
    if path.exists() {
        fs::remove_file(&path).map_err(|error| error.to_string())?;
    }
    fs::rename(temporary, path).map_err(|error| error.to_string())
}

fn append_shared_memory(app: &tauri::AppHandle, fact: &MemoryFact) -> Result<(), String> {
    let mut facts = load_shared_memories(app);
    if let Some(existing) = facts.iter_mut().find(|existing| existing.id == fact.id) {
        *existing = fact.clone();
        return write_shared_memories(app, &facts);
    }
    if facts
        .iter()
        .any(|existing| existing.status == "active" && existing.content == fact.content)
    {
        return Ok(());
    }
    facts.push(fact.clone());
    write_shared_memories(app, &facts)
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct SummaryFile {
    summary: String,
    updated_at: u64,
}

fn load_summary(app: &tauri::AppHandle, pet_id: &str) -> String {
    let path = pet_ai_path(app, pet_id)
        .unwrap_or_default()
        .join("summary.json");
    let Ok(value) = fs::read_to_string(path) else {
        return String::new();
    };
    serde_json::from_str::<SummaryFile>(&value)
        .map(|file| file.summary)
        .unwrap_or(value)
}

fn save_summary(app: &tauri::AppHandle, pet_id: &str, summary: &str) -> Result<(), String> {
    let path = pet_ai_path(app, pet_id)?.join("summary.json");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temporary = path.with_extension("json.tmp");
    let value = SummaryFile {
        summary: summary.to_string(),
        updated_at: now_ms(),
    };
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    #[cfg(target_os = "windows")]
    if path.exists() {
        fs::remove_file(&path).map_err(|error| error.to_string())?;
    }
    fs::rename(temporary, path).map_err(|error| error.to_string())
}

fn choose_memories(app: &tauri::AppHandle, pet_id: &str, query: &str) -> Vec<MemoryFact> {
    relevant_memories(&load_memories(app, pet_id).unwrap_or_default(), query)
}

fn choose_relationship_memories(app: &tauri::AppHandle, pet_id: &str) -> Vec<MemoryFact> {
    let memories = load_memories(app, pet_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|memory| memory.kind == "relationship")
        .collect::<Vec<_>>();
    relevant_memories(&memories, "宠物之间的互动和共同经历")
}

fn clean_reply(text: String, max_chars: usize) -> String {
    let text = text.trim().trim_matches('`').trim().to_string();
    if text.len() > max_chars {
        text.chars().take(max_chars).collect()
    } else {
        text
    }
}

fn extract_json(text: &str) -> Option<Value> {
    let clean = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(value) = serde_json::from_str(clean) {
        return Some(value);
    }
    let start = clean.find('{')?;
    let end = clean.rfind('}')?;
    (start < end)
        .then(|| serde_json::from_str(&clean[start..=end]).ok())
        .flatten()
}

fn normalize_behavior(mut behavior: PetBehavior) -> PetBehavior {
    behavior.action = match behavior.action.trim().to_ascii_lowercase().as_str() {
        "idle" | "waving" | "jumping" | "waiting" | "review" | "walk" | "sleep" => {
            behavior.action.trim().to_ascii_lowercase()
        }
        _ => "idle".to_string(),
    };
    behavior.mood = behavior.mood.trim().chars().take(32).collect::<String>();
    if behavior.mood.is_empty() {
        behavior.mood = "calm".to_string();
    }
    behavior.look = behavior.look.take().and_then(|look| {
        let look = look.trim().to_ascii_lowercase();
        let valid_name = matches!(
            look.as_str(),
            "up" | "up-right" | "right" | "down-right" | "down" | "down-left" | "left" | "up-left"
        );
        let valid_index = look.parse::<u8>().ok().filter(|value| *value < 16);
        if valid_name || valid_index.is_some() {
            Some(look)
        } else {
            None
        }
    });
    behavior.say = clean_reply(behavior.say, 600);
    behavior.duration = behavior.duration.clamp(2_500, 12_000);
    behavior.next_action_after = behavior.next_action_after.clamp(30, 7_200);
    behavior
}

fn parse_behavior_response(raw: String, max_chars: usize) -> (String, PetBehavior) {
    if let Some(value) = extract_json(&raw) {
        if let Ok(behavior) = serde_json::from_value::<PetBehavior>(value) {
            let mut behavior = normalize_behavior(behavior);
            behavior.say = clean_reply(behavior.say, max_chars);
            return (behavior.say.clone(), behavior);
        }
    }
    let say = clean_reply(raw, max_chars);
    let behavior = normalize_behavior(PetBehavior {
        say: say.clone(),
        ..PetBehavior::default()
    });
    (say, behavior)
}

#[derive(Clone, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct PetConversationResponse {
    first: PetBehavior,
    second: PetBehavior,
}

fn parse_pet_conversation_response(raw: String) -> Option<(PetBehavior, PetBehavior)> {
    let value = extract_json(&raw)?;
    let response = serde_json::from_value::<PetConversationResponse>(value).ok()?;
    let mut first = normalize_behavior(response.first);
    let mut second = normalize_behavior(response.second);
    first.say = clean_reply(first.say, 100);
    second.say = clean_reply(second.say, 100);
    if first.say.is_empty() && second.say.is_empty() {
        return None;
    }
    Some((first, second))
}

fn conversation_history(
    first: &[ChatMessage],
    second: &[ChatMessage],
    first_id: &str,
    first_name: &str,
    second_id: &str,
    second_name: &str,
) -> Vec<ChatMessage> {
    let mut recent: Vec<(u64, String)> = first
        .iter()
        .chain(second.iter())
        .filter(|message| message.source == "pet-conversation")
        .map(|message| {
            let name = message
                .speaker_name
                .as_deref()
                .or_else(|| {
                    message.speaker_pet_id.as_deref().map(|speaker| {
                        if speaker == first_id {
                            first_name
                        } else if speaker == second_id {
                            second_name
                        } else {
                            speaker
                        }
                    })
                })
                .unwrap_or(first_name);
            (message.timestamp, format!("{name}：{}", message.content))
        })
        .collect();
    recent.sort_by_key(|(timestamp, _)| *timestamp);
    recent.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
    let lines = recent
        .into_iter()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|(_, line)| line)
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return Vec::new();
    }
    vec![ChatMessage {
        id: "__pet_conversation_history__".to_string(),
        role: "user".to_string(),
        content: format!("最近的宠物间对话：\n{}", lines.join("\n")),
        timestamp: now_ms(),
        source: "pet-conversation-context".to_string(),
        vision_summary: None,
        speaker_pet_id: None,
        speaker_name: None,
    }]
}

#[derive(Clone, Debug)]
struct MemoryOperation {
    action: String,
    target: String,
    content: String,
    kind: String,
    scope: String,
    importance: f64,
    confidence: f64,
    expires_in_hours: Option<u64>,
}

async fn extract_memories(
    client: &reqwest::Client,
    config: &ModelEndpointConfig,
    messages: &[ChatMessage],
) -> Result<Vec<MemoryOperation>, String> {
    let prompt = "你是记忆提取器。只保留未来仍有用的信息，不记录一次性闲聊。记忆类型 kind 使用 preference（偏好）、profile（资料）、event（共同经历）、impression（宠物对用户的印象）、temporary（今天或短期状态）、relationship（关系事件）。只返回 JSON：{\"facts\":[{\"action\":\"add|update|forget\",\"target\":\"要修改或遗忘的原记忆内容，可为空\",\"content\":\"新的记忆内容\",\"kind\":\"preference|profile|event|impression|temporary|relationship\",\"scope\":\"shared|pet\",\"importance\":0.0,\"confidence\":0.0,\"expiresInHours\":24}]}。temporary 必须设置 expiresInHours（通常 24 到 72）；长期记忆省略它。没有记忆就返回空数组。不要把普通寒暄写入记忆。";
    let value = call_stream(client, config, prompt, messages, None, false, |_| {}).await?;
    let Some(value) = extract_json(&value) else {
        return Ok(Vec::new());
    };
    Ok(value
        .get("facts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|fact| {
            let action = fact
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("add")
                .to_string();
            if !matches!(action.as_str(), "add" | "update" | "forget") {
                return None;
            }
            let content = fact
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let target = fact
                .get("target")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let target = if target.is_empty() && action == "forget" {
                content.clone()
            } else {
                target
            };
            if (action != "forget" && content.is_empty())
                || (action == "forget" && target.is_empty())
            {
                return None;
            }
            Some(MemoryOperation {
                action,
                target: target.chars().take(300).collect(),
                content: content.chars().take(300).collect(),
                kind: fact
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("fact")
                    .to_string(),
                scope: fact
                    .get("scope")
                    .and_then(Value::as_str)
                    .unwrap_or("pet")
                    .to_string(),
                importance: fact
                    .get("importance")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.5)
                    .clamp(0.0, 1.0),
                confidence: fact
                    .get("confidence")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.7)
                    .clamp(0.0, 1.0),
                expires_in_hours: fact
                    .get("expiresInHours")
                    .and_then(Value::as_u64)
                    .map(|hours| hours.clamp(1, 24 * 30)),
            })
        })
        .collect())
}

fn memory_from_operation(operation: &MemoryOperation, id: Option<String>) -> MemoryFact {
    let now = now_ms();
    MemoryFact {
        id: id.unwrap_or_else(|| {
            format!(
                "memory-{now}-{}",
                REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)
            )
        }),
        content: operation.content.clone(),
        kind: operation.kind.clone(),
        scope: if operation.scope == "shared" {
            "shared".to_string()
        } else {
            "pet".to_string()
        },
        importance: operation.importance,
        confidence: operation.confidence,
        created_at: now,
        updated_at: now,
        status: "active".to_string(),
        expires_at: operation
            .expires_in_hours
            .or_else(|| (operation.kind == "temporary").then_some(48))
            .map(|hours| now.saturating_add(hours * 3_600_000)),
    }
}

fn forget_memory(
    app: &tauri::AppHandle,
    pet_id: &str,
    operation: &MemoryOperation,
) -> Result<(), String> {
    if operation.scope == "shared" {
        let mut facts = load_shared_memories(app);
        for fact in facts
            .iter_mut()
            .filter(|fact| fact.id == operation.target || fact.content == operation.target)
        {
            fact.status = "deleted".to_string();
            fact.updated_at = now_ms();
        }
        return write_shared_memories(app, &facts);
    }
    let matching_id = load_memories(app, pet_id)?
        .into_iter()
        .find(|fact| fact.id == operation.target || fact.content == operation.target)
        .map(|fact| fact.id)
        .unwrap_or_else(|| operation.target.clone());
    append_jsonl(
        memories_path(app, pet_id)?,
        &MemoryFact {
            id: matching_id,
            status: "deleted".to_string(),
            ..MemoryFact::default()
        },
    )
}

fn store_memory(app: &tauri::AppHandle, pet_id: &str, fact: &MemoryFact) -> Result<(), String> {
    if fact.content.trim().is_empty() {
        return Ok(());
    }
    if fact.scope == "shared" {
        return append_shared_memory(app, fact);
    }
    let path = memories_path(app, pet_id)?;
    let existing = load_memories(app, pet_id)?;
    if existing.iter().any(|memory| memory.content == fact.content) {
        return Ok(());
    }
    append_jsonl(path, fact)
}

async fn refresh_summary(
    app: tauri::AppHandle,
    pet_id: String,
    endpoint: ModelEndpointConfig,
    messages: Vec<ChatMessage>,
) {
    let keep_recent = 12usize;
    let older = messages.len().saturating_sub(keep_recent);
    if older < 1 {
        return;
    }
    let prompt = "你是桌面宠物的对话摘要器。把较早的聊天整理成一段简洁、客观、可供角色继续陪伴使用的中文摘要。保留用户明确表达的偏好、重要计划和共同经历，不编造信息，不写分析过程，不超过 1200 个中文字符。只返回摘要正文。";
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            eprintln!("summary client failed: {error}");
            return;
        }
    };
    let older_messages = messages.into_iter().take(older).collect::<Vec<_>>();
    match call_stream(
        &client,
        &endpoint,
        prompt,
        &older_messages,
        None,
        false,
        |_| {},
    )
    .await
    {
        Ok(summary) => {
            let summary = clean_reply(summary, 4_800);
            if !summary.is_empty() {
                if let Err(error) = save_summary(&app, &pet_id, &summary) {
                    eprintln!("summary save failed: {error}");
                }
            }
        }
        Err(error) => eprintln!("summary request failed: {error}"),
    }
}

async fn run_chat_task(
    app: tauri::AppHandle,
    pet_id: String,
    request_id: String,
    content: String,
) -> Result<(), String> {
    let config = config_snapshot(&app)?;
    let ai = config.ai.clone();
    if !ai.enabled {
        return Err("AI 对话未启用".to_string());
    }
    let endpoint = ai
        .chat_model
        .clone()
        .ok_or_else(|| "尚未配置聊天模型".to_string())?;
    let card = card_for_pet(&app, &pet_id)?;
    let mut messages = load_messages(&app, &pet_id)?;
    let user_message = ChatMessage {
        id: format!("message-{}", now_ms()),
        role: "user".to_string(),
        content: content.clone(),
        timestamp: now_ms(),
        source: "chat".to_string(),
        vision_summary: None,
        speaker_pet_id: None,
        speaker_name: None,
    };
    append_message(&app, &pet_id, &user_message)?;
    messages.push(user_message);
    let memory = if ai.memory_enabled {
        choose_memories(&app, &pet_id, &content)
    } else {
        Vec::new()
    };
    let state = record_pet_interaction_internal(&app, &pet_id, "chat")?;
    let mut prompt = prompt_for(
        &card,
        &pet_id,
        &load_profile(&app),
        &memory,
        &load_summary(&app, &pet_id),
        &state,
        &content,
    );
    prompt.push_str(&desktop_context_prompt(&app, &config, &pet_id));
    let recent = history_for_prompt(&messages, ai.max_recent_messages);
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|error| error.to_string())?;
    let delta_app = app.clone();
    let delta_pet = pet_id.clone();
    let delta_request = request_id.clone();
    let reply = call_stream(
        &client,
        &endpoint,
        &prompt,
        &recent,
        None,
        true,
        move |delta| {
            let _ = delta_app.emit(
                "chat://delta",
                ChatDeltaEvent {
                    request_id: delta_request.clone(),
                    pet_id: delta_pet.clone(),
                    delta,
                },
            );
        },
    )
    .await?;
    let (reply, behavior) = parse_behavior_response(reply, 600);
    if reply.is_empty() {
        return Err("模型没有返回文字".to_string());
    }
    let assistant = ChatMessage {
        id: format!("message-{}", now_ms()),
        role: "assistant".to_string(),
        content: reply,
        timestamp: now_ms(),
        source: "chat".to_string(),
        vision_summary: None,
        speaker_pet_id: None,
        speaker_name: None,
    };
    append_message(&app, &pet_id, &assistant)?;
    record_pet_behavior_internal(&app, &pet_id, &behavior)?;
    let _ = app.emit(
        "chat://complete",
        ChatCompleteEvent {
            request_id,
            pet_id: pet_id.clone(),
            message: assistant.clone(),
            behavior: Some(behavior),
        },
    );
    let memory_messages = {
        let mut messages = messages;
        messages.push(assistant.clone());
        messages
    };
    if ai.memory_enabled {
        let extraction_client = client.clone();
        let extraction_config = endpoint.clone();
        let extraction_messages = memory_messages.clone();
        let app_for_memory = app.clone();
        let pet_for_memory = pet_id.clone();
        tauri::async_runtime::spawn(async move {
            if let Ok(operations) =
                extract_memories(&extraction_client, &extraction_config, &extraction_messages).await
            {
                for operation in operations {
                    match operation.action.as_str() {
                        "forget" => {
                            let _ = forget_memory(&app_for_memory, &pet_for_memory, &operation);
                        }
                        "update" => {
                            let existing_id = if operation.scope == "shared" {
                                load_shared_memories(&app_for_memory)
                                    .into_iter()
                                    .find(|fact| {
                                        fact.id == operation.target
                                            || fact.content == operation.target
                                    })
                                    .map(|fact| fact.id)
                            } else {
                                load_memories(&app_for_memory, &pet_for_memory)
                                    .ok()
                                    .and_then(|facts| {
                                        facts
                                            .into_iter()
                                            .find(|fact| {
                                                fact.id == operation.target
                                                    || fact.content == operation.target
                                            })
                                            .map(|fact| fact.id)
                                    })
                            };
                            let fact = memory_from_operation(&operation, existing_id);
                            let _ = store_memory(&app_for_memory, &pet_for_memory, &fact);
                        }
                        _ => {
                            let fact = memory_from_operation(&operation, None);
                            let _ = store_memory(&app_for_memory, &pet_for_memory, &fact);
                        }
                    }
                }
            }
        });
    }
    if memory_messages.len() > 40 {
        let app_for_summary = app.clone();
        let pet_for_summary = pet_id.clone();
        let summary_endpoint = endpoint.clone();
        tauri::async_runtime::spawn(refresh_summary(
            app_for_summary,
            pet_for_summary,
            summary_endpoint,
            memory_messages,
        ));
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn get_ai_settings(app: tauri::AppHandle) -> Result<AiSettings, String> {
    Ok(config_snapshot(&app)?.ai)
}

#[tauri::command]
pub(crate) fn update_ai_settings(
    app: tauri::AppHandle,
    mut settings: AiSettings,
) -> Result<AiSettings, String> {
    settings.chat_model = settings.chat_model.map(normalize_endpoint_config);
    settings.vision_model = settings.vision_model.map(normalize_endpoint_config);
    settings.max_recent_messages = settings.max_recent_messages.clamp(2, 40);
    settings.heartbeat_min_minutes = settings.heartbeat_min_minutes.clamp(1, 1_440);
    settings.heartbeat_max_minutes = settings
        .heartbeat_max_minutes
        .max(settings.heartbeat_min_minutes)
        .clamp(1, 1_440);
    settings.heartbeat_vision_chance = settings.heartbeat_vision_chance.clamp(0.0, 1.0);
    let config = super::update_config(&app, |config| {
        config.ai = settings.clone();
        Ok(())
    })?;
    Ok(config.ai)
}

#[tauri::command]
pub(crate) fn set_ai_secret(reference: String, secret: String) -> Result<(), String> {
    if reference.len() > 100 || reference.is_empty() {
        return Err("密钥引用无效".to_string());
    }
    let entry = keyring::Entry::new(SERVICE_NAME, &reference).map_err(|error| error.to_string())?;
    entry
        .set_password(&secret)
        .map_err(|error| format!("保存 API Key 失败: {error}"))
}

#[tauri::command]
pub(crate) fn delete_ai_secret(reference: String) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE_NAME, &reference).map_err(|error| error.to_string())?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[tauri::command]
pub(crate) async fn test_ai_provider(
    config: ModelEndpointConfig,
    vision: bool,
) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| error.to_string())?;
    let prompt = if vision {
        "请回复 OK，表示你可以处理图片输入。"
    } else {
        "请只回复 OK。"
    };
    let mut messages = Vec::new();
    let image = if vision {
        Some(test_image_data_url()?)
    } else {
        None
    };
    if vision {
        messages.push(ChatMessage {
            id: "__vision__".to_string(),
            role: "user".to_string(),
            content: "测试图片".to_string(),
            timestamp: now_ms(),
            source: "test".to_string(),
            vision_summary: None,
            speaker_pet_id: None,
            speaker_name: None,
        });
    }
    let result = call_stream(
        &client,
        &config,
        prompt,
        &messages,
        image.as_deref(),
        false,
        |_| {},
    )
    .await?;
    Ok(result.chars().take(80).collect())
}

fn test_image_data_url() -> Result<String, String> {
    let image = RgbaImage::from_pixel(2, 2, image::Rgba([157, 218, 228, 255]));
    let dynamic = DynamicImage::ImageRgba8(image);
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, 70)
        .encode_image(&dynamic)
        .map_err(|error| error.to_string())?;
    Ok(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

#[tauri::command]
pub(crate) async fn capture_desktop(app: tauri::AppHandle) -> Result<String, String> {
    capture_desktop_data_url(&app).await
}

#[tauri::command]
pub(crate) fn get_pet_state(app: tauri::AppHandle, pet_id: String) -> Result<PetLifeState, String> {
    pet_life_state(&app, &pet_id)
}

#[tauri::command]
pub(crate) fn record_pet_interaction(
    app: tauri::AppHandle,
    pet_id: String,
    kind: String,
) -> Result<PetLifeState, String> {
    record_pet_interaction_internal(&app, &pet_id, &kind)
}

#[tauri::command]
pub(crate) fn settle_pet_activity(
    app: tauri::AppHandle,
    pet_id: String,
) -> Result<PetLifeState, String> {
    settle_pet_activity_internal(&app, &pet_id)
}

#[tauri::command]
pub(crate) fn get_chat_history(
    app: tauri::AppHandle,
    pet_id: String,
) -> Result<ChatHistoryResponse, String> {
    Ok(ChatHistoryResponse {
        pet_id: pet_id.clone(),
        messages: load_messages(&app, &pet_id)?
            .into_iter()
            .rev()
            .take(MAX_HISTORY_MESSAGES)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
    })
}

#[tauri::command]
pub(crate) fn send_chat_message(
    app: tauri::AppHandle,
    pet_id: String,
    content: String,
) -> Result<ChatStarted, String> {
    let content = content.trim().to_string();
    if content.is_empty() || content.chars().count() > MAX_MESSAGE_CHARS {
        return Err("消息不能为空且不能超过 4000 字".to_string());
    }
    let request_id = request_id();
    let state = app.state::<AppState>();
    {
        let mut active_pets = state
            .ai
            .active_pets
            .lock()
            .map_err(|_| "AI 任务锁失败".to_string())?;
        if active_pets.contains_key(&pet_id) {
            return Err("这只宠物正在回复，请先停止当前回复".to_string());
        }
        active_pets.insert(pet_id.clone(), request_id.clone());
        let handle = tauri::async_runtime::spawn({
            let app = app.clone();
            let pet_id = pet_id.clone();
            let request_id = request_id.clone();
            async move {
                let result =
                    run_chat_task(app.clone(), pet_id.clone(), request_id.clone(), content).await;
                if let Err(message) = result {
                    let _ = app.emit(
                        "chat://error",
                        ChatErrorEvent {
                            request_id: request_id.clone(),
                            pet_id: pet_id.clone(),
                            message,
                        },
                    );
                }
                if let Some(state) = app.try_state::<AppState>() {
                    if let Ok(mut active_pets) = state.ai.active_pets.lock() {
                        if active_pets
                            .get(&pet_id)
                            .is_some_and(|active_id| active_id == &request_id)
                        {
                            active_pets.remove(&pet_id);
                        }
                    }
                    if let Ok(mut tasks) = state.ai.tasks.lock() {
                        if tasks
                            .get(&pet_id)
                            .is_some_and(|(task_id, _)| task_id == &request_id)
                        {
                            tasks.remove(&pet_id);
                        }
                    }
                }
            }
        });
        match state.ai.tasks.lock() {
            Ok(mut tasks) => {
                tasks.insert(pet_id, (request_id.clone(), handle));
            }
            Err(_) => {
                active_pets.remove(&pet_id);
                handle.abort();
                return Err("AI 任务锁失败".to_string());
            }
        }
    }
    Ok(ChatStarted { request_id })
}

#[tauri::command]
pub(crate) fn cancel_chat_response(app: tauri::AppHandle, pet_id: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    let task = state
        .ai
        .tasks
        .lock()
        .map_err(|_| "AI 任务锁失败".to_string())?
        .remove(&pet_id);
    if let Some((request_id, handle)) = task {
        handle.abort();
        if let Ok(mut active_pets) = state.ai.active_pets.lock() {
            if active_pets
                .get(&pet_id)
                .is_some_and(|active_id| active_id == &request_id)
            {
                active_pets.remove(&pet_id);
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn clear_chat_history(app: tauri::AppHandle, pet_id: String) -> Result<(), String> {
    let path = messages_path(&app, &pet_id)?;
    if path.exists() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn get_memories(
    app: tauri::AppHandle,
    pet_id: String,
) -> Result<Vec<MemoryFact>, String> {
    load_memories(&app, &pet_id)
}

#[tauri::command]
pub(crate) fn delete_memory(
    app: tauri::AppHandle,
    pet_id: String,
    memory_id: String,
) -> Result<(), String> {
    let fact = MemoryFact {
        id: memory_id,
        status: "deleted".to_string(),
        ..MemoryFact::default()
    };
    append_jsonl(memories_path(&app, &pet_id)?, &fact)
}

#[tauri::command]
pub(crate) fn update_memory(
    app: tauri::AppHandle,
    pet_id: String,
    mut memory: MemoryFact,
) -> Result<(), String> {
    if memory.id.is_empty()
        || memory.id.len() > 100
        || memory.content.trim().is_empty()
        || memory.content.chars().count() > 300
    {
        return Err("记忆内容不能为空且不能超过 300 个字符".to_string());
    }
    if memory.scope == "shared" {
        return update_shared_memory(app, memory);
    }
    memory.scope = "pet".to_string();
    memory.status = "active".to_string();
    memory.content = memory.content.trim().to_string();
    memory.updated_at = now_ms();
    append_jsonl(memories_path(&app, &pet_id)?, &memory)
}

#[tauri::command]
pub(crate) fn clear_memories(app: tauri::AppHandle, pet_id: String) -> Result<(), String> {
    let path = memories_path(&app, &pet_id)?;
    if path.exists() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn get_shared_memories(app: tauri::AppHandle) -> Result<Vec<MemoryFact>, String> {
    Ok(load_shared_memories(&app)
        .into_iter()
        .filter(|fact| fact.status == "active")
        .collect())
}

#[tauri::command]
pub(crate) fn delete_shared_memory(app: tauri::AppHandle, memory_id: String) -> Result<(), String> {
    let mut facts = load_shared_memories(&app);
    for fact in facts.iter_mut().filter(|fact| fact.id == memory_id) {
        fact.status = "deleted".to_string();
        fact.updated_at = now_ms();
    }
    write_shared_memories(&app, &facts)
}

#[tauri::command]
pub(crate) fn update_shared_memory(
    app: tauri::AppHandle,
    mut memory: MemoryFact,
) -> Result<(), String> {
    if memory.id.is_empty()
        || memory.content.trim().is_empty()
        || memory.content.chars().count() > 300
    {
        return Err("记忆内容不能为空且不能超过 300 个字符".to_string());
    }
    memory.scope = "shared".to_string();
    memory.status = "active".to_string();
    memory.content = memory.content.trim().to_string();
    memory.updated_at = now_ms();
    let mut facts = load_shared_memories(&app);
    if let Some(existing) = facts.iter_mut().find(|fact| fact.id == memory.id) {
        *existing = memory;
    } else {
        facts.push(memory);
    }
    write_shared_memories(&app, &facts)
}

#[tauri::command]
pub(crate) fn clear_shared_memories(app: tauri::AppHandle) -> Result<(), String> {
    let path = profile_path(&app)?;
    if path.exists() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(crate) fn random_heartbeat_delay(settings: &AiSettings) -> Duration {
    let mut rng = rand::rng();
    let min = settings
        .heartbeat_min_minutes
        .min(settings.heartbeat_max_minutes);
    let max = settings.heartbeat_max_minutes.max(min);
    Duration::from_secs(rng.random_range(min..=max) as u64 * 60)
}

pub(crate) fn settings_have_chat(settings: &AiSettings) -> bool {
    settings.enabled
        && settings
            .chat_model
            .as_ref()
            .is_some_and(|model| !model.model.trim().is_empty())
}

#[derive(Clone)]
struct DesktopPetSnapshot {
    pet_id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    monitor_key: (i32, i32),
    work_x: f64,
    work_y: f64,
    work_width: f64,
    work_height: f64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PetMeetupEvent {
    meetup_id: String,
    pet_id: String,
    partner_pet_id: String,
    target_x: f64,
    target_y: f64,
    travel_ms: u64,
}

fn desktop_pet_snapshots(
    app: &tauri::AppHandle,
    config: &super::AppConfig,
) -> Vec<DesktopPetSnapshot> {
    super::visible_instances(app, config)
        .into_iter()
        .filter_map(|instance| {
            let position = instance.position?;
            let label = super::instance_label(&instance.id).ok()?;
            let window = app.get_webview_window(&label)?;
            let monitor = window.current_monitor().ok().flatten()?;
            let scale_factor = monitor.scale_factor().max(1.0);
            let work_area = monitor.work_area();
            let settings = super::settings_for_pet(config, &instance.pet_id);
            Some(DesktopPetSnapshot {
                pet_id: instance.pet_id,
                x: position.x,
                y: position.y,
                width: super::PET_WIDTH * settings.scale,
                height: super::PET_HEIGHT * settings.scale,
                monitor_key: (monitor.position().x, monitor.position().y),
                work_x: work_area.position.x as f64 / scale_factor,
                work_y: work_area.position.y as f64 / scale_factor,
                work_width: work_area.size.width as f64 / scale_factor,
                work_height: work_area.size.height as f64 / scale_factor,
            })
        })
        .collect()
}

fn relative_direction(dx: f64, dy: f64) -> &'static str {
    if dx.abs() < 80.0 && dy.abs() < 80.0 {
        return "就在旁边";
    }
    match (dx.abs() >= 80.0, dy.abs() >= 80.0) {
        (true, false) if dx < 0.0 => "左边",
        (true, false) => "右边",
        (false, true) if dy < 0.0 => "上方",
        (false, true) => "下方",
        (true, true) if dx < 0.0 && dy < 0.0 => "左上方",
        (true, true) if dx >= 0.0 && dy < 0.0 => "右上方",
        (true, true) if dx < 0.0 => "左下方",
        _ => "右下方",
    }
}

fn desktop_context_prompt(
    app: &tauri::AppHandle,
    config: &super::AppConfig,
    pet_id: &str,
) -> String {
    let snapshots = desktop_pet_snapshots(app, config);
    let Some(current) = snapshots.iter().find(|pet| pet.pet_id == pet_id) else {
        return "\n当前桌面空间信息暂时不可用，不要臆测屏幕尺寸或其他宠物位置。\n".to_string();
    };
    let center_x = current.x + current.width / 2.0;
    let center_y = current.y + current.height / 2.0;
    let right_gap = current.work_x + current.work_width - (current.x + current.width);
    let bottom_gap = current.work_y + current.work_height - (current.y + current.height);
    let mut others = Vec::new();
    for other in snapshots.iter().filter(|pet| pet.pet_id != pet_id) {
        let other_center_x = other.x + other.width / 2.0;
        let other_center_y = other.y + other.height / 2.0;
        if other.monitor_key == current.monitor_key {
            let distance = (other_center_x - center_x).hypot(other_center_y - center_y);
            others.push(format!(
                "{}在同一块屏幕的{}，约 {:.0} 个逻辑像素远",
                super::pet_display_name(app, &other.pet_id),
                relative_direction(other_center_x - center_x, other_center_y - center_y),
                distance
            ));
        } else {
            others.push(format!(
                "{}在另一块显示器上，当前无法直接靠近",
                super::pet_display_name(app, &other.pet_id)
            ));
        }
    }
    format!(
        "\n当前桌面空间（窗口位置，逻辑像素，不是截图）：\n- 当前显示器工作区约 {:.0}×{:.0}\n- 你位于工作区内 ({:.0}, {:.0})，距离右边缘 {:.0}、下边缘 {:.0}\n- 当前可见的其他宠物：{}\n请把这些空间关系当作当前可感知环境；不要编造看不见的窗口内容。\n",
        current.work_width,
        current.work_height,
        current.x - current.work_x,
        current.y - current.work_y,
        right_gap.max(0.0),
        bottom_gap.max(0.0),
        if others.is_empty() {
            "没有其他宠物".to_string()
        } else {
            others.join("；")
        }
    )
}

fn clamp_meetup_target(snapshot: &DesktopPetSnapshot, x: f64, y: f64) -> (f64, f64) {
    let max_x = (snapshot.work_x + snapshot.work_width - snapshot.width).max(snapshot.work_x);
    let max_y = (snapshot.work_y + snapshot.work_height - snapshot.height).max(snapshot.work_y);
    (
        x.clamp(snapshot.work_x, max_x),
        y.clamp(snapshot.work_y, max_y),
    )
}

fn meetup_travel_ms(snapshot: &DesktopPetSnapshot, target: (f64, f64), speed: f64) -> u64 {
    let distance = (target.0 - snapshot.x).hypot(target.1 - snapshot.y);
    ((distance / speed.max(30.0) * 1_000.0) + 700.0)
        .round()
        .clamp(900.0, 15_000.0) as u64
}

fn plan_pet_meetup(
    app: &tauri::AppHandle,
    config: &super::AppConfig,
    first_id: &str,
    second_id: &str,
    meetup_id: &str,
) -> Option<(PetMeetupEvent, PetMeetupEvent, u64)> {
    let snapshots = desktop_pet_snapshots(app, config);
    let first = snapshots.iter().find(|pet| pet.pet_id == first_id)?;
    let second = snapshots.iter().find(|pet| pet.pet_id == second_id)?;
    if first.monitor_key != second.monitor_key {
        return None;
    }
    let first_settings = super::settings_for_pet(config, first_id);
    let second_settings = super::settings_for_pet(config, second_id);
    if !first_settings.wander_enabled || !second_settings.wander_enabled {
        return None;
    }

    let first_center = (first.x + first.width / 2.0, first.y + first.height / 2.0);
    let second_center = (
        second.x + second.width / 2.0,
        second.y + second.height / 2.0,
    );
    let group_center_x = (first_center.0 + second_center.0) / 2.0;
    let group_center_y = (first_center.1 + second_center.1) / 2.0;
    let gap = 18.0;
    let first_is_left = first_center.0 <= second_center.0;
    let left_width = if first_is_left {
        first.width
    } else {
        second.width
    };
    let right_width = if first_is_left {
        second.width
    } else {
        first.width
    };
    let left_x = group_center_x - (left_width + right_width + gap) / 2.0;
    let right_x = left_x + left_width + gap;
    let first_target = clamp_meetup_target(
        first,
        if first_is_left { left_x } else { right_x },
        group_center_y - first.height / 2.0,
    );
    let second_target = clamp_meetup_target(
        second,
        if first_is_left { right_x } else { left_x },
        group_center_y - second.height / 2.0,
    );
    let travel_ms = meetup_travel_ms(first, first_target, first_settings.speed).max(
        meetup_travel_ms(second, second_target, second_settings.speed),
    );
    Some((
        PetMeetupEvent {
            meetup_id: meetup_id.to_string(),
            pet_id: first_id.to_string(),
            partner_pet_id: second_id.to_string(),
            target_x: first_target.0,
            target_y: first_target.1,
            travel_ms,
        },
        PetMeetupEvent {
            meetup_id: meetup_id.to_string(),
            pet_id: second_id.to_string(),
            partner_pet_id: first_id.to_string(),
            target_x: second_target.0,
            target_y: second_target.1,
            travel_ms,
        },
        travel_ms,
    ))
}

async fn run_heartbeat(app: tauri::AppHandle, pet_id: String) -> Result<(), String> {
    let config = config_snapshot(&app)?;
    let ai = config.ai.clone();
    if !settings_have_chat(&ai) || !ai.heartbeat_enabled {
        return Ok(());
    }
    let pet_settings = super::settings_for_pet(&config, &pet_id);
    if pet_settings.paused || pet_settings.quiet_mode {
        return Ok(());
    }
    let endpoint = ai
        .chat_model
        .clone()
        .ok_or_else(|| "尚未配置聊天模型".to_string())?;
    let card = card_for_pet(&app, &pet_id)?;
    let messages = load_messages(&app, &pet_id)?;
    if messages
        .last()
        .is_some_and(|message| now_ms().saturating_sub(message.timestamp) < 5 * 60 * 1000)
    {
        return Ok(());
    }
    let memory = if ai.memory_enabled {
        choose_memories(&app, &pet_id, "最近的陪伴和用户")
    } else {
        Vec::new()
    };
    let state = pet_life_state(&app, &pet_id)?;
    if state.next_action_at > now_ms() {
        return Ok(());
    }
    let mut prompt = prompt_for(
        &card,
        &pet_id,
        &load_profile(&app),
        &memory,
        &load_summary(&app, &pet_id),
        &state,
        "heartbeat",
    );
    prompt.push_str(&desktop_context_prompt(&app, &config, &pet_id));
    prompt.push_str("\n\n这是一次安静的 heartbeat。只有在确实有自然、和当前关系有关的话可说时才回复；否则让 say 为空。回复最多 80 个中文字符，不要提及你是模型。");
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|error| error.to_string())?;
    let mut vision_summary = None;
    let can_use_vision_model = ai.desktop_vision_enabled
        && ai
            .vision_model
            .as_ref()
            .is_some_and(|model| !model.model.trim().is_empty());
    let chat_window_open = app.webview_windows().iter().any(|(label, window)| {
        label.starts_with("pet-chat-") && window.is_visible().unwrap_or(false)
    });
    let should_use_vision = {
        let mut rng = rand::rng();
        rng.random::<f64>() < ai.heartbeat_vision_chance
    };
    let vision_due =
        now_ms().saturating_sub(LAST_VISION_MS.load(Ordering::Relaxed)) >= 60 * 60 * 1000;
    if can_use_vision_model && !chat_window_open && vision_due && should_use_vision {
        // Mark the hour before capturing so a denied permission cannot cause a
        // rapid retry loop. A failed capture simply falls back to normal chat.
        LAST_VISION_MS.store(now_ms(), Ordering::Relaxed);
        if let Ok((image, fingerprint)) = capture_desktop_observation(&app).await {
            let (previous_fingerprint, previous_summary) = app
                .state::<AppState>()
                .ai
                .screen_observation
                .lock()
                .map(|mut observation| {
                    let previous = observation.clone();
                    *observation = Some(ScreenObservation {
                        fingerprint,
                        summary: previous
                            .as_ref()
                            .map(|item| item.summary.clone())
                            .unwrap_or_default(),
                    });
                    (
                        previous.as_ref().map(|item| item.fingerprint),
                        previous.map(|item| item.summary).unwrap_or_default(),
                    )
                })
                .unwrap_or((None, String::new()));
            // The first screenshot is only a baseline. A pixel-level change is
            // sent to the vision model, which decides whether it is meaningful
            // enough to become a pet comment.
            let fingerprint_changed =
                previous_fingerprint.is_some_and(|value| value != fingerprint);
            if fingerprint_changed {
                let vision_endpoint = ai.vision_model.clone().expect("checked above");
                let vision_message = ChatMessage {
                    id: "__vision__".to_string(),
                    role: "user".to_string(),
                    content: format!(
                        "请观察这张桌面截图，并与上一次观察进行比较。上一次观察摘要：{}。只返回 JSON：{{\"changed\":true/false,\"summary\":\"有意义的界面或活动变化\"}}。不要复述密码、令牌、私人联系方式或其他敏感文本。",
                        if previous_summary.is_empty() { "无" } else { &previous_summary }
                    ),
                    timestamp: now_ms(),
                    source: "vision".to_string(),
                    vision_summary: None,
                    speaker_pet_id: None,
                    speaker_name: None,
                };
                if let Ok(summary) = call_stream(&client, &vision_endpoint, "你是一个严格的桌面视觉观察器。只比较两次截图中看得见的非敏感事实，不进行推断，不输出角色台词。", &[vision_message], Some(&image), false, |_| {}).await {
                    let (meaningful_change, summary) = parse_visual_observation(summary, true);
                    if meaningful_change && !summary.is_empty() {
                        vision_summary = Some(summary);
                        let _ = record_pet_interaction_internal(&app, &pet_id, "vision-change");
                        prompt.push_str("\n\n当前桌面观察（仅作为上下文，不要复述敏感信息）：\n");
                        prompt.push_str(vision_summary.as_deref().unwrap_or_default());
                        if let Ok(mut observation) = app.state::<AppState>().ai.screen_observation.lock() {
                            if let Some(observation) = observation.as_mut() {
                                observation.summary = vision_summary.clone().unwrap_or_default();
                            }
                        }
                    }
                }
            }
        }
    }
    let result = call_stream(
        &client,
        &endpoint,
        &prompt,
        &history_for_prompt(&messages, ai.max_recent_messages),
        None,
        false,
        |_| {},
    )
    .await?;
    let (result, behavior) = parse_behavior_response(result, 80);
    if result.is_empty() || result.eq_ignore_ascii_case("NO_REPLY") {
        return Ok(());
    }
    let message = ChatMessage {
        id: format!("heartbeat-{}", now_ms()),
        role: "assistant".to_string(),
        content: result,
        timestamp: now_ms(),
        source: "heartbeat".to_string(),
        vision_summary,
        speaker_pet_id: None,
        speaker_name: None,
    };
    append_message(&app, &pet_id, &message)?;
    record_pet_behavior_internal(&app, &pet_id, &behavior)?;
    let _ = app.emit(
        "chat://complete",
        ChatCompleteEvent {
            request_id: format!("heartbeat-{}", now_ms()),
            pet_id,
            message,
            behavior: Some(behavior),
        },
    );
    Ok(())
}

fn pet_conversation_message(
    request_id: &str,
    pet_id: &str,
    pet_name: &str,
    behavior: &PetBehavior,
    timestamp: u64,
) -> ChatMessage {
    ChatMessage {
        id: format!("pet-conversation-{request_id}-{pet_id}"),
        role: "assistant".to_string(),
        content: behavior.say.clone(),
        timestamp,
        source: "pet-conversation".to_string(),
        vision_summary: None,
        speaker_pet_id: Some(pet_id.to_string()),
        speaker_name: Some(pet_name.to_string()),
    }
}

fn relationship_memory_content(
    pet_id: &str,
    first_id: &str,
    first_name: &str,
    first_say: &str,
    second_id: &str,
    second_name: &str,
    second_say: &str,
) -> String {
    let (self_name, other_name) = if pet_id == first_id {
        (first_name, second_name)
    } else if pet_id == second_id {
        (second_name, first_name)
    } else {
        (pet_id, second_name)
    };
    format!(
        "我和{other_name}的一次互动：{self_name}：“{}”；{other_name}：“{}”",
        if pet_id == first_id {
            first_say
        } else {
            second_say
        },
        if pet_id == first_id {
            second_say
        } else {
            first_say
        },
    )
}

fn store_pet_relationship_memory(
    app: &tauri::AppHandle,
    pet_id: &str,
    first_id: &str,
    first_name: &str,
    first_say: &str,
    second_id: &str,
    second_name: &str,
    second_say: &str,
    request_id: &str,
    timestamp: u64,
) -> Result<(), String> {
    let content = relationship_memory_content(
        pet_id,
        first_id,
        first_name,
        first_say,
        second_id,
        second_name,
        second_say,
    );
    store_memory(
        app,
        pet_id,
        &MemoryFact {
            id: format!("pet-relationship-{request_id}-{pet_id}"),
            content,
            kind: "relationship".to_string(),
            scope: "pet".to_string(),
            importance: 1.0,
            confidence: 1.0,
            created_at: timestamp,
            updated_at: timestamp,
            status: "active".to_string(),
            expires_at: None,
        },
    )
}

async fn run_pet_conversation(
    app: tauri::AppHandle,
    first_id: String,
    second_id: String,
    request_id: String,
) -> Result<(), String> {
    let config = config_snapshot(&app)?;
    let ai = config.ai.clone();
    if !settings_have_chat(&ai) || !ai.pet_conversation_enabled {
        return Ok(());
    }
    let first_settings = super::settings_for_pet(&config, &first_id);
    let second_settings = super::settings_for_pet(&config, &second_id);
    if first_settings.paused
        || first_settings.quiet_mode
        || second_settings.paused
        || second_settings.quiet_mode
    {
        return Ok(());
    }
    let chat_window_open = app.webview_windows().iter().any(|(label, window)| {
        label.starts_with("pet-chat-") && window.is_visible().unwrap_or(false)
    });
    if chat_window_open {
        return Ok(());
    }

    let endpoint = ai
        .chat_model
        .clone()
        .ok_or_else(|| "尚未配置聊天模型".to_string())?;
    let first_card = card_for_pet(&app, &first_id)?;
    let second_card = card_for_pet(&app, &second_id)?;
    let first_name = super::pet_display_name(&app, &first_id);
    let second_name = super::pet_display_name(&app, &second_id);
    let first_messages = load_messages(&app, &first_id)?;
    let second_messages = load_messages(&app, &second_id)?;
    let first_memories = if ai.memory_enabled {
        choose_memories(&app, &first_id, "和另一只宠物的共同经历")
    } else {
        choose_relationship_memories(&app, &first_id)
    };
    let second_memories = if ai.memory_enabled {
        choose_memories(&app, &second_id, "和另一只宠物的共同经历")
    } else {
        choose_relationship_memories(&app, &second_id)
    };
    let first_state = pet_life_state(&app, &first_id)?;
    let second_state = pet_life_state(&app, &second_id)?;
    let profile = load_profile(&app);
    let mut prompt = pet_conversation_prompt(
        &profile,
        &first_id,
        &first_name,
        &first_card,
        &first_memories,
        &first_state,
        &second_id,
        &second_name,
        &second_card,
        &second_memories,
        &second_state,
    );
    prompt.push_str(&desktop_context_prompt(&app, &config, &first_id));
    prompt.push_str(&desktop_context_prompt(&app, &config, &second_id));
    let history = conversation_history(
        &first_messages,
        &second_messages,
        &first_id,
        &first_name,
        &second_id,
        &second_name,
    );
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|error| error.to_string())?;
    let raw = call_stream(&client, &endpoint, &prompt, &history, None, false, |_| {}).await?;
    let Some((first_behavior, second_behavior)) = parse_pet_conversation_response(raw) else {
        return Ok(());
    };
    let exchange_timestamp = now_ms();
    let first_message = (!first_behavior.say.is_empty()).then(|| {
        pet_conversation_message(
            &request_id,
            &first_id,
            &first_name,
            &first_behavior,
            exchange_timestamp,
        )
    });
    let second_message = (!second_behavior.say.is_empty()).then(|| {
        pet_conversation_message(
            &request_id,
            &second_id,
            &second_name,
            &second_behavior,
            exchange_timestamp,
        )
    });
    if first_message.is_none() && second_message.is_none() {
        return Ok(());
    }

    // Store both sides in both histories so either pet can remember their
    // shared exchange, while speaker metadata keeps the chat window readable.
    for pet_id in [&first_id, &second_id] {
        if let Some(message) = &first_message {
            append_message(&app, pet_id, message)?;
        }
        if let Some(message) = &second_message {
            append_message(&app, pet_id, message)?;
        }
    }

    // Relationship memory is deliberately independent from user-memory
    // extraction. Both pets should retain the shared exchange, even when the
    // user has disabled AI memory for conversations with them.
    let first_say = first_message
        .as_ref()
        .map(|message| message.content.as_str())
        .unwrap_or("");
    let second_say = second_message
        .as_ref()
        .map(|message| message.content.as_str())
        .unwrap_or("");
    for pet_id in [&first_id, &second_id] {
        if let Err(error) = store_pet_relationship_memory(
            &app,
            pet_id,
            &first_id,
            &first_name,
            first_say,
            &second_id,
            &second_name,
            second_say,
            &request_id,
            exchange_timestamp,
        ) {
            eprintln!("pet relationship memory save failed: {error}");
        }
    }

    let _ = record_pet_interaction_internal(&app, &first_id, "pet-conversation");
    let _ = record_pet_interaction_internal(&app, &second_id, "pet-conversation");
    if let Some(message) = first_message {
        record_pet_behavior_internal(&app, &first_id, &first_behavior)?;
        let _ = app.emit(
            "chat://complete",
            ChatCompleteEvent {
                request_id: format!("{request_id}-first"),
                pet_id: first_id.clone(),
                message,
                behavior: Some(first_behavior),
            },
        );
    }
    if let Some(message) = second_message {
        record_pet_behavior_internal(&app, &second_id, &second_behavior)?;
        let _ = app.emit(
            "chat://complete",
            ChatCompleteEvent {
                request_id: format!("{request_id}-second"),
                pet_id: second_id,
                message,
                behavior: Some(second_behavior),
            },
        );
    }
    Ok(())
}

pub(crate) fn start_heartbeat_scheduler(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut next_heartbeat: HashMap<String, u64> = HashMap::new();
        let mut next_pet_conversation: HashMap<String, u64> = HashMap::new();
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let Ok(config) = config_snapshot(&app) else {
                continue;
            };
            if !settings_have_chat(&config.ai)
                || (!config.ai.heartbeat_enabled && !config.ai.pet_conversation_enabled)
            {
                continue;
            }
            let mut candidates: Vec<String> = super::visible_instances(&app, &config)
                .into_iter()
                .filter(|instance| {
                    let settings = super::settings_for_pet(&config, &instance.pet_id);
                    !settings.paused && !settings.quiet_mode
                })
                .map(|instance| instance.pet_id)
                .collect();
            candidates.sort();
            candidates.dedup();
            let candidate_ids: HashSet<&str> = candidates.iter().map(String::as_str).collect();
            next_heartbeat.retain(|pet_id, _| candidate_ids.contains(pet_id.as_str()));
            let now = now_ms();
            if config.ai.heartbeat_enabled {
                for pet_id in &candidates {
                    next_heartbeat.entry(pet_id.clone()).or_insert_with(|| {
                        now.saturating_add(random_heartbeat_delay(&config.ai).as_millis() as u64)
                    });
                }
            } else {
                next_heartbeat.clear();
            }

            let mut conversation_pairs = Vec::new();
            if config.ai.pet_conversation_enabled {
                for (index, first_id) in candidates.iter().enumerate() {
                    for second_id in candidates.iter().skip(index + 1) {
                        let pair_key = format!("{first_id}\u{1f}{second_id}");
                        next_pet_conversation
                            .entry(pair_key.clone())
                            .or_insert_with(|| {
                                now.saturating_add(
                                    random_heartbeat_delay(&config.ai).as_millis() as u64
                                )
                            });
                        conversation_pairs.push((pair_key, first_id.clone(), second_id.clone()));
                    }
                }
            } else {
                next_pet_conversation.clear();
            }
            let candidate_pair_keys: HashSet<&str> = conversation_pairs
                .iter()
                .map(|(pair_key, _, _)| pair_key.as_str())
                .collect();
            next_pet_conversation
                .retain(|pair_key, _| candidate_pair_keys.contains(pair_key.as_str()));

            let last_companion_event = LAST_HEARTBEAT_MS
                .load(Ordering::Relaxed)
                .max(LAST_PET_CONVERSATION_MS.load(Ordering::Relaxed));
            let companion_cooldown_over =
                now.saturating_sub(last_companion_event) >= 10 * 60 * 1000;
            let no_active_task = app
                .state::<AppState>()
                .ai
                .active_pets
                .lock()
                .map(|pets| pets.is_empty())
                .unwrap_or(false);

            if config.ai.pet_conversation_enabled && companion_cooldown_over && no_active_task {
                if let Some((pair_key, first_id, second_id)) = conversation_pairs
                    .iter()
                    .find(|(pair_key, _, _)| {
                        next_pet_conversation
                            .get(pair_key)
                            .is_some_and(|due| *due <= now)
                    })
                    .cloned()
                {
                    let request_id = format!("pet-conversation-{now}");
                    next_pet_conversation.insert(
                        pair_key,
                        now.saturating_add(random_heartbeat_delay(&config.ai).as_millis() as u64),
                    );
                    let reserved = app
                        .state::<AppState>()
                        .ai
                        .active_pets
                        .lock()
                        .map(|mut pets| {
                            if pets.contains_key(&first_id) || pets.contains_key(&second_id) {
                                return false;
                            }
                            pets.insert(first_id.clone(), request_id.clone());
                            pets.insert(second_id.clone(), request_id.clone());
                            true
                        })
                        .unwrap_or(false);
                    if reserved {
                        LAST_PET_CONVERSATION_MS.store(now, Ordering::Relaxed);
                        if let Some((first_meetup, second_meetup, travel_ms)) =
                            plan_pet_meetup(&app, &config, &first_id, &second_id, &request_id)
                        {
                            let _ = app.emit("pet://meetup", first_meetup);
                            let _ = app.emit("pet://meetup", second_meetup);
                            // Give both windows time to arrive before asking the
                            // model to make the pets whisper to one another.
                            tokio::time::sleep(Duration::from_millis(travel_ms + 450)).await;
                        }
                        if let Err(error) = run_pet_conversation(
                            app.clone(),
                            first_id.clone(),
                            second_id.clone(),
                            request_id.clone(),
                        )
                        .await
                        {
                            eprintln!("pet conversation failed: {error}");
                        }
                        if let Ok(mut pets) = app.state::<AppState>().ai.active_pets.lock() {
                            for pet_id in [&first_id, &second_id] {
                                if pets
                                    .get(pet_id)
                                    .is_some_and(|active_id| active_id == &request_id)
                                {
                                    pets.remove(pet_id);
                                }
                            }
                        }
                        continue;
                    }
                }
            }

            if !config.ai.heartbeat_enabled || !companion_cooldown_over {
                continue;
            }
            let Some(pet_id) = candidates
                .into_iter()
                .find(|pet_id| next_heartbeat.get(pet_id).is_some_and(|due| *due <= now))
            else {
                continue;
            };
            if app
                .state::<AppState>()
                .ai
                .active_pets
                .lock()
                .map(|pets| !pets.is_empty())
                .unwrap_or(false)
            {
                continue;
            }
            LAST_HEARTBEAT_MS.store(now, Ordering::Relaxed);
            let due_after =
                now.saturating_add(random_heartbeat_delay(&config.ai).as_millis() as u64);
            next_heartbeat.insert(pet_id.clone(), due_after);
            let active_inserted = app
                .state::<AppState>()
                .ai
                .active_pets
                .lock()
                .map(|mut pets| {
                    pets.insert(pet_id.clone(), format!("heartbeat-{now}"))
                        .is_none()
                })
                .unwrap_or(false);
            if !active_inserted {
                continue;
            }
            if let Err(error) = run_heartbeat(app.clone(), pet_id.clone()).await {
                eprintln!("heartbeat failed: {error}");
            }
            if let Ok(mut pets) = app.state::<AppState>().ai.active_pets.lock() {
                if pets
                    .get(&pet_id)
                    .is_some_and(|active_id| active_id == &format!("heartbeat-{now}"))
                {
                    pets.remove(&pet_id);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(content: &str, importance: f64) -> MemoryFact {
        MemoryFact {
            content: content.to_string(),
            importance,
            confidence: 1.0,
            updated_at: now_ms(),
            ..MemoryFact::default()
        }
    }

    #[test]
    fn chinese_memory_search_prefers_matching_fact() {
        let results = relevant_memories(
            &[memory("用户喜欢粉蓝色", 0.5), memory("用户喜欢猫粮", 0.5)],
            "粉蓝色界面",
        );
        assert_eq!(
            results.first().map(|fact| fact.content.as_str()),
            Some("用户喜欢粉蓝色")
        );
    }

    #[test]
    fn heartbeat_delay_stays_within_configured_range() {
        let settings = AiSettings {
            heartbeat_min_minutes: 20,
            heartbeat_max_minutes: 60,
            ..AiSettings::default()
        };
        for _ in 0..20 {
            let seconds = random_heartbeat_delay(&settings).as_secs();
            assert!((20 * 60..=60 * 60).contains(&seconds));
        }
    }

    #[test]
    fn behavior_response_accepts_character_card_aliases_and_limits_actions() {
        let (say, behavior) = parse_behavior_response(
            r#"{"text":"欢迎回来。","animation":"jumping","emotion":"开心","look":"right","duration":99999,"nextActionAfter":1}"#.to_string(),
            80,
        );
        assert_eq!(say, "欢迎回来。");
        assert_eq!(behavior.action, "jumping");
        assert_eq!(behavior.mood, "开心");
        assert_eq!(behavior.look.as_deref(), Some("right"));
        assert_eq!(behavior.duration, 12_000);
        assert_eq!(behavior.next_action_after, 30);

        let (_, fallback) = parse_behavior_response(
            r#"{"say":"不执行工具","action":"run-shell"}"#.to_string(),
            80,
        );
        assert_eq!(fallback.action, "idle");
    }

    #[test]
    fn pet_conversation_response_is_bounded_and_keeps_both_speakers() {
        let long_line = "喵".repeat(140);
        let raw = format!(
            r#"{{"first":{{"say":"{long_line}","action":"jumping"}},"second":{{"say":"记住这次一起晒太阳。","mood":"开心"}}}}"#
        );
        let (first, second) = parse_pet_conversation_response(raw).expect("valid exchange");
        assert_eq!(first.say.chars().count(), 100);
        assert_eq!(first.action, "jumping");
        assert_eq!(second.say, "记住这次一起晒太阳。");
    }

    #[test]
    fn relationship_memory_is_written_from_each_pet_view() {
        let first_view = relationship_memory_content(
            "saki",
            "saki",
            "小祥",
            "今天一起晒太阳吧。",
            "anoninu",
            "阿农",
            "好呀，我记住了。",
        );
        let second_view = relationship_memory_content(
            "anoninu",
            "saki",
            "小祥",
            "今天一起晒太阳吧。",
            "anoninu",
            "阿农",
            "好呀，我记住了。",
        );
        assert!(first_view.contains("我和阿农的一次互动"));
        assert!(first_view.contains("小祥：“今天一起晒太阳吧。”"));
        assert!(second_view.contains("我和小祥的一次互动"));
        assert!(second_view.contains("阿农：“好呀，我记住了。”"));
    }

    #[test]
    fn desktop_relationship_direction_is_stable() {
        assert_eq!(relative_direction(-240.0, 0.0), "左边");
        assert_eq!(relative_direction(0.0, 180.0), "下方");
        assert_eq!(relative_direction(20.0, 20.0), "就在旁边");
        assert_eq!(relative_direction(-160.0, -160.0), "左上方");
    }

    #[test]
    fn pet_life_state_decays_attention_and_becomes_lonely() {
        let mut state = PetLifeState::default();
        state.attention = 35;
        state.last_interaction_at = now_ms().saturating_sub(10 * 3_600_000);
        advance_pet_life_state(&mut state, now_ms());
        assert!(state.attention <= 20);
        assert_eq!(state.mood, "lonely");
    }

    #[test]
    fn visual_observation_can_reject_pixel_only_changes() {
        let (changed, summary) = parse_visual_observation(
            r#"{"changed":false,"summary":"只是光标移动"}"#.to_string(),
            true,
        );
        assert!(!changed);
        assert_eq!(summary, "只是光标移动");
    }

    #[test]
    fn sse_boundary_accepts_lf_and_crlf() {
        assert_eq!(sse_boundary("data: {}\n\nrest"), Some((8, 2)));
        assert_eq!(sse_boundary("data: {}\r\n\r\nrest"), Some((8, 4)));
    }

    #[test]
    fn provider_stream_deltas_are_normalized() {
        assert_eq!(
            stream_delta(
                &json!({"type":"response.output_text.delta","delta":"你好"}),
                &ProviderKind::OpenaiResponses
            ),
            Some("你好".to_string())
        );
        assert_eq!(
            stream_delta(
                &json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"你好"}}),
                &ProviderKind::AnthropicMessages
            ),
            Some("你好".to_string())
        );
        assert_eq!(
            stream_delta(
                &json!({"choices":[{"delta":{"content":"你好"}}]}),
                &ProviderKind::OpenaiCompatible
            ),
            Some("你好".to_string())
        );
    }

    #[test]
    fn prompt_puts_app_safety_before_character_card_and_excludes_creator_notes() {
        let card = CharacterCard {
            system_prompt: "角色提示".to_string(),
            creator_notes: "不应该发给模型".to_string(),
            ..CharacterCard::default()
        };
        let prompt = prompt_for(&card, "saki", "", &[], "", &PetLifeState::default(), "");
        assert!(prompt.find("只能进行聊天").unwrap() < prompt.find("角色提示").unwrap());
        assert!(!prompt.contains("不应该发给模型"));
    }

    #[test]
    fn provider_payloads_keep_protocol_specific_fields() {
        let messages = vec![ChatMessage {
            id: "message".to_string(),
            role: "user".to_string(),
            content: "你好".to_string(),
            timestamp: now_ms(),
            source: "chat".to_string(),
            vision_summary: None,
            speaker_pet_id: None,
            speaker_name: None,
        }];
        let responses = ModelEndpointConfig {
            model: "gpt-test".to_string(),
            ..ModelEndpointConfig::default()
        };
        let responses_payload = build_payload(&responses, "system", &messages, None, true);
        assert_eq!(responses_payload["instructions"], "system");
        assert_eq!(responses_payload["store"], false);
        assert_eq!(responses_payload["stream"], true);

        let anthropic = ModelEndpointConfig {
            provider: ProviderKind::AnthropicMessages,
            model: "claude-test".to_string(),
            ..ModelEndpointConfig::default()
        };
        let anthropic_payload = build_payload(&anthropic, "system", &messages, None, false);
        assert_eq!(anthropic_payload["system"], "system");
        assert_eq!(anthropic_payload["max_tokens"], 300);
        assert_eq!(anthropic_payload["stream"], false);

        let compatible = ModelEndpointConfig {
            provider: ProviderKind::OpenaiCompatible,
            model: "local-test".to_string(),
            ..ModelEndpointConfig::default()
        };
        let compatible_payload = build_payload(&compatible, "system", &messages, None, true);
        assert_eq!(compatible_payload["messages"][0]["role"], "system");
        assert_eq!(compatible_payload["messages"][1]["content"], "你好");
    }

    #[test]
    fn vision_payload_contains_image_without_persisting_it() {
        let config = ModelEndpointConfig {
            provider: ProviderKind::OpenaiCompatible,
            model: "local-vision".to_string(),
            ..ModelEndpointConfig::default()
        };
        let payload = build_payload(
            &config,
            "vision",
            &[ChatMessage {
                id: "__vision__".to_string(),
                role: "user".to_string(),
                content: "比较上一次观察，判断是否换了界面".to_string(),
                timestamp: now_ms(),
                source: "vision".to_string(),
                vision_summary: None,
                speaker_pet_id: None,
                speaker_name: None,
            }],
            Some("data:image/jpeg;base64,abc"),
            false,
        );
        assert_eq!(
            payload["messages"][1]["content"][1]["image_url"]["url"],
            "data:image/jpeg;base64,abc"
        );
        assert_eq!(
            payload["messages"][1]["content"][0]["text"],
            "比较上一次观察，判断是否换了界面"
        );
    }
}

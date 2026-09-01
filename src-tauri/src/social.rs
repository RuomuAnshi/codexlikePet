//! Local-first multi-pet social runtime.
//!
//! A scene is a small, cancellable choreography. Rust reserves the
//! participating windows and moves them through safe waypoints; pet webviews
//! only render the phase and speech bubble.

use chrono::Local;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, LogicalPosition, Manager, WebviewUrl, WebviewWindowBuilder};

use super::{config_snapshot, is_safe_id, AppConfig, AppState, CharacterCard, PetPosition};

const SOCIAL_DIRECTORY: &str = "social";
const SOCIAL_EVENT_MAX_CHARS: usize = 240;
const PROXIMITY_DISTANCE: f64 = 260.0;
const SCENE_TICK_MS: u64 = 40;
const SCENE_BUSY_ERROR: &str = "社交舞台正在使用中，请稍后再试";
static SCENE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct SocialSettings {
    pub enabled: bool,
    pub min_interval_minutes: u32,
    pub max_interval_minutes: u32,
    pub proximity_enabled: bool,
    pub manual_enabled: bool,
    pub max_participants: u8,
    pub props_enabled: bool,
}

impl Default for SocialSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            min_interval_minutes: 3,
            max_interval_minutes: 10,
            proximity_enabled: true,
            manual_enabled: true,
            max_participants: 4,
            props_enabled: true,
        }
    }
}

#[derive(Default)]
pub(crate) struct SocialRuntime {
    pub(crate) active: Mutex<HashMap<String, ActiveScene>>,
    pub(crate) runtime: Mutex<HashMap<String, RuntimePetState>>,
    pub(crate) next_scheduled_at: Mutex<u64>,
}

#[derive(Clone)]
pub(crate) struct ActiveScene {
    pub(crate) participants: Vec<String>,
    stage: Rect,
    pub(crate) cancel: Arc<AtomicBool>,
}

#[allow(dead_code)]
#[derive(Clone, Default)]
pub(crate) struct RuntimePetState {
    pub(crate) instance_id: String,
    pub(crate) pet_id: String,
    pub(crate) position: Option<PetPosition>,
    pub(crate) dragging: bool,
    pub(crate) busy: bool,
}

#[derive(Clone, Copy, Default)]
struct Rect {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

impl Rect {
    fn from_points(points: impl Iterator<Item = (f64, f64)>) -> Option<Self> {
        let points: Vec<(f64, f64)> = points.collect();
        let &(first_x, first_y) = points.first()?;
        let mut rect = Self {
            left: first_x,
            top: first_y,
            right: first_x,
            bottom: first_y,
        };
        for (x, y) in points.into_iter().skip(1) {
            rect.left = rect.left.min(x);
            rect.top = rect.top.min(y);
            rect.right = rect.right.max(x);
            rect.bottom = rect.bottom.max(y);
        }
        Some(rect)
    }

    fn padded(self, amount: f64) -> Self {
        Self {
            left: self.left - amount,
            top: self.top - amount,
            right: self.right + amount,
            bottom: self.bottom + amount,
        }
    }

    fn overlaps(self, other: Self) -> bool {
        self.left < other.right
            && self.right > other.left
            && self.top < other.bottom
            && self.bottom > other.top
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct SocialTraits {
    pub sociability: f64,
    pub initiative: f64,
    pub playfulness: f64,
    pub competitiveness: f64,
}

impl Default for SocialTraits {
    fn default() -> Self {
        Self {
            sociability: 0.5,
            initiative: 0.5,
            playfulness: 0.5,
            competitiveness: 0.5,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct SocialRelationshipCard {
    pub initial_disposition: String,
    pub prompt: String,
    pub romance_allowed: bool,
    pub interaction_weights: HashMap<String, f64>,
    pub dialogue: HashMap<String, Vec<String>>,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct SocialCharacterConfig {
    pub version: u8,
    pub traits: SocialTraits,
    pub interaction_weights: HashMap<String, f64>,
    pub dialogue: HashMap<String, Vec<String>>,
    pub relationships: HashMap<String, SocialRelationshipCard>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", default)]
struct DirectionalFeelings {
    trust: i16,
    fondness: i16,
    attachment: i16,
    attraction: i16,
    rivalry: i16,
    jealousy: i16,
    resentment: i16,
}

impl Default for DirectionalFeelings {
    fn default() -> Self {
        Self {
            trust: 0,
            fondness: 0,
            attachment: 0,
            attraction: 0,
            rivalry: 0,
            jealousy: 0,
            resentment: 0,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", default)]
struct SocialRelationshipFile {
    pair_id: String,
    first_pet_id: String,
    second_pet_id: String,
    affinity: u8,
    peak_affinity: u8,
    level: u8,
    known_since: u64,
    interaction_count: u64,
    last_interaction_at: u64,
    last_advanced_at: u64,
    unlocked_milestones: Vec<String>,
    romance_status: String,
    directional: HashMap<String, DirectionalFeelings>,
}

impl Default for SocialRelationshipFile {
    fn default() -> Self {
        let now = now_ms();
        Self {
            pair_id: String::new(),
            first_pet_id: String::new(),
            second_pet_id: String::new(),
            affinity: 0,
            peak_affinity: 0,
            level: 1,
            known_since: now,
            interaction_count: 0,
            last_interaction_at: 0,
            last_advanced_at: now,
            unlocked_milestones: Vec::new(),
            romance_status: "none".to_string(),
            directional: HashMap::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PublicRelationship {
    pub pair_id: String,
    pub first_pet_id: String,
    pub second_pet_id: String,
    pub affinity: u8,
    pub peak_affinity: u8,
    pub level: u8,
    pub interaction_count: u64,
    pub last_interaction_at: u64,
    pub romance_status: String,
    pub unlocked_milestones: Vec<String>,
}

impl From<&SocialRelationshipFile> for PublicRelationship {
    fn from(value: &SocialRelationshipFile) -> Self {
        Self {
            pair_id: value.pair_id.clone(),
            first_pet_id: value.first_pet_id.clone(),
            second_pet_id: value.second_pet_id.clone(),
            affinity: value.affinity,
            peak_affinity: value.peak_affinity,
            level: value.level,
            interaction_count: value.interaction_count,
            last_interaction_at: value.last_interaction_at,
            romance_status: value.romance_status.clone(),
            unlocked_milestones: value.unlocked_milestones.clone(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SocialLogEntry {
    pub id: String,
    pub timestamp: u64,
    pub participants: Vec<String>,
    pub interaction_type: String,
    pub trigger: String,
    pub dialogue: Vec<SocialLogDialogue>,
    pub milestones: Vec<String>,
    pub outcome: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SocialLogDialogue {
    pub pet_id: String,
    pub text: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SceneActor {
    instance_id: String,
    pet_id: String,
    role: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SceneStartEvent {
    scene_id: String,
    scene: String,
    trigger: String,
    participants: Vec<SceneActor>,
    prop: Option<String>,
    duration_ms: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenePhaseEvent {
    scene_id: String,
    phase: String,
    participants: Vec<ScenePhaseActor>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenePhaseActor {
    instance_id: String,
    pet_id: String,
    animation: String,
    look: Option<String>,
    say: Option<String>,
    effect: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SceneEndEvent {
    scene_id: String,
    scene: String,
    cancelled: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SocialSceneSummary {
    pub scene_id: String,
    pub scene: String,
    pub trigger: String,
    pub participants: Vec<String>,
    pub queued: bool,
}

#[derive(Clone)]
struct Snapshot {
    instance_id: String,
    pet_id: String,
    position: PetPosition,
    monitor_key: String,
    bounds: Rect,
}

#[derive(Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct ModelSceneDecision {
    scene: String,
    participants: Vec<ModelSceneActor>,
    prop: Option<String>,
    relationship_signals: Vec<ModelRelationshipSignal>,
}

#[derive(Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct ModelSceneActor {
    pet_id: String,
    role: String,
    say: String,
}

#[derive(Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct ModelRelationshipSignal {
    from: String,
    to: String,
    change: String,
}

#[derive(Clone)]
struct PlannedActor {
    snapshot: Snapshot,
    role: String,
    say: String,
    target: PetPosition,
}

#[derive(Clone)]
struct ScenePlan {
    scene_id: String,
    scene: String,
    trigger: String,
    prop: Option<String>,
    actors: Vec<PlannedActor>,
    stage: Rect,
    duration_ms: u64,
    relationship_signals: Vec<ModelRelationshipSignal>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn next_scene_id() -> String {
    format!(
        "social-{}-{}",
        now_ms(),
        SCENE_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn pair_key(first: &str, second: &str) -> Result<(String, String, String), String> {
    if !is_safe_id(first) || !is_safe_id(second) || first == second {
        return Err("无效的宠物组合".to_string());
    }
    let (first, second) = if first < second {
        (first.to_string(), second.to_string())
    } else {
        (second.to_string(), first.to_string())
    };
    Ok((
        format!("{first}--{second}"),
        first.clone(),
        second.clone(),
    ))
}

fn social_root(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join(SOCIAL_DIRECTORY))
        .map_err(|error| format!("无法定位宠物社交数据目录: {error}"))
}

fn relationship_path(
    app: &tauri::AppHandle,
    first: &str,
    second: &str,
) -> Result<std::path::PathBuf, String> {
    let (key, _, _) = pair_key(first, second)?;
    Ok(social_root(app)?
        .join("relationships")
        .join(format!("{key}.json")))
}

fn migrate_legacy_relationship(
    app: &tauri::AppHandle,
    first: &str,
    second: &str,
) -> Option<SocialRelationshipFile> {
    let (_, first, second) = pair_key(first, second).ok()?;
    let old_path = app
        .path()
        .app_data_dir()
        .ok()?
        .join("ai")
        .join("relationships")
        .join(format!("{first}\u{1f}{second}.json"));
    let old = fs::read(old_path).ok()?;
    let value = serde_json::from_slice::<Value>(&old).ok()?;
    let mut file = SocialRelationshipFile::default();
    file.pair_id = format!("{first}--{second}");
    file.first_pet_id = first;
    file.second_pet_id = second;
    file.affinity = value.get("affinity")?.as_u64()?.min(100) as u8;
    file.peak_affinity = value
        .get("peakAffinity")
        .and_then(Value::as_u64)
        .unwrap_or(u64::from(file.affinity))
        .min(100) as u8;
    file.level = relationship_level(file.affinity);
    file.known_since = value
        .get("knownSince")
        .and_then(Value::as_u64)
        .unwrap_or(file.known_since);
    file.interaction_count = value
        .get("interactionCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    file.last_interaction_at = value
        .get("lastInteractionAt")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    file.last_advanced_at = value
        .get("lastAdvancedAt")
        .and_then(Value::as_u64)
        .unwrap_or(now_ms());
    file.unlocked_milestones = value
        .get("unlockedMilestones")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let _ = save_relationship(app, &file);
    Some(file)
}

fn load_relationship(
    app: &tauri::AppHandle,
    first: &str,
    second: &str,
) -> Result<SocialRelationshipFile, String> {
    let (key, first, second) = pair_key(first, second)?;
    let path = relationship_path(app, &first, &second)?;
    let mut file = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SocialRelationshipFile>(&bytes).ok())
        .or_else(|| migrate_legacy_relationship(app, &first, &second))
        .unwrap_or_default();
    file.pair_id = key;
    file.first_pet_id = first;
    file.second_pet_id = second;
    if file.known_since == 0 {
        file.known_since = now_ms();
    }
    if file.last_advanced_at == 0 {
        file.last_advanced_at = now_ms();
    }
    file.level = relationship_level(file.affinity);
    Ok(file)
}

fn save_relationship(
    app: &tauri::AppHandle,
    relationship: &SocialRelationshipFile,
) -> Result<(), String> {
    let path = relationship_path(
        app,
        &relationship.first_pet_id,
        &relationship.second_pet_id,
    )?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建关系目录: {error}"))?;
    }
    let temp = path.with_extension("json.tmp");
    fs::write(
        &temp,
        serde_json::to_vec_pretty(relationship).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("无法保存关系: {error}"))?;
    #[cfg(target_os = "windows")]
    if path.exists() {
        fs::remove_file(&path).map_err(|error| error.to_string())?;
    }
    fs::rename(temp, path).map_err(|error| format!("无法替换关系文件: {error}"))
}

fn relationship_level(value: u8) -> u8 {
    match value {
        0..=19 => 1,
        20..=44 => 2,
        45..=69 => 3,
        70..=89 => 4,
        _ => 5,
    }
}

fn append_log(app: &tauri::AppHandle, entry: &SocialLogEntry) -> Result<(), String> {
    let root = social_root(app)?.join("events");
    fs::create_dir_all(&root).map_err(|error| format!("无法创建社交日志目录: {error}"))?;
    let path = root.join(format!("{}.jsonl", Local::now().format("%Y-%m")));
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("无法打开社交日志: {error}"))?;
    let line = serde_json::to_string(entry).map_err(|error| error.to_string())?;
    writeln!(file, "{line}").map_err(|error| format!("无法写入社交日志: {error}"))?;
    let _ = app.emit("social://log-appended", entry);
    Ok(())
}

fn read_logs(app: &tauri::AppHandle) -> Result<Vec<SocialLogEntry>, String> {
    let root = social_root(app)?.join("events");
    let Ok(entries) = fs::read_dir(root) else {
        return Ok(Vec::new());
    };
    let mut logs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        for line in content.lines() {
            if let Ok(value) = serde_json::from_str::<SocialLogEntry>(line) {
                logs.push(value);
            }
        }
    }
    logs.sort_by_key(|entry| entry.timestamp);
    Ok(logs)
}

fn card_social(card: &CharacterCard) -> SocialCharacterConfig {
    card.extensions
        .get("sakipet")
        .and_then(|value| value.get("social"))
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

fn scene_options(participant_count: usize) -> &'static [&'static str] {
    if participant_count > 2 {
        &[
            "parade",
            "chain-chase",
            "circle-chat",
            "group-pile",
            "group-cheer",
            "group-nap",
            "toy-scramble",
        ]
    } else {
        &[
            "greet",
            "whisper",
            "nuzzle",
            "chase",
            "tag",
            "follow",
            "sync-jump",
            "stack",
            "bump",
            "prank",
            "tug",
            "steal",
            "share-snack",
            "comfort",
            "reconcile",
            "group-nap",
        ]
    }
}

fn valid_scene(value: &str, count: usize) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    scene_options(count)
        .iter()
        .find(|item| **item == value)
        .map(|item| (*item).to_string())
}

fn valid_prop(value: Option<&str>) -> Option<String> {
    match value.unwrap_or_default().trim().to_ascii_lowercase().as_str() {
        "ball" | "ribbon" | "snack" | "toy" => {
            Some(value.unwrap().trim().to_ascii_lowercase())
        }
        _ => None,
    }
}

fn generic_line(scene: &str, role: &str, index: usize) -> String {
    let lines: &[&str] = match (scene, role) {
        ("greet", "leader") => &["你也在这里呀。", "要一起待一会儿吗？"],
        ("greet", _) => &["嗯，来打个招呼。", "我看到你啦。"],
        ("whisper", "leader") => &["过来一点，小声说。", "这件事不要告诉别人哦。"],
        ("whisper", _) => &["真的吗？我听见啦。", "你靠得太近了。"],
        ("nuzzle", _) => &["靠一下就好。", "今天的距离刚刚好。"],
        ("chase", "chaser") => &["站住！别跑！", "这次我一定抓到你。"],
        ("chase", _) => &["才不会被你抓到！", "追得上再说吧。"],
        ("tag", "chaser") => &["轮到我来抓你啦！"],
        ("tag", _) => &["换我抓你了！"],
        ("follow", "leader") => &["跟紧一点，不要走散。"],
        ("follow", _) => &["好，我跟着你。"],
        ("sync-jump", _) => &["一、二、跳！", "一起跳起来！"],
        ("stack", "top") => &["借我站一下嘛。"],
        ("stack", _) => &["喂，不要压过来。"],
        ("bump", _) => &["哎呀，撞到啦。", "你没事吧？"],
        ("prank", "prankster") => &["嘿嘿，被我骗到了。"],
        ("prank", _) => &["你怎么可以这样！"],
        ("tug", "winner") => &["这次是我赢了。"],
        ("tug", _) => &["我还没有放手呢！"],
        ("steal", "thief") => &["这个玩具归我啦。"],
        ("steal", _) => &["还给我！那是我的。"],
        ("share-snack", _) => &["一人一半，公平吧？"],
        ("comfort", "comforter") => &["别难过，我陪着你。"],
        ("comfort", _) => &["嗯……谢谢你。"],
        ("reconcile", _) => &["刚才的事，就算和好啦。"],
        ("group-nap", _) => &["一起睡一会儿吧。"],
        ("parade", "leader") => &["跟上队伍，出发！"],
        ("circle-chat", _) => &["大家都在听吗？"],
        ("group-pile", _) => &["不要挤，我也要进来。"],
        ("group-cheer", _) => &["耶！一起庆祝！"],
        ("chain-chase", _) => &["接下来轮到你啦！"],
        ("toy-scramble", _) => &["玩具在这里！快抢！"],
        _ => &["我们一起玩吧。"],
    };
    lines[index % lines.len()].to_string()
}

fn role_for(scene: &str, index: usize) -> String {
    match scene {
        "chase" | "tag" | "chain-chase" => {
            if index == 0 {
                "chaser"
            } else {
                "runner"
            }
            .to_string()
        }
        "stack" => {
            if index == 0 {
                "base"
            } else {
                "top"
            }
            .to_string()
        }
        "tug" => {
            if index == 0 {
                "winner"
            } else {
                "challenger"
            }
            .to_string()
        }
        "steal" => {
            if index == 0 {
                "thief"
            } else {
                "owner"
            }
            .to_string()
        }
        "prank" => {
            if index == 0 {
                "prankster"
            } else {
                "target"
            }
            .to_string()
        }
        "comfort" => {
            if index == 0 {
                "comforter"
            } else {
                "comforted"
            }
            .to_string()
        }
        "greet" | "parade" => {
            if index == 0 {
                "leader"
            } else {
                "friend"
            }
            .to_string()
        }
        _ => {
            if index == 0 {
                "leader"
            } else {
                "friend"
            }
            .to_string()
        }
    }
}

fn local_dialogue(
    app: &tauri::AppHandle,
    pet_id: &str,
    scene: &str,
    role: &str,
    index: usize,
    partner: Option<&str>,
) -> String {
    let card = super::load_pet_character(app, pet_id).unwrap_or_default();
    let social = card_social(&card);
    let key = if role == "winner" { "tug-win" } else { scene };
    if let Some(lines) = social.dialogue.get(key).filter(|lines| !lines.is_empty()) {
        return lines[index % lines.len()]
            .chars()
            .take(SOCIAL_EVENT_MAX_CHARS)
            .collect();
    }
    if let Some(partner) = partner {
        if let Some(relationship) = social.relationships.get(partner) {
            if let Some(lines) = relationship.dialogue.get(key).filter(|lines| !lines.is_empty()) {
                return lines[index % lines.len()]
                    .chars()
                    .take(SOCIAL_EVENT_MAX_CHARS)
                    .collect();
            }
        }
    }
    generic_line(scene, role, index)
}

fn snapshots(app: &tauri::AppHandle, config: &AppConfig) -> Vec<Snapshot> {
    let runtime_positions = app
        .state::<AppState>()
        .social
        .runtime
        .lock()
        .map(|runtime| {
            runtime
                .iter()
                .filter_map(|(instance_id, state)| {
                    state.position.clone().map(|position| (instance_id.clone(), position))
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    super::visible_instances(app, config)
        .into_iter()
        .filter(|instance| {
            let settings = super::settings_for_pet(config, &instance.pet_id);
            instance.visible
                && settings.social_enabled
                && !settings.paused
                && !settings.quiet_mode
                && !config.disabled_pet_ids.iter().any(|id| id == &instance.pet_id)
        })
        .filter_map(|instance| {
            let label = super::instance_label(&instance.id).ok()?;
            let window = app.get_webview_window(&label)?;
            let position = runtime_positions
                .get(&instance.id)
                .cloned()
                .or_else(|| instance.position.clone())?;
            let monitor = window.current_monitor().ok().flatten();
            let monitor_key = monitor
                .as_ref()
                .map(|monitor| format!("{}:{}", monitor.position().x, monitor.position().y))
                .unwrap_or_else(|| "primary".to_string());
            let scale = window.scale_factor().ok().unwrap_or(1.0).max(1.0);
            let size = window.outer_size().ok()?;
            Some(Snapshot {
                instance_id: instance.id,
                pet_id: instance.pet_id,
                position: position.clone(),
                monitor_key,
                bounds: Rect {
                    left: position.x,
                    top: position.y,
                    right: position.x + f64::from(size.width) / scale,
                    bottom: position.y + f64::from(size.height) / scale,
                },
            })
        })
        .collect()
}

fn choose_candidates(
    app: &tauri::AppHandle,
    config: &AppConfig,
    requested: &[String],
) -> Result<Vec<Snapshot>, String> {
    let available = snapshots(app, config);
    if !requested.is_empty() {
        let mut selected = Vec::new();
        for pet_id in requested.iter().take(config.social.max_participants as usize) {
            if let Some(snapshot) = available.iter().find(|item| &item.pet_id == pet_id) {
                if !selected
                    .iter()
                    .any(|item: &Snapshot| item.pet_id == snapshot.pet_id)
                {
                    selected.push(snapshot.clone());
                }
            }
        }
        if requested.len() == 1 && selected.len() == 1 {
            let source = selected[0].clone();
            let mut nearby = available
                .iter()
                .filter(|item| {
                    item.pet_id != source.pet_id && item.monitor_key == source.monitor_key
                })
                .cloned()
                .collect::<Vec<_>>();
            nearby.sort_by(|left, right| {
                let left_distance = (left.position.x - source.position.x)
                    .hypot(left.position.y - source.position.y);
                let right_distance = (right.position.x - source.position.x)
                    .hypot(right.position.y - source.position.y);
                left_distance
                    .partial_cmp(&right_distance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            selected.extend(
                nearby
                    .into_iter()
                    .take(config.social.max_participants.saturating_sub(1) as usize),
            );
        }
        if selected.len() >= 2 {
            let monitor = selected[0].monitor_key.clone();
            selected.retain(|item| item.monitor_key == monitor);
            return if selected.len() >= 2 {
                Ok(selected)
            } else {
                Err("宠物需要位于同一块屏幕才能互动".to_string())
            };
        }
        return Err("至少需要两只可见且未暂停的宠物".to_string());
    }
    let mut groups: HashMap<String, Vec<Snapshot>> = HashMap::new();
    for snapshot in available {
        groups
            .entry(snapshot.monitor_key.clone())
            .or_default()
            .push(snapshot);
    }
    let mut group = groups
        .into_values()
        .find(|group| group.len() >= 2)
        .unwrap_or_default();
    group.sort_by(|left, right| left.pet_id.cmp(&right.pet_id));
    group.truncate(config.social.max_participants as usize);
    if group.len() < 2 {
        return Err("当前没有两只位于同一屏幕的可互动宠物".to_string());
    }
    Ok(group)
}

fn scene_prompt(app: &tauri::AppHandle, _config: &AppConfig, actors: &[Snapshot]) -> String {
    let mut prompt = String::from(
        "你是桌面宠物社交导演。只返回 JSON，不要 Markdown。只能从给出的 petId、场景和道具中选择。不能返回坐标、动画名、工具或数值修改。每句 say 不超过 80 个中文字符。允许的格式：{\"scene\":\"chase\",\"participants\":[{\"petId\":\"...\",\"role\":\"...\",\"say\":\"...\"}],\"prop\":null,\"relationshipSignals\":[{\"from\":\"...\",\"to\":\"...\",\"change\":\"fondness|jealous|rivalry|trust|resentment|romance|breakup|reconcile\"}]}。恋爱只能用于角色卡明确允许的组合。",
    );
    prompt.push_str("\n宠物：\n");
    for actor in actors {
        let card = super::load_pet_character(app, &actor.pet_id).unwrap_or_default();
        let social = card_social(&card);
        let relationship = actors
            .iter()
            .filter(|partner| partner.pet_id != actor.pet_id)
            .find_map(|partner| social.relationships.get(&partner.pet_id))
            .map(|relationship| {
                format!(
                    "特殊关系：{}；恋爱允许：{}；关系提示：{}",
                    relationship.initial_disposition,
                    relationship.romance_allowed,
                    relationship.prompt
                )
            })
            .unwrap_or_default();
        prompt.push_str(&format!(
            "- petId={}，角色={}，性格={:?}，{}\n",
            actor.pet_id, card.name, social.traits, relationship
        ));
    }
    prompt.push_str(&format!("当前时间：{}。", Local::now().format("%H:%M")));
    prompt
}

fn extract_json(value: &str) -> Option<Value> {
    let trimmed = value.trim();
    serde_json::from_str(trimmed).ok().or_else(|| {
        let start = trimmed.find('{')?;
        let end = trimmed.rfind('}')?;
        serde_json::from_str(&trimmed[start..=end]).ok()
    })
}

async fn choose_scene_with_ai(
    app: &tauri::AppHandle,
    config: &AppConfig,
    candidates: &[Snapshot],
) -> Option<ModelSceneDecision> {
    if !config.ai.enabled
        || !config.ai.pet_conversation_enabled
        || !super::ai::settings_have_chat(&config.ai)
    {
        return None;
    }
    let prompt = scene_prompt(app, config, candidates);
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        super::ai::request_social_director(app, &prompt),
    )
    .await
    .ok()?
    .ok()?;
    let value = extract_json(&result)?;
    serde_json::from_value(value).ok()
}

fn compile_decision(
    app: &tauri::AppHandle,
    candidates: &[Snapshot],
    decision: Option<ModelSceneDecision>,
    trigger: &str,
) -> (
    String,
    Option<String>,
    Vec<ModelRelationshipSignal>,
    Vec<(String, String, String)>,
) {
    let candidate_ids: HashSet<&str> =
        candidates.iter().map(|item| item.pet_id.as_str()).collect();
    if let Some(decision) = decision {
        let mut actors = Vec::new();
        for actor in decision.participants {
            if !candidate_ids.contains(actor.pet_id.as_str())
                || actors
                    .iter()
                    .any(|(pet_id, _, _): &(String, String, String)| pet_id == &actor.pet_id)
            {
                continue;
            }
            let role = actor.role.trim();
            let role = if role.is_empty() {
                "friend"
            } else {
                role
            };
            actors.push((
                actor.pet_id,
                role.chars().take(32).collect(),
                actor.say.chars().take(80).collect(),
            ));
        }
        // Validate the scene against the participants selected by the model,
        // not the whole candidate pool. This allows a model to choose a
        // two-pet chase from a four-pet screen without accidentally turning it
        // into an invalid group scene.
        let scene = valid_scene(&decision.scene, actors.len());
        if let Some(scene) = scene {
            if actors.len() >= 2 {
                return (
                    scene,
                    valid_prop(decision.prop.as_deref()),
                    decision.relationship_signals,
                    actors,
                );
            }
        }
    }
    let options = scene_options(candidates.len());
    let index = if trigger == "scheduled" {
        rand::rng().random_range(0..options.len())
    } else {
        0
    };
    let scene = options[index].to_string();
    let actors = candidates
        .iter()
        .enumerate()
        .map(|(index, actor)| {
            let role = role_for(&scene, index);
            let partner = candidates
                .iter()
                .find(|candidate| candidate.pet_id != actor.pet_id)
                .map(|candidate| candidate.pet_id.as_str());
            (
                actor.pet_id.clone(),
                role.clone(),
                local_dialogue(app, &actor.pet_id, &scene, &role, index, partner),
            )
        })
        .collect();
    (scene.clone(), default_prop(&scene), Vec::new(), actors)
}

fn default_prop(scene: &str) -> Option<String> {
    match scene {
        "chase" | "tag" | "tug" | "steal" | "toy-scramble" => Some("toy".to_string()),
        "share-snack" => Some("snack".to_string()),
        "prank" => Some("ribbon".to_string()),
        _ => None,
    }
}

fn target_positions(scene: &str, candidates: &[Snapshot]) -> Vec<PetPosition> {
    let center_x =
        candidates.iter().map(|item| item.position.x).sum::<f64>() / candidates.len() as f64;
    let center_y =
        candidates.iter().map(|item| item.position.y).sum::<f64>() / candidates.len() as f64;
    if candidates.len() == 2 {
        let direction = if candidates[1].position.x >= candidates[0].position.x {
            1.0
        } else {
            -1.0
        };
        return match scene {
            "chase" | "tag" => vec![
                PetPosition {
                    x: center_x - 90.0 * direction,
                    y: center_y,
                },
                PetPosition {
                    x: center_x + 90.0 * direction,
                    y: center_y,
                },
            ],
            "stack" => vec![
                PetPosition {
                    x: center_x,
                    y: center_y + 18.0,
                },
                PetPosition {
                    x: center_x,
                    y: center_y - 76.0,
                },
            ],
            _ => vec![
                PetPosition {
                    x: center_x - 82.0,
                    y: center_y,
                },
                PetPosition {
                    x: center_x + 82.0,
                    y: center_y,
                },
            ],
        };
    }
    (0..candidates.len())
        .map(|index| {
            let angle = (index as f64 / candidates.len() as f64) * std::f64::consts::TAU;
            PetPosition {
                x: center_x + angle.cos() * 110.0,
                y: center_y + angle.sin() * 46.0,
            }
        })
        .collect()
}

fn clamp_targets(targets: &mut [PetPosition], candidates: &[Snapshot]) {
    let min_x = candidates
        .iter()
        .map(|item| item.bounds.left)
        .fold(f64::INFINITY, f64::min)
        - 240.0;
    let min_y = candidates
        .iter()
        .map(|item| item.bounds.top)
        .fold(f64::INFINITY, f64::min)
        - 120.0;
    let max_x = candidates
        .iter()
        .map(|item| item.bounds.right)
        .fold(f64::NEG_INFINITY, f64::max)
        + 240.0;
    let max_y = candidates
        .iter()
        .map(|item| item.bounds.bottom)
        .fold(f64::NEG_INFINITY, f64::max)
        + 120.0;
    for target in targets {
        target.x = target.x.clamp(min_x, max_x);
        target.y = target.y.clamp(min_y, max_y);
    }
}

fn build_plan(
    app: &tauri::AppHandle,
    candidates: Vec<Snapshot>,
    trigger: String,
    decision: Option<ModelSceneDecision>,
    settings: &SocialSettings,
) -> ScenePlan {
    let scene_id = next_scene_id();
    let (scene, model_prop, relationship_signals, dialogue) =
        compile_decision(app, &candidates, decision, &trigger);
    let selected_ids: HashSet<&str> =
        dialogue.iter().map(|(pet_id, _, _)| pet_id.as_str()).collect();
    let mut selected: Vec<Snapshot> = candidates
        .iter()
        .filter(|candidate| selected_ids.contains(candidate.pet_id.as_str()))
        .cloned()
        .collect();
    if selected.len() < 2 {
        selected = candidates.clone();
    }
    let mut targets = target_positions(&scene, &selected);
    clamp_targets(&mut targets, &selected);
    let actors = selected
        .into_iter()
        .enumerate()
        .map(|(index, snapshot)| {
            let role = role_for(&scene, index);
            let (role, say) = dialogue
                .iter()
                .find(|(pet_id, _, _)| pet_id == &snapshot.pet_id)
                .map(|(_, model_role, say)| {
                    (
                        if model_role.is_empty() {
                            role.clone()
                        } else {
                            model_role.clone()
                        },
                        say.clone(),
                    )
                })
                .unwrap_or_else(|| {
                    let partner = candidates
                        .iter()
                        .find(|candidate| candidate.pet_id != snapshot.pet_id)
                        .map(|candidate| candidate.pet_id.as_str());
                    (
                        role.clone(),
                        local_dialogue(
                            app,
                            &snapshot.pet_id,
                            &scene,
                            &role,
                            index,
                            partner,
                        ),
                    )
                });
            PlannedActor {
                snapshot,
                role,
                say,
                target: targets[index].clone(),
            }
        })
        .collect::<Vec<_>>();
    let stage = Rect::from_points(actors.iter().map(|actor| (actor.target.x, actor.target.y)))
        .unwrap_or_default()
        .padded(160.0);
    let duration_ms = 1_100
        + actors
            .iter()
            .map(|actor| {
                let dx = actor.snapshot.position.x - actor.target.x;
                let dy = actor.snapshot.position.y - actor.target.y;
                (dx.hypot(dy) / 0.32).clamp(400.0, 2_800.0) as u64
            })
            .max()
            .unwrap_or(900);
    let prop = if settings.props_enabled { model_prop } else { None };
    ScenePlan {
        scene_id,
        scene,
        trigger,
        prop,
        actors,
        stage,
        duration_ms,
        relationship_signals,
    }
}

fn reserve_scene(app: &tauri::AppHandle, plan: &ScenePlan) -> Result<Arc<AtomicBool>, String> {
    let state = app.state::<AppState>();
    let mut active = state
        .social
        .active
        .lock()
        .map_err(|_| "社交场景状态损坏".to_string())?;
    let ids: HashSet<&str> = plan
        .actors
        .iter()
        .map(|actor| actor.snapshot.pet_id.as_str())
        .collect();
    if active.values().any(|scene| {
        scene.stage.overlaps(plan.stage)
            || scene
                .participants
                .iter()
                .any(|pet_id| ids.contains(pet_id.as_str()))
    }) {
        return Err(SCENE_BUSY_ERROR.to_string());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    active.insert(
        plan.scene_id.clone(),
        ActiveScene {
            participants: ids.into_iter().map(str::to_string).collect(),
            stage: plan.stage,
            cancel: cancel.clone(),
        },
    );
    Ok(cancel)
}

fn emit_phase(app: &tauri::AppHandle, scene_id: &str, phase: &str, actors: &[ScenePhaseActor]) {
    let _ = app.emit(
        "pet://social-phase",
        ScenePhaseEvent {
            scene_id: scene_id.to_string(),
            phase: phase.to_string(),
            participants: actors.to_vec(),
        },
    );
}

fn phase_actors(plan: &ScenePlan, phase: &str) -> Vec<ScenePhaseActor> {
    plan.actors
        .iter()
        .enumerate()
        .map(|(index, actor)| {
            let animation = match phase {
                "approach" => {
                    if plan.scene == "chase" || plan.scene == "tag" {
                        "running"
                    } else {
                        "walking"
                    }
                }
                "interaction" => match plan.scene.as_str() {
                    "sync-jump" | "group-cheer" => "jumping",
                    "group-nap" => "waiting",
                    "chase" | "tag" | "chain-chase" => "running",
                    _ => "waving",
                },
                _ => "idle",
            };
            let look = if index % 2 == 0 {
                Some("right".to_string())
            } else {
                Some("left".to_string())
            };
            ScenePhaseActor {
                instance_id: actor.snapshot.instance_id.clone(),
                pet_id: actor.snapshot.pet_id.clone(),
                animation: animation.to_string(),
                look,
                say: (phase == "interaction").then(|| actor.say.clone()),
                effect: (phase == "interaction").then(|| {
                    match plan.scene.as_str() {
                        "nuzzle" | "comfort" | "reconcile" => "heart",
                        "sync-jump" | "group-cheer" => "star",
                        "chase" | "tag" | "toy-scramble" => "dust",
                        _ => "sparkle",
                    }
                    .to_string()
                }),
            }
        })
        .collect()
}

async fn sleep_or_cancel(duration: Duration, cancel: &AtomicBool) -> bool {
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return true;
        }
        tokio::time::sleep(
            (deadline - now).min(Duration::from_millis(SCENE_TICK_MS)),
        )
        .await;
    }
}

async fn move_actors(app: &tauri::AppHandle, plan: &ScenePlan, cancel: &AtomicBool) -> bool {
    let started = tokio::time::Instant::now();
    let duration = Duration::from_millis(plan.duration_ms);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let progress = (started.elapsed().as_secs_f64() / duration.as_secs_f64()).min(1.0);
        let eased = progress * progress * (3.0 - 2.0 * progress);
        for actor in &plan.actors {
            let x = actor.snapshot.position.x
                + (actor.target.x - actor.snapshot.position.x) * eased;
            let y = actor.snapshot.position.y
                + (actor.target.y - actor.snapshot.position.y) * eased;
            if let Ok(label) = super::instance_label(&actor.snapshot.instance_id) {
                if let Some(window) = app.get_webview_window(&label) {
                    let _ = window.set_position(LogicalPosition::new(x, y));
                }
            }
        }
        if progress >= 1.0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(SCENE_TICK_MS)).await;
    }
}

fn update_scene_runtime_positions(app: &tauri::AppHandle, plan: &ScenePlan) {
    let state = app.state::<AppState>();
    let Ok(mut runtime) = state.social.runtime.lock() else {
        return;
    };
    for actor in &plan.actors {
        let entry = runtime
            .entry(actor.snapshot.instance_id.clone())
            .or_insert_with(|| RuntimePetState {
                instance_id: actor.snapshot.instance_id.clone(),
                pet_id: actor.snapshot.pet_id.clone(),
                ..RuntimePetState::default()
            });
        entry.position = Some(actor.target.clone());
        entry.dragging = false;
        entry.busy = false;
    }
}

fn create_prop_window(
    app: &tauri::AppHandle,
    scene_id: &str,
    kind: &str,
    position: PetPosition,
) -> Option<String> {
    let label = format!("social-prop-{scene_id}");
    let window = WebviewWindowBuilder::new(
        app,
        &label,
        WebviewUrl::App(format!("social-prop.html?kind={kind}").into()),
    )
    .title("")
    .inner_size(72.0, 72.0)
    .position(position.x, position.y)
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .visible(true)
    .additional_browser_args(super::BROWSER_ARGS)
    .build()
    .ok()?;
    let _ = window.set_position(LogicalPosition::new(position.x, position.y));
    Some(label)
}

async fn run_scene(app: tauri::AppHandle, plan: ScenePlan, cancel: Arc<AtomicBool>) {
    let participants = plan
        .actors
        .iter()
        .enumerate()
        .map(|(index, actor)| SceneActor {
            instance_id: actor.snapshot.instance_id.clone(),
            pet_id: actor.snapshot.pet_id.clone(),
            role: if actor.role.is_empty() {
                role_for(&plan.scene, index)
            } else {
                actor.role.clone()
            },
        })
        .collect();
    let _ = app.emit(
        "pet://social-scene-start",
        SceneStartEvent {
            scene_id: plan.scene_id.clone(),
            scene: plan.scene.clone(),
            trigger: plan.trigger.clone(),
            participants,
            prop: plan.prop.clone(),
            duration_ms: plan.duration_ms + 3_000,
        },
    );
    emit_phase(&app, &plan.scene_id, "approach", &phase_actors(&plan, "approach"));
    let arrived = move_actors(&app, &plan, &cancel).await;
    if arrived {
        update_scene_runtime_positions(&app, &plan);
        let mut prop_label = None;
        if let Some(kind) = plan.prop.as_deref() {
            let center = plan.stage;
            prop_label = create_prop_window(
                &app,
                &plan.scene_id,
                kind,
                PetPosition {
                    x: (center.left + center.right) / 2.0,
                    y: (center.top + center.bottom) / 2.0,
                },
            );
        }
        emit_phase(
            &app,
            &plan.scene_id,
            "interaction",
            &phase_actors(&plan, "interaction"),
        );
        let _ = sleep_or_cancel(Duration::from_millis(2_400), &cancel).await;
        emit_phase(&app, &plan.scene_id, "settle", &phase_actors(&plan, "settle"));
        let _ = sleep_or_cancel(Duration::from_millis(600), &cancel).await;
        if let Some(label) = prop_label {
            if let Some(window) = app.get_webview_window(&label) {
                let _ = window.destroy();
            }
        }
        if !cancel.load(Ordering::Relaxed) {
            let dialogue = plan
                .actors
                .iter()
                .map(|actor| SocialLogDialogue {
                    pet_id: actor.snapshot.pet_id.clone(),
                    text: actor.say.clone(),
                })
                .collect::<Vec<_>>();
            let mut milestones = Vec::new();
            for first_index in 0..plan.actors.len() {
                for second_index in (first_index + 1)..plan.actors.len() {
                    if let Ok(relationship) = record_relationship_event(
                        &app,
                        &plan.actors[first_index].snapshot.pet_id,
                        &plan.actors[second_index].snapshot.pet_id,
                        &plan.scene,
                        &plan.relationship_signals,
                    ) {
                        milestones.extend(relationship.1);
                    }
                }
            }
            let scene_dialogue = plan
                .actors
                .iter()
                .map(|actor| (actor.snapshot.pet_id.clone(), actor.say.clone()))
                .collect::<Vec<_>>();
            if let Err(error) =
                super::ai::record_social_scene_memory(&app, &plan.scene, &scene_dialogue)
            {
                eprintln!("social scene memory save failed: {error}");
            }
            let entry = SocialLogEntry {
                id: plan.scene_id.clone(),
                timestamp: now_ms(),
                participants: plan
                    .actors
                    .iter()
                    .map(|actor| actor.snapshot.pet_id.clone())
                    .collect(),
                interaction_type: plan.scene.clone(),
                trigger: plan.trigger.clone(),
                dialogue,
                milestones: milestones.clone(),
                outcome: "completed".to_string(),
            };
            let _ = append_log(&app, &entry);
            if !milestones.is_empty() {
                let _ = app.emit(
                    "social://relationship-milestone",
                    serde_json::json!({"participants": entry.participants, "milestones": milestones}),
                );
            }
        }
    }
    let cancelled = cancel.load(Ordering::Relaxed) || !arrived;
    let _ = app.emit(
        "pet://social-scene-end",
        SceneEndEvent {
            scene_id: plan.scene_id.clone(),
            scene: plan.scene.clone(),
            cancelled,
        },
    );
    if let Ok(mut active) = app.state::<AppState>().social.active.lock() {
        active.remove(&plan.scene_id);
    }
}

fn relationship_allowed(app: &tauri::AppHandle, first: &str, second: &str) -> bool {
    let first_card = super::load_pet_character(app, first).unwrap_or_default();
    let second_card = super::load_pet_character(app, second).unwrap_or_default();
    let first_social = card_social(&first_card);
    let second_social = card_social(&second_card);
    first_social
        .relationships
        .get(second)
        .is_some_and(|entry| entry.romance_allowed)
        || second_social
            .relationships
            .get(first)
            .is_some_and(|entry| entry.romance_allowed)
}

fn record_relationship_event(
    app: &tauri::AppHandle,
    first: &str,
    second: &str,
    scene: &str,
    signals: &[ModelRelationshipSignal],
) -> Result<(PublicRelationship, Vec<String>), String> {
    let now = now_ms();
    let mut relationship = load_relationship(app, first, second)?;
    let mut affinity_delta: i16 = 2;
    relationship.peak_affinity = relationship.peak_affinity.max(relationship.affinity);
    relationship.level = relationship_level(relationship.affinity);
    relationship.interaction_count = relationship.interaction_count.saturating_add(1);
    relationship.last_interaction_at = now;
    relationship.last_advanced_at = now;
    let mut milestones = Vec::new();
    for (id, unlocked) in [
        ("first-meet", relationship.interaction_count >= 1),
        ("ten-interactions", relationship.interaction_count >= 10),
        ("fifty-interactions", relationship.interaction_count >= 50),
    ] {
        if unlocked
            && !relationship
                .unlocked_milestones
                .iter()
                .any(|item| item == id)
        {
            relationship.unlocked_milestones.push(id.to_string());
            milestones.push(id.to_string());
        }
    }
    for signal in signals.iter().filter(|signal| {
        (signal.from == first && signal.to == second)
            || (signal.from == second && signal.to == first)
    }) {
        let feelings = relationship.directional.entry(signal.from.clone()).or_default();
        match signal.change.as_str() {
            "fondness" => {
                feelings.fondness = (feelings.fondness + 3).clamp(-100, 100);
                affinity_delta += 2;
            }
            "trust" => {
                feelings.trust = (feelings.trust + 3).clamp(-100, 100);
                affinity_delta += 2;
            }
            "jealous" => feelings.jealousy = (feelings.jealousy + 3).clamp(-100, 100),
            "rivalry" => {
                feelings.rivalry = (feelings.rivalry + 3).clamp(-100, 100);
                affinity_delta -= 1;
            }
            "resentment" => {
                feelings.resentment = (feelings.resentment + 3).clamp(-100, 100);
                affinity_delta -= 2;
            }
            "romance" if relationship_allowed(app, first, second) => {
                relationship.romance_status = "dating".to_string()
            }
            "breakup" => {
                relationship.romance_status = "none".to_string();
                affinity_delta -= 3;
            }
            "reconcile" if relationship_allowed(app, first, second) => {
                relationship.romance_status = "dating".to_string();
                affinity_delta += 3;
            }
            _ => {}
        }
    }
    if scene == "reconcile" && relationship.romance_status == "none" {
        affinity_delta += 1;
    }
    relationship.affinity = if affinity_delta >= 0 {
        relationship
            .affinity
            .saturating_add(affinity_delta as u8)
            .min(100)
    } else {
        relationship.affinity.saturating_sub((-affinity_delta) as u8)
    };
    relationship.peak_affinity = relationship.peak_affinity.max(relationship.affinity);
    relationship.level = relationship_level(relationship.affinity);
    save_relationship(app, &relationship)?;
    let public = PublicRelationship::from(&relationship);
    let _ = app.emit("pet://pair-relationship", &public);
    Ok((public, milestones))
}

fn start_scene_task(app: &tauri::AppHandle, plan: ScenePlan, cancel: Arc<AtomicBool>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move { run_scene(app, plan, cancel).await });
}

async fn start_scene_internal(
    app: tauri::AppHandle,
    requested: Vec<String>,
    trigger: String,
) -> Result<SocialSceneSummary, String> {
    match start_scene_once(app.clone(), requested.clone(), trigger.clone()).await {
        Ok(summary) => Ok(summary),
        Err(error) if error == SCENE_BUSY_ERROR => {
            let retry_app = app.clone();
            let retry_requested = requested.clone();
            let retry_trigger = trigger.clone();
            tauri::async_runtime::spawn(async move {
                // Re-evaluate the participants and their current positions
                // on every attempt. This is a real queue, not a stale scene
                // reservation, so user dragging/hiding/pausing still wins.
                for _ in 0..30 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    match start_scene_once(
                        retry_app.clone(),
                        retry_requested.clone(),
                        retry_trigger.clone(),
                    )
                    .await
                    {
                        Ok(_) => break,
                        Err(error) if error == SCENE_BUSY_ERROR => continue,
                        Err(_) => break,
                    }
                }
            });
            Ok(SocialSceneSummary {
                scene_id: format!("queued-{}", now_ms()),
                scene: "queued".to_string(),
                trigger,
                participants: requested,
                queued: true,
            })
        }
        Err(error) => Err(error),
    }
}

async fn start_scene_once(
    app: tauri::AppHandle,
    requested: Vec<String>,
    trigger: String,
) -> Result<SocialSceneSummary, String> {
    let config = config_snapshot(&app)?;
    if !config.social.enabled {
        return Err("宠物社交已关闭".to_string());
    }
    let candidates = choose_candidates(&app, &config, &requested)?;
    let decision = choose_scene_with_ai(&app, &config, &candidates).await;
    let plan = build_plan(&app, candidates, trigger.clone(), decision, &config.social);
    let participants = plan
        .actors
        .iter()
        .map(|actor| actor.snapshot.pet_id.clone())
        .collect::<Vec<_>>();
    let cancel = reserve_scene(&app, &plan)?;
    let summary = SocialSceneSummary {
        scene_id: plan.scene_id.clone(),
        scene: plan.scene.clone(),
        trigger,
        participants,
        queued: false,
    };
    start_scene_task(&app, plan, cancel);
    Ok(summary)
}

fn maybe_start_proximity(app: &tauri::AppHandle, pet_id: &str) {
    let Ok(config) = config_snapshot(app) else {
        return;
    };
    if !config.social.proximity_enabled || !config.social.enabled {
        return;
    }
    let available = snapshots(app, &config);
    let Some(source) = available.iter().find(|item| item.pet_id == pet_id) else {
        return;
    };
    let Some(target) = available
        .iter()
        .filter(|item| item.pet_id != pet_id && item.monitor_key == source.monitor_key)
        .filter(|item| {
            source.position.x - item.position.x < PROXIMITY_DISTANCE
                && item.position.x - source.position.x < PROXIMITY_DISTANCE
                && (item.position.y - source.position.y).abs() < PROXIMITY_DISTANCE
        })
        .min_by(|left, right| {
            let left_distance =
                (left.position.x - source.position.x).hypot(left.position.y - source.position.y);
            let right_distance =
                (right.position.x - source.position.x).hypot(right.position.y - source.position.y);
            left_distance
                .partial_cmp(&right_distance)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    else {
        return;
    };
    let app = app.clone();
    let ids = vec![pet_id.to_string(), target.pet_id.clone()];
    tauri::async_runtime::spawn(async move {
        let _ = start_scene_internal(app, ids, "proximity".to_string()).await;
    });
}

pub(crate) fn cancel_scenes_for_pet(app: &tauri::AppHandle, pet_id: &str) {
    if let Ok(active) = app.state::<AppState>().social.active.lock() {
        for scene in active
            .values()
            .filter(|scene| scene.participants.iter().any(|id| id == pet_id))
        {
            scene.cancel.store(true, Ordering::Relaxed);
        }
    }
}

pub(crate) fn cancel_all_scenes(app: &tauri::AppHandle) {
    if let Ok(active) = app.state::<AppState>().social.active.lock() {
        for scene in active.values() {
            scene.cancel.store(true, Ordering::Relaxed);
        }
    }
}

#[tauri::command]
pub(crate) async fn start_social_interaction(
    app: tauri::AppHandle,
    source_pet_id: Option<String>,
    target_pet_ids: Option<Vec<String>>,
) -> Result<SocialSceneSummary, String> {
    let mut requested = Vec::new();
    if let Some(source) = source_pet_id.filter(|id| !id.is_empty()) {
        requested.push(source);
    }
    requested.extend(target_pet_ids.unwrap_or_default());
    let config = config_snapshot(&app)?;
    if !config.social.manual_enabled {
        return Err("手动宠物社交已关闭".to_string());
    }
    start_scene_internal(app, requested, "manual".to_string()).await
}

#[tauri::command]
pub(crate) fn cancel_social_scene(
    app: tauri::AppHandle,
    scene_id: String,
) -> Result<(), String> {
    if let Some(scene) = app
        .state::<AppState>()
        .social
        .active
        .lock()
        .map_err(|_| "社交场景状态损坏".to_string())?
        .get(&scene_id)
    {
        scene.cancel.store(true, Ordering::Relaxed);
        return Ok(());
    }
    Err("社交场景不存在".to_string())
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RuntimeStateInput {
    pub x: f64,
    pub y: f64,
}

#[tauri::command]
pub(crate) fn report_pet_runtime_state(
    app: tauri::AppHandle,
    instance_id: String,
    pet_id: String,
    dragging: bool,
    busy: bool,
    position: Option<RuntimeStateInput>,
) -> Result<(), String> {
    if !is_safe_id(&instance_id) || !is_safe_id(&pet_id) {
        return Err("无效的宠物运行状态".to_string());
    }
    if dragging {
        cancel_scenes_for_pet(&app, &pet_id);
    }
    let state = app.state::<AppState>();
    if let Ok(mut runtime) = state.social.runtime.lock() {
        runtime.insert(
            instance_id.clone(),
            RuntimePetState {
                instance_id,
                pet_id: pet_id.clone(),
                position: position.map(|position| PetPosition {
                    x: position.x,
                    y: position.y,
                }),
                dragging,
                busy,
            },
        );
    }
    if !dragging && !busy {
        maybe_start_proximity(&app, &pet_id);
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn get_social_settings(
    app: tauri::AppHandle,
) -> Result<SocialSettings, String> {
    Ok(config_snapshot(&app)?.social)
}

#[tauri::command]
pub(crate) fn update_social_settings(
    app: tauri::AppHandle,
    mut settings: SocialSettings,
) -> Result<SocialSettings, String> {
    settings.min_interval_minutes = settings.min_interval_minutes.clamp(1, 120);
    settings.max_interval_minutes = settings
        .max_interval_minutes
        .clamp(settings.min_interval_minutes, 240);
    settings.max_participants = settings.max_participants.clamp(2, 4);
    let config = super::update_config(&app, |config| {
        config.social = settings.clone();
        Ok(())
    })?;
    Ok(config.social)
}

#[tauri::command]
pub(crate) fn get_public_relationships(
    app: tauri::AppHandle,
) -> Result<Vec<PublicRelationship>, String> {
    let config = config_snapshot(&app)?;
    let ids: Vec<String> = super::installed_pets(&app, &config)
        .into_iter()
        .map(|pet| pet.id)
        .collect();
    let mut relationships = Vec::new();
    for (index, first) in ids.iter().enumerate() {
        for second in ids.iter().skip(index + 1) {
            let relationship = load_relationship(&app, first, second)?;
            if relationship.interaction_count > 0 {
                relationships.push(PublicRelationship::from(&relationship));
            }
        }
    }
    Ok(relationships)
}

#[tauri::command]
pub(crate) fn get_social_log(
    app: tauri::AppHandle,
    pet_id: Option<String>,
    second_pet_id: Option<String>,
    interaction_type: Option<String>,
    from_ms: Option<u64>,
    to_ms: Option<u64>,
) -> Result<Vec<SocialLogEntry>, String> {
    let first = pet_id.as_deref();
    let second = second_pet_id.as_deref();
    Ok(read_logs(&app)?
        .into_iter()
        .filter(|entry| {
            first.is_none_or(|id| entry.participants.iter().any(|item| item == id))
        })
        .filter(|entry| {
            second.is_none_or(|id| entry.participants.iter().any(|item| item == id))
        })
        .filter(|entry| {
            interaction_type
                .as_deref()
                .is_none_or(|kind| entry.interaction_type == kind)
        })
        .filter(|entry| from_ms.is_none_or(|from| entry.timestamp >= from))
        .filter(|entry| to_ms.is_none_or(|to| entry.timestamp <= to))
        .rev()
        .take(200)
        .collect())
}

#[tauri::command]
pub(crate) fn clear_social_log(app: tauri::AppHandle) -> Result<(), String> {
    let root = social_root(&app)?.join("events");
    if root.exists() {
        fs::remove_dir_all(root).map_err(|error| format!("无法清理宠物日志: {error}"))?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn open_social_log(app: tauri::AppHandle) -> Result<(), String> {
    let Some(window) = app.get_webview_window("social-log") else {
        return Err("宠物日志窗口不可用".to_string());
    };
    if window.is_minimized().unwrap_or(false) {
        let _ = window.unminimize();
    }
    window.show().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn open_social_settings(app: tauri::AppHandle) -> Result<(), String> {
    let Some(window) = app.get_webview_window("social-settings") else {
        return Err("宠物社交设置窗口不可用".to_string());
    };
    if window.is_minimized().unwrap_or(false) {
        let _ = window.unminimize();
    }
    window.show().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())
}

pub(crate) fn start_scheduler(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let Ok(config) = config_snapshot(&app) else {
                continue;
            };
            if !config.social.enabled {
                continue;
            }
            let now = now_ms();
            let due = app
                .state::<AppState>()
                .social
                .next_scheduled_at
                .lock()
                .map(|mut next| {
                    if *next == 0 {
                        let min = config.social.min_interval_minutes * 60_000;
                        let max = config.social.max_interval_minutes * 60_000;
                        *next = now + u64::from(rand::rng().random_range(min..=max));
                        return false;
                    }
                    if now >= *next {
                        *next = now
                            + u64::from(rand::rng().random_range(
                                config.social.min_interval_minutes * 60_000
                                    ..=config.social.max_interval_minutes * 60_000,
                            ));
                        return true;
                    }
                    false
                })
                .unwrap_or(false);
            if !due {
                continue;
            }
            if let Err(error) =
                start_scene_internal(app.clone(), Vec::new(), "scheduled".to_string()).await
            {
                eprintln!("social scene skipped: {error}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_catalog_supports_pair_and_group_interactions() {
        assert!(scene_options(2).contains(&"chase"));
        assert!(scene_options(4).contains(&"group-pile"));
        assert!(!scene_options(2).contains(&"group-pile"));
    }

    #[test]
    fn relationship_levels_are_bounded() {
        assert_eq!(relationship_level(19), 1);
        assert_eq!(relationship_level(90), 5);
    }

    #[test]
    fn model_json_is_extracted_from_fenced_output() {
        let value = extract_json("{\"scene\":\"greet\"}").unwrap();
        assert_eq!(value["scene"], "greet");
    }
}

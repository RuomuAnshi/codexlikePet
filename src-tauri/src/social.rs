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
const PROXIMITY_COOLDOWN_MS: u64 = 30_000;
const SOCIAL_DIRECTOR_TIMEOUT_SECS: u64 = 30;
const SCENE_TICK_MS: u64 = 40;
const SCENE_BUSY_ERROR: &str = "社交舞台正在使用中，请稍后再试";
const SOCIAL_COLLISION_GAP: f64 = 12.0;
static SCENE_COUNTER: AtomicU64 = AtomicU64::new(1);
static LAST_AI_FALLBACK_LOG_AT: AtomicU64 = AtomicU64::new(0);

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
    last_proximity_scenes: Mutex<HashMap<String, String>>,
    proximity_cooldowns: Mutex<HashMap<String, u64>>,
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

    fn width(self) -> f64 {
        (self.right - self.left).max(0.0)
    }

    fn height(self) -> f64 {
        (self.bottom - self.top).max(0.0)
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

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct SocialLogEntry {
    pub id: String,
    pub timestamp: u64,
    pub participants: Vec<String>,
    pub interaction_type: String,
    pub trigger: String,
    pub prop: Option<String>,
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
    Ok((format!("{first}--{second}"), first.clone(), second.clone()))
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
    let path = relationship_path(app, &relationship.first_pet_id, &relationship.second_pet_id)?;
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
            "pass",
            "share-snack",
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
            "kick-and-chase",
            "pass",
            "fetch",
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
    match value
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        // `ball` and `toy` were used by the first social prototype. Keep
        // accepting them in character cards and model responses, but map
        // them to the named built-in resources.
        "ball" | "football" => Some("football".to_string()),
        "snack" => Some("snack".to_string()),
        "plush" | "toy" => Some("plush".to_string()),
        "ribbon" => Some("ribbon".to_string()),
        _ => None,
    }
}

fn prop_radius(prop: &str) -> f64 {
    match prop {
        "football" => 18.0,
        "snack" => 16.0,
        "plush" => 20.0,
        "ribbon" => 18.0,
        _ => 18.0,
    }
}

fn prop_scene(scene: &str) -> bool {
    matches!(
        scene,
        "kick-and-chase"
            | "pass"
            | "fetch"
            | "share-snack"
            | "tug"
            | "steal"
            | "prank"
            | "toy-scramble"
    )
}

fn generic_line(scene: &str, role: &str) -> String {
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
        ("kick-and-chase", "kicker") => &["接住！我踢过去了！", "快追上它！"],
        ("kick-and-chase", _) => &["等等我！球滚远了！", "这次换我来！"],
        ("pass", "passer") => &["传给你！", "准备好了吗？"],
        ("pass", _) => &["接到了！再传回来！", "好球！"],
        ("fetch", "thrower") => &["去把它捡回来！", "看我扔得多远！"],
        ("fetch", _) => &["我马上回来！", "抓到啦！"],
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
    lines[rand::rng().random_range(0..lines.len())].to_string()
}

fn role_for(scene: &str, index: usize) -> String {
    match scene {
        "chase" | "tag" | "chain-chase" => if index == 0 { "chaser" } else { "runner" }.to_string(),
        "stack" => if index == 0 { "base" } else { "top" }.to_string(),
        "tug" => if index == 0 { "winner" } else { "challenger" }.to_string(),
        "steal" => if index == 0 { "thief" } else { "owner" }.to_string(),
        "kick-and-chase" => if index == 0 { "kicker" } else { "chaser" }.to_string(),
        "pass" => if index == 0 { "passer" } else { "receiver" }.to_string(),
        "fetch" => if index == 0 { "thrower" } else { "fetcher" }.to_string(),
        "prank" => if index == 0 { "prankster" } else { "target" }.to_string(),
        "comfort" => if index == 0 { "comforter" } else { "comforted" }.to_string(),
        "greet" | "parade" => if index == 0 { "leader" } else { "friend" }.to_string(),
        _ => if index == 0 { "leader" } else { "friend" }.to_string(),
    }
}

fn local_dialogue(
    app: &tauri::AppHandle,
    pet_id: &str,
    scene: &str,
    role: &str,
    partner: Option<&str>,
) -> String {
    let card = super::load_pet_character(app, pet_id).unwrap_or_default();
    let social = card_social(&card);
    let key = if role == "winner" { "tug-win" } else { scene };
    if let Some(lines) = social.dialogue.get(key).filter(|lines| !lines.is_empty()) {
        return lines[rand::rng().random_range(0..lines.len())]
            .chars()
            .take(SOCIAL_EVENT_MAX_CHARS)
            .collect();
    }
    if let Some(partner) = partner {
        if let Some(relationship) = social.relationships.get(partner) {
            if let Some(lines) = relationship
                .dialogue
                .get(key)
                .filter(|lines| !lines.is_empty())
            {
                return lines[rand::rng().random_range(0..lines.len())]
                    .chars()
                    .take(SOCIAL_EVENT_MAX_CHARS)
                    .collect();
            }
        }
    }
    generic_line(scene, role)
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
                    state
                        .position
                        .clone()
                        .map(|position| (instance_id.clone(), position))
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
                && !config
                    .disabled_pet_ids
                    .iter()
                    .any(|id| id == &instance.pet_id)
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
        for pet_id in requested
            .iter()
            .take(config.social.max_participants as usize)
        {
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
        "你是桌面宠物社交导演。只返回 JSON，不要 Markdown。只能从给出的 petId、场景和道具中选择。不能返回坐标、速度、动画名、工具或数值修改。每句 say 不超过 80 个中文字符。内置道具只有 football（足球）、snack（零食）、plush（毛绒玩具），也可以不使用道具。足球适合 kick-and-chase、pass、fetch；零食适合 share-snack；毛绒玩具适合 tug、steal、toy-scramble。允许的格式：{\"scene\":\"kick-and-chase\",\"participants\":[{\"petId\":\"...\",\"role\":\"...\",\"say\":\"...\"}],\"prop\":\"football\",\"relationshipSignals\":[{\"from\":\"...\",\"to\":\"...\",\"change\":\"fondness|jealous|rivalry|trust|resentment|romance|breakup|reconcile\"}]}。恋爱只能用于角色卡明确允许的组合。",
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
    if let Ok(value) = serde_json::from_str(trimmed) {
        return Some(value);
    }
    // Find the outermost balanced object, skipping braces inside string
    // literals, so trailing prose or a closing brace in a say string cannot
    // break extraction.
    let start = trimmed.find('{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut end = None;
    for (index, character) in trimmed[start..].char_indices() {
        let absolute = start + index;
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(absolute);
                    break;
                }
            }
            _ => {}
        }
    }
    serde_json::from_str(&trimmed[start..=end?]).ok()
}

fn log_ai_fallback(message: &str) {
    let now = now_ms();
    let previous = LAST_AI_FALLBACK_LOG_AT.load(Ordering::Relaxed);
    if now.saturating_sub(previous) < 60_000 {
        return;
    }
    if LAST_AI_FALLBACK_LOG_AT
        .compare_exchange(previous, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        eprintln!("[social] AI 社交导演降级为本地互动: {message}");
    }
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
    let result = match tokio::time::timeout(
        Duration::from_secs(SOCIAL_DIRECTOR_TIMEOUT_SECS),
        super::ai::request_social_director(app, &prompt, |_| {}),
    )
    .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            log_ai_fallback(&format!("请求失败: {error}"));
            return None;
        }
        Err(_) => {
            log_ai_fallback(&format!("请求超过 {SOCIAL_DIRECTOR_TIMEOUT_SECS} 秒"));
            return None;
        }
    };
    let Some(value) = extract_json(&result) else {
        log_ai_fallback("返回内容不是有效 JSON");
        return None;
    };
    match serde_json::from_value(value) {
        Ok(decision) => Some(decision),
        Err(error) => {
            log_ai_fallback(&format!("JSON 未通过校验: {error}"));
            None
        }
    }
}

fn fallback_scene(app: &tauri::AppHandle, candidates: &[Snapshot], trigger: &str) -> String {
    const PROXIMITY_SCENES: &[&str] = &["greet", "whisper", "nuzzle", "bump"];
    if trigger == "proximity" {
        let mut pair = candidates
            .iter()
            .map(|candidate| candidate.pet_id.as_str())
            .collect::<Vec<_>>();
        pair.sort_unstable();
        let pair_key = pair.join("\u{1f}");
        if let Ok(mut previous_scenes) = app.state::<AppState>().social.last_proximity_scenes.lock()
        {
            let previous = previous_scenes.get(&pair_key).map(String::as_str);
            let choices = PROXIMITY_SCENES
                .iter()
                .copied()
                .filter(|scene| Some(*scene) != previous)
                .collect::<Vec<_>>();
            let selected = choices[rand::rng().random_range(0..choices.len())].to_string();
            previous_scenes.insert(pair_key, selected.clone());
            return selected;
        }
        return PROXIMITY_SCENES[rand::rng().random_range(0..PROXIMITY_SCENES.len())].to_string();
    }

    let options = scene_options(candidates.len());
    options[rand::rng().random_range(0..options.len())].to_string()
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
    let candidate_ids: HashSet<&str> = candidates.iter().map(|item| item.pet_id.as_str()).collect();
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
            let role = if role.is_empty() { "friend" } else { role };
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
    let scene = fallback_scene(app, candidates, trigger);
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
                local_dialogue(app, &actor.pet_id, &scene, &role, partner),
            )
        })
        .collect();
    (scene.clone(), default_prop(&scene), Vec::new(), actors)
}

fn default_prop(scene: &str) -> Option<String> {
    match scene {
        "kick-and-chase" | "pass" | "fetch" | "chase" => Some("football".to_string()),
        "share-snack" => Some("snack".to_string()),
        "tug" | "steal" | "toy-scramble" | "prank" => Some("plush".to_string()),
        _ => None,
    }
}

fn target_positions(scene: &str, candidates: &[Snapshot]) -> Vec<PetPosition> {
    let center_x = candidates
        .iter()
        .map(|item| item.position.x + item.bounds.width() / 2.0)
        .sum::<f64>()
        / candidates.len() as f64;
    let center_y = candidates
        .iter()
        .map(|item| item.position.y + item.bounds.height() / 2.0)
        .sum::<f64>()
        / candidates.len() as f64;
    if candidates.len() == 2 {
        let direction = if candidates[1].position.x >= candidates[0].position.x {
            1.0
        } else {
            -1.0
        };
        let first_width = candidates[0].bounds.width();
        let second_width = candidates[1].bounds.width();
        let first_height = candidates[0].bounds.height();
        let second_height = candidates[1].bounds.height();
        return match scene {
            "chase" | "tag" | "kick-and-chase" | "pass" | "fetch" => vec![
                PetPosition {
                    x: if direction > 0.0 {
                        center_x - (first_width + second_width + SOCIAL_COLLISION_GAP + 180.0) / 2.0
                    } else {
                        center_x + (first_width + second_width + SOCIAL_COLLISION_GAP + 180.0) / 2.0
                            - first_width
                    },
                    y: center_y - first_height / 2.0,
                },
                PetPosition {
                    x: if direction > 0.0 {
                        center_x - (first_width + second_width + SOCIAL_COLLISION_GAP + 180.0) / 2.0
                            + first_width
                            + SOCIAL_COLLISION_GAP
                            + 180.0
                    } else {
                        center_x - (first_width + second_width + SOCIAL_COLLISION_GAP + 180.0) / 2.0
                    },
                    y: center_y - second_height / 2.0,
                },
            ],
            "stack" => vec![
                PetPosition {
                    x: center_x - first_width / 2.0,
                    y: center_y - (first_height + second_height + SOCIAL_COLLISION_GAP) / 2.0,
                },
                PetPosition {
                    x: center_x - second_width / 2.0,
                    y: center_y - (first_height + second_height + SOCIAL_COLLISION_GAP) / 2.0
                        + first_height
                        + SOCIAL_COLLISION_GAP,
                },
            ],
            _ => vec![
                PetPosition {
                    x: if direction > 0.0 {
                        center_x - (first_width + second_width + SOCIAL_COLLISION_GAP + 24.0) / 2.0
                    } else {
                        center_x + (first_width + second_width + SOCIAL_COLLISION_GAP + 24.0) / 2.0
                            - first_width
                    },
                    y: center_y - first_height / 2.0,
                },
                PetPosition {
                    x: if direction > 0.0 {
                        center_x - (first_width + second_width + SOCIAL_COLLISION_GAP + 24.0) / 2.0
                            + first_width
                            + SOCIAL_COLLISION_GAP
                            + 24.0
                    } else {
                        center_x - (first_width + second_width + SOCIAL_COLLISION_GAP + 24.0) / 2.0
                    },
                    y: center_y - second_height / 2.0,
                },
            ],
        };
    }
    (0..candidates.len())
        .map(|index| {
            let angle = (index as f64 / candidates.len() as f64) * std::f64::consts::TAU;
            let snapshot = &candidates[index];
            PetPosition {
                x: center_x + angle.cos() * 190.0 - snapshot.bounds.width() / 2.0,
                y: center_y + angle.sin() * 110.0 - snapshot.bounds.height() / 2.0,
            }
        })
        .collect()
}

fn clamp_targets(targets: &mut [PetPosition], candidates: &[Snapshot]) {
    let clamp_to_screen = |target: &mut PetPosition, index: usize| {
        let width = candidates[index].bounds.width();
        let height = candidates[index].bounds.height();
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
        target.x = target.x.clamp(min_x, (max_x - width).max(min_x));
        target.y = target.y.clamp(min_y, (max_y - height).max(min_y));
    };
    for (index, target) in targets.iter_mut().enumerate() {
        clamp_to_screen(target, index);
    }
    for _ in 0..4 {
        separate_snapshot_positions(targets, candidates);
        for (index, target) in targets.iter_mut().enumerate() {
            clamp_to_screen(target, index);
        }
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
    let selected_ids: HashSet<&str> = dialogue
        .iter()
        .map(|(pet_id, _, _)| pet_id.as_str())
        .collect();
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
                        local_dialogue(app, &snapshot.pet_id, &scene, &role, partner),
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
    // A model may omit `prop` even when it picked a prop-oriented scene. The
    // local template remains authoritative so built-in interactions never
    // degrade into an empty scene because of a partial model response.
    let prop = if settings.props_enabled {
        model_prop.or_else(|| default_prop(&scene))
    } else {
        None
    };
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

/// Convert a screen-space vector into one of the named atlas directions.
/// Screen Y grows downwards, so positive Y means "down" here.
fn look_direction_toward(from: (f64, f64), to: (f64, f64)) -> String {
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    if dx.abs() < 1.0 && dy.abs() < 1.0 {
        return "down".to_string();
    }
    let angle = dy.atan2(dx).to_degrees();
    match angle {
        angle if (-22.5..22.5).contains(&angle) => "right",
        angle if (22.5..67.5).contains(&angle) => "down-right",
        angle if (67.5..112.5).contains(&angle) => "down",
        angle if (112.5..157.5).contains(&angle) => "down-left",
        angle if angle >= 157.5 || angle < -157.5 => "left",
        angle if (-157.5..-112.5).contains(&angle) => "up-left",
        angle if (-112.5..-67.5).contains(&angle) => "up",
        _ => "up-right",
    }
    .to_string()
}

fn actor_center(actor: &PlannedActor, position: &PetPosition) -> (f64, f64) {
    (
        position.x + actor.snapshot.bounds.width() / 2.0,
        position.y + actor.snapshot.bounds.height() / 2.0,
    )
}

/// Choose the direction from the actor's current phase position toward its
/// conversation partner. For a group, face the centre of the other actors.
/// Dynamic positions are used by chase scenes; regular scenes use their
/// collision-resolved targets.
fn phase_look(
    plan: &ScenePlan,
    index: usize,
    phase: &str,
    positions: Option<&[PetPosition]>,
) -> String {
    let actor = &plan.actors[index];
    let current_position = if phase == "approach" {
        &actor.snapshot.position
    } else {
        positions
            .and_then(|items| items.get(index))
            .unwrap_or(&actor.target)
    };
    let from = actor_center(actor, current_position);
    let destination = if phase == "approach" {
        actor_center(actor, &actor.target)
    } else if plan.actors.len() == 2 {
        let partner_index = 1 - index;
        let partner = &plan.actors[partner_index];
        let partner_position = positions
            .and_then(|items| items.get(partner_index))
            .unwrap_or(&partner.target);
        actor_center(partner, partner_position)
    } else {
        let mut count = 0.0;
        let mut x = 0.0;
        let mut y = 0.0;
        for (other_index, other) in plan.actors.iter().enumerate() {
            if other_index == index {
                continue;
            }
            let other_position = positions
                .and_then(|items| items.get(other_index))
                .unwrap_or(&other.target);
            let center = actor_center(other, other_position);
            x += center.0;
            y += center.1;
            count += 1.0;
        }
        if count == 0.0 {
            from
        } else {
            (x / count, y / count)
        }
    };
    look_direction_toward(from, destination)
}

fn phase_actors_at(
    plan: &ScenePlan,
    phase: &str,
    positions: Option<&[PetPosition]>,
    speaker: Option<usize>,
) -> Vec<ScenePhaseActor> {
    plan.actors
        .iter()
        .enumerate()
        .map(|(index, actor)| {
            let is_speaker = speaker.map(|value| value == index).unwrap_or(true);
            let animation = match phase {
                "approach" if plan.actors.len() == 2 => "running",
                "approach" => "walking",
                "face" => "idle",
                "interaction" => match plan.scene.as_str() {
                    "sync-jump" | "group-cheer" | "kick-and-chase" | "pass" | "fetch" => "jumping",
                    "group-nap" => "waiting",
                    _ => "waving",
                },
                _ => "idle",
            };
            ScenePhaseActor {
                instance_id: actor.snapshot.instance_id.clone(),
                pet_id: actor.snapshot.pet_id.clone(),
                animation: animation.to_string(),
                look: Some(phase_look(plan, index, phase, positions)),
                say: (phase == "interaction" && is_speaker).then(|| actor.say.clone()),
                effect: (phase == "interaction" && is_speaker).then(|| {
                    match plan.scene.as_str() {
                        "nuzzle" | "comfort" | "reconcile" => "heart",
                        "sync-jump" | "group-cheer" => "star",
                        "kick-and-chase" | "pass" | "fetch" => "star",
                        "chase" | "tag" | "toy-scramble" => "dust",
                        "share-snack" => "food",
                        _ => "sparkle",
                    }
                    .to_string()
                }),
            }
        })
        .collect()
}

fn phase_actors(plan: &ScenePlan, phase: &str) -> Vec<ScenePhaseActor> {
    phase_actors_at(plan, phase, None, None)
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
        tokio::time::sleep((deadline - now).min(Duration::from_millis(SCENE_TICK_MS))).await;
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
        let mut positions: Vec<PetPosition> = plan
            .actors
            .iter()
            .map(|actor| {
                let x = actor.snapshot.position.x
                    + (actor.target.x - actor.snapshot.position.x) * eased;
                let y = actor.snapshot.position.y
                    + (actor.target.y - actor.snapshot.position.y) * eased;
                PetPosition { x, y }
            })
            .collect();
        separate_positions(&mut positions, plan);
        clamp_scene_positions(&mut positions, plan);
        separate_positions(&mut positions, plan);
        for (move_index, actor) in plan.actors.iter().enumerate() {
            if let Ok(label) = super::instance_label(&actor.snapshot.instance_id) {
                if let Some(window) = app.get_webview_window(&label) {
                    let _ = window.set_position(LogicalPosition::new(
                        positions[move_index].x,
                        positions[move_index].y,
                    ));
                    let _ = super::reposition_pet_speech(app, &actor.snapshot.instance_id);
                }
            }
        }
        if progress >= 1.0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(SCENE_TICK_MS)).await;
    }
}

/// A conservative separation distance used only for choosing a chase stage.
/// Actual collision checks below always use both rectangle axes and the
/// scale-aware window dimensions.
fn window_margin(first: &Snapshot, second: &Snapshot) -> f64 {
    first.bounds.width() + second.bounds.width() + SOCIAL_COLLISION_GAP
}

fn separate_snapshot_positions(positions: &mut [PetPosition], snapshots: &[Snapshot]) {
    for _ in 0..8 {
        let mut changed = false;
        for first in 0..snapshots.len() {
            for second in (first + 1)..snapshots.len() {
                let first_rect = Rect {
                    left: positions[first].x,
                    top: positions[first].y,
                    right: positions[first].x + snapshots[first].bounds.width(),
                    bottom: positions[first].y + snapshots[first].bounds.height(),
                };
                let second_rect = Rect {
                    left: positions[second].x,
                    top: positions[second].y,
                    right: positions[second].x + snapshots[second].bounds.width(),
                    bottom: positions[second].y + snapshots[second].bounds.height(),
                };
                let overlap_x = (first_rect.right.min(second_rect.right) + SOCIAL_COLLISION_GAP)
                    - first_rect.left.max(second_rect.left);
                let overlap_y = (first_rect.bottom.min(second_rect.bottom) + SOCIAL_COLLISION_GAP)
                    - first_rect.top.max(second_rect.top);
                if overlap_x <= 0.0 || overlap_y <= 0.0 {
                    continue;
                }
                changed = true;
                let first_center_x = (first_rect.left + first_rect.right) / 2.0;
                let second_center_x = (second_rect.left + second_rect.right) / 2.0;
                let first_center_y = (first_rect.top + first_rect.bottom) / 2.0;
                let second_center_y = (second_rect.top + second_rect.bottom) / 2.0;
                if overlap_x <= overlap_y {
                    let direction = if second_center_x >= first_center_x {
                        1.0
                    } else {
                        -1.0
                    };
                    let push = overlap_x / 2.0;
                    positions[first].x -= direction * push;
                    positions[second].x += direction * push;
                } else {
                    let direction = if second_center_y >= first_center_y {
                        1.0
                    } else {
                        -1.0
                    };
                    let push = overlap_y / 2.0;
                    positions[first].y -= direction * push;
                    positions[second].y += direction * push;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

fn clamp_scene_positions(positions: &mut [PetPosition], plan: &ScenePlan) {
    for (position, actor) in positions.iter_mut().zip(plan.actors.iter()) {
        let width = actor.snapshot.bounds.width();
        let height = actor.snapshot.bounds.height();
        position.x = position.x.clamp(
            plan.stage.left,
            (plan.stage.right - width).max(plan.stage.left),
        );
        position.y = position.y.clamp(
            plan.stage.top,
            (plan.stage.bottom - height).max(plan.stage.top),
        );
    }
}

/// Push overlapping windows apart along their minimum translation axis. A
/// distance check is not sufficient here: two tall windows can be close in
/// Euclidean distance without touching, while two windows with the same
/// centre can overlap completely.
fn separate_positions(positions: &mut [PetPosition], plan: &ScenePlan) {
    let snapshots = plan
        .actors
        .iter()
        .map(|actor| actor.snapshot.clone())
        .collect::<Vec<_>>();
    separate_snapshot_positions(positions, &snapshots);
}

const CHASE_RUNNER_SPEED: f64 = 34.0;
const CHASE_CHASER_MULTIPLIER: f64 = 1.55;

/// Rectangle the chase can roam in, widened to give a real pursuit lane.
fn chase_bounds(plan: &ScenePlan) -> Rect {
    let mut rect = Rect::from_points(
        plan.actors
            .iter()
            .map(|actor| (actor.snapshot.position.x, actor.snapshot.position.y)),
    )
    .unwrap_or(Rect {
        left: 0.0,
        top: 0.0,
        right: 1_200.0,
        bottom: 800.0,
    });
    rect = rect.padded(220.0);
    if plan.actors.len() == 2 {
        let margin = window_margin(&plan.actors[0].snapshot, &plan.actors[1].snapshot);
        if rect.right - rect.left < margin * 3.0 {
            let center = (rect.left + rect.right) / 2.0;
            rect.left = center - margin * 2.2;
            rect.right = center + margin * 2.2;
        }
    }
    rect
}

fn chase_phase_actors(
    plan: &ScenePlan,
    phase: &str,
    positions: &[PetPosition],
    speaker: Option<usize>,
) -> Vec<ScenePhaseActor> {
    phase_actors_at(plan, phase, Some(positions), speaker)
}

/// Real pursuit for chase-like scenes: the chaser window genuinely moves
/// toward the runner every tick, the runner flees around the stage, windows
/// are kept apart by their own margins, and each pet says its line when it
/// catches the other. Runs for several seconds so it reads as a chase rather
/// than a single glide to a static spot.
async fn run_chase_movement(
    app: &tauri::AppHandle,
    plan: &ScenePlan,
    cancel: &AtomicBool,
    prop_label: Option<&str>,
) -> bool {
    if plan.actors.len() != 2 {
        return move_actors(app, plan, cancel).await;
    }
    let bounds = chase_bounds(plan);
    let mut chaser = if plan.actors[0].role == "runner" {
        1
    } else {
        0
    };
    let mut runner = 1 - chaser;
    let mut positions: Vec<PetPosition> = plan
        .actors
        .iter()
        .map(|actor| actor.snapshot.position.clone())
        .collect();
    let started = tokio::time::Instant::now();
    let minimum_ms = plan.duration_ms.max(5_600);
    let mut voiced = [false, false];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let elapsed = started.elapsed().as_millis() as u64;
        if elapsed >= minimum_ms {
            break;
        }
        let dt = SCENE_TICK_MS as f64 / 1000.0;
        let dx = positions[runner].x - positions[chaser].x;
        let dy = positions[runner].y - positions[chaser].y;
        let distance = dx.hypot(dy).max(1.0);
        positions[chaser].x += dx / distance * CHASE_RUNNER_SPEED * CHASE_CHASER_MULTIPLIER * dt;
        positions[chaser].y += dy / distance * CHASE_RUNNER_SPEED * CHASE_CHASER_MULTIPLIER * dt;
        positions[runner].x -= dx / distance * CHASE_RUNNER_SPEED * dt;
        positions[runner].y -= dy / distance * CHASE_RUNNER_SPEED * dt;
        for position in positions.iter_mut() {
            position.x = position.x.clamp(bounds.left, bounds.right);
            position.y = position.y.clamp(bounds.top, bounds.bottom);
        }
        separate_positions(&mut positions, plan);
        clamp_scene_positions(&mut positions, plan);
        separate_positions(&mut positions, plan);
        for (chase_index, actor) in plan.actors.iter().enumerate() {
            if let Ok(label) = super::instance_label(&actor.snapshot.instance_id) {
                if let Some(window) = app.get_webview_window(&label) {
                    let _ = window.set_position(LogicalPosition::new(
                        positions[chase_index].x,
                        positions[chase_index].y,
                    ));
                    let _ = super::reposition_pet_speech(app, &actor.snapshot.instance_id);
                }
            }
        }
        let chaser_bounds = &plan.actors[chaser].snapshot.bounds;
        let runner_bounds = &plan.actors[runner].snapshot.bounds;
        let chaser_center = (
            positions[chaser].x + chaser_bounds.width() / 2.0,
            positions[chaser].y + chaser_bounds.height() / 2.0,
        );
        let runner_center = (
            positions[runner].x + runner_bounds.width() / 2.0,
            positions[runner].y + runner_bounds.height() / 2.0,
        );
        if let Some(label) = prop_label {
            // Keep the football between the two pets while they run. The
            // small bob is intentionally local and deterministic; the prop
            // window itself is still positioned by Rust, not CSS animation.
            let prop_center = (
                (chaser_center.0 + runner_center.0) / 2.0,
                (chaser_center.1 + runner_center.1) / 2.0 - (elapsed as f64 / 180.0).sin() * 8.0,
            );
            set_prop_window_position(
                app,
                label,
                PetPosition {
                    x: prop_center.0 - 36.0,
                    y: prop_center.1 - 36.0,
                },
            );
        }
        let caught = (runner_center.0 - chaser_center.0).hypot(runner_center.1 - chaser_center.1)
            < ((chaser_bounds.width() + runner_bounds.width()) / 2.0 + 24.0).max(36.0);
        if caught {
            if !voiced[chaser] {
                emit_phase(
                    app,
                    &plan.scene_id,
                    "face",
                    &chase_phase_actors(plan, "face", &positions, None),
                );
                if !sleep_or_cancel(Duration::from_millis(360), cancel).await {
                    return false;
                }
                emit_phase(
                    app,
                    &plan.scene_id,
                    "interaction",
                    &chase_phase_actors(plan, "interaction", &positions, Some(chaser)),
                );
                voiced[chaser] = true;
            }
            std::mem::swap(&mut chaser, &mut runner);
            let direction = if positions[runner].x >= positions[chaser].x {
                1.0
            } else {
                -1.0
            };
            if direction > 0.0 {
                positions[runner].x = positions[chaser].x
                    + plan.actors[chaser].snapshot.bounds.width()
                    + SOCIAL_COLLISION_GAP;
            } else {
                positions[runner].x = positions[chaser].x
                    - plan.actors[runner].snapshot.bounds.width()
                    - SOCIAL_COLLISION_GAP;
            }
            for position in positions.iter_mut() {
                position.x = position.x.clamp(bounds.left, bounds.right);
                position.y = position.y.clamp(bounds.top, bounds.bottom);
            }
            separate_positions(&mut positions, plan);
            clamp_scene_positions(&mut positions, plan);
            if voiced.iter().all(|value| *value) && elapsed >= (minimum_ms * 3) / 4 {
                break;
            }
        }
        let _ = sleep_or_cancel(Duration::from_millis(SCENE_TICK_MS), cancel).await;
    }
    update_runtime_positions(app, plan, &positions);
    true
}

fn update_runtime_positions(app: &tauri::AppHandle, plan: &ScenePlan, positions: &[PetPosition]) {
    let state = app.state::<AppState>();
    let Ok(mut runtime) = state.social.runtime.lock() else {
        return;
    };
    for (index, actor) in plan.actors.iter().enumerate() {
        let entry = runtime
            .entry(actor.snapshot.instance_id.clone())
            .or_insert_with(|| RuntimePetState {
                instance_id: actor.snapshot.instance_id.clone(),
                pet_id: actor.snapshot.pet_id.clone(),
                ..RuntimePetState::default()
            });
        entry.position = positions
            .get(index)
            .cloned()
            .or_else(|| Some(actor.target.clone()));
        entry.dragging = false;
        entry.busy = false;
    }
}

fn update_scene_runtime_positions(app: &tauri::AppHandle, plan: &ScenePlan) {
    let positions: Vec<PetPosition> = plan
        .actors
        .iter()
        .map(|actor| actor.target.clone())
        .collect();
    update_runtime_positions(app, plan, &positions);
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

fn set_prop_window_position(app: &tauri::AppHandle, label: &str, position: PetPosition) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.set_position(LogicalPosition::new(position.x, position.y));
    }
}

/// Single-pet toy play: the pet owns a toy (inventory), the stats/cooldown of
/// "play" apply, and a toy prop window bounces in front of the pet before it
/// catches it. Runs the tween off the main thread so the menu stays responsive.
pub(super) fn play_single_toy(
    app: &tauri::AppHandle,
    instance_id: &str,
    pet_id: &str,
    toy: &str,
) -> Result<(), String> {
    if !matches!(toy, "football" | "ribbon" | "plush") {
        return Err("这个玩具还不能玩".to_string());
    }
    let state = super::ai::get_pet_state(app.clone(), pet_id.to_string())?;
    if !state.toy_ids.iter().any(|owned| owned == toy) {
        return Err("还没有这个玩具，先多陪陪它吧".to_string());
    }
    if state.activity == "sleeping" {
        return Err("它正在睡觉，别吵醒它".to_string());
    }
    super::ai::perform_pet_action_internal(app, pet_id, "play")?;
    let app_for_task = app.clone();
    let instance_for_task = instance_id.to_string();
    let pet_for_task = pet_id.to_string();
    let toy_for_task = toy.to_string();
    tauri::async_runtime::spawn(async move {
        run_single_toy_tween(
            &app_for_task,
            &instance_for_task,
            &pet_for_task,
            &toy_for_task,
        )
        .await;
    });
    Ok(())
}

async fn run_single_toy_tween(app: &tauri::AppHandle, instance_id: &str, pet_id: &str, toy: &str) {
    let Some(window) =
        app.get_webview_window(&super::instance_label(instance_id).unwrap_or_default())
    else {
        return;
    };
    let (Ok(position), Ok(scale_factor)) = (window.outer_position(), window.scale_factor()) else {
        return;
    };
    let logical: LogicalPosition<f64> = position.to_logical(scale_factor);
    let scene_id = format!("toy-{pet_id}");
    // The pet faces left by default, so the toy starts and lands in front of
    // it (screen-left) and bounces outward from there.
    let base = PetPosition {
        x: logical.x - 52.0,
        y: logical.y + 10.0,
    };
    let far = PetPosition {
        x: (logical.x - 168.0).max(0.0),
        y: logical.y + 2.0,
    };
    let Some(prop_label) = create_prop_window(app, &scene_id, toy, base.clone()) else {
        return;
    };
    let _ = app.emit(
        "pet://toy-play",
        serde_json::json!({"petId": pet_id, "toy": toy}),
    );
    for target in [far.clone(), base.clone(), far, base.clone()] {
        tween_toy_window(app, &prop_label, &base, &target, 520).await;
        if !app.get_webview_window(&prop_label).is_some() {
            break;
        }
    }
    if let Some(prop) = app.get_webview_window(&prop_label) {
        let _ = prop.destroy();
    }
}

async fn tween_toy_window(
    app: &tauri::AppHandle,
    label: &str,
    from: &PetPosition,
    to: &PetPosition,
    duration_ms: u64,
) {
    let Some(_window) = app.get_webview_window(label) else {
        return;
    };
    let started = tokio::time::Instant::now();
    loop {
        let progress = (started.elapsed().as_secs_f64() / (duration_ms as f64 / 1000.0)).min(1.0);
        let eased = progress * progress * (3.0 - 2.0 * progress);
        set_prop_window_position(
            app,
            label,
            PetPosition {
                x: from.x + (to.x - from.x) * eased,
                y: from.y + (to.y - from.y) * eased,
            },
        );
        if progress >= 1.0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(SCENE_TICK_MS)).await;
    }
}

fn prop_position_for_actor(
    actor: &PlannedActor,
    position: &PetPosition,
    prop: &str,
) -> PetPosition {
    let center = actor_center(actor, position);
    let size = 72.0;
    let radius = prop_radius(prop);
    PetPosition {
        x: center.0 - size / 2.0,
        y: center.1 - size / 2.0 - (radius - 18.0) * 0.15,
    }
}

fn prop_stage_center(plan: &ScenePlan) -> PetPosition {
    PetPosition {
        x: (plan.stage.left + plan.stage.right) / 2.0 - 36.0,
        y: (plan.stage.top + plan.stage.bottom) / 2.0 - 36.0,
    }
}

async fn move_prop_window(
    app: &tauri::AppHandle,
    label: &str,
    from: PetPosition,
    to: PetPosition,
    bounce_height: f64,
    duration_ms: u64,
    cancel: &AtomicBool,
) -> bool {
    let started = tokio::time::Instant::now();
    let duration = Duration::from_millis(duration_ms.max(SCENE_TICK_MS));
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let progress = (started.elapsed().as_secs_f64() / duration.as_secs_f64()).min(1.0);
        let eased = progress * progress * (3.0 - 2.0 * progress);
        let lift = (std::f64::consts::PI * progress).sin() * bounce_height;
        set_prop_window_position(
            app,
            label,
            PetPosition {
                x: from.x + (to.x - from.x) * eased,
                y: from.y + (to.y - from.y) * eased - lift,
            },
        );
        if progress >= 1.0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(SCENE_TICK_MS)).await;
    }
}

fn apply_scene_positions(app: &tauri::AppHandle, plan: &ScenePlan, positions: &[PetPosition]) {
    for (index, actor) in plan.actors.iter().enumerate() {
        if let (Some(position), Ok(label)) = (
            positions.get(index),
            super::instance_label(&actor.snapshot.instance_id),
        ) {
            if let Some(window) = app.get_webview_window(&label) {
                let _ = window.set_position(LogicalPosition::new(position.x, position.y));
                let _ = super::reposition_pet_speech(app, &actor.snapshot.instance_id);
            }
        }
    }
}

/// Play a prop-led scene after the pets have run to their shared stage. The
/// coordinator owns the prop's trajectory; the webview only draws its image.
/// This keeps the interaction deterministic and makes the same scene work for
/// two, three, or four pets without trusting model-supplied coordinates.
async fn run_prop_play(
    app: &tauri::AppHandle,
    plan: &ScenePlan,
    prop_label: &str,
    cancel: &AtomicBool,
) -> bool {
    let Some(prop) = plan.prop.as_deref() else {
        return true;
    };
    let positions = plan
        .actors
        .iter()
        .map(|actor| actor.target.clone())
        .collect::<Vec<_>>();
    apply_scene_positions(app, plan, &positions);
    let mut prop_position = prop_stage_center(plan);
    set_prop_window_position(app, prop_label, prop_position.clone());
    emit_phase(app, &plan.scene_id, "face", &phase_actors(plan, "face"));
    if !sleep_or_cancel(Duration::from_millis(300), cancel).await {
        return false;
    }

    let actor_count = plan.actors.len().max(2);
    match prop {
        "football" => {
            let rounds = if plan.scene == "fetch" {
                2
            } else {
                actor_count * 2
            };
            for round in 0..rounds {
                let sender = if plan.scene == "fetch" {
                    round % 2
                } else {
                    round % actor_count
                };
                let receiver = if sender + 1 < actor_count {
                    sender + 1
                } else {
                    0
                };
                if sender >= plan.actors.len() || receiver >= plan.actors.len() {
                    continue;
                }
                emit_phase(
                    app,
                    &plan.scene_id,
                    "interaction",
                    &phase_actors_at(plan, "interaction", Some(&positions), Some(sender)),
                );
                let from = if round == 0 {
                    prop_position.clone()
                } else {
                    prop_position_for_actor(&plan.actors[sender], &positions[sender], prop)
                };
                let to =
                    prop_position_for_actor(&plan.actors[receiver], &positions[receiver], prop);
                if !move_prop_window(app, prop_label, from, to.clone(), 34.0, 720, cancel).await {
                    return false;
                }
                prop_position = to;
                emit_phase(
                    app,
                    &plan.scene_id,
                    "interaction",
                    &phase_actors_at(plan, "interaction", Some(&positions), Some(receiver)),
                );
                if !sleep_or_cancel(Duration::from_millis(360), cancel).await {
                    return false;
                }
            }
        }
        "snack" => {
            // The snack visits each participant in turn, which reads as
            // sharing for groups and as a small tug-of-war for a pair.
            for index in 0..plan.actors.len() {
                emit_phase(
                    app,
                    &plan.scene_id,
                    "interaction",
                    &phase_actors_at(plan, "interaction", Some(&positions), Some(index)),
                );
                let to = prop_position_for_actor(&plan.actors[index], &positions[index], prop);
                if !move_prop_window(
                    app,
                    prop_label,
                    prop_position.clone(),
                    to.clone(),
                    12.0,
                    420,
                    cancel,
                )
                .await
                {
                    return false;
                }
                if !sleep_or_cancel(Duration::from_millis(260), cancel).await {
                    return false;
                }
                prop_position = prop_stage_center(plan);
                if !move_prop_window(app, prop_label, to, prop_position.clone(), 8.0, 320, cancel)
                    .await
                {
                    return false;
                }
            }
        }
        "plush" | "ribbon" => {
            let rounds = if plan.scene == "tug" {
                6
            } else {
                actor_count * 2
            };
            for round in 0..rounds {
                let first = round % actor_count;
                let second = if first + 1 < actor_count {
                    first + 1
                } else {
                    0
                };
                if first >= plan.actors.len() || second >= plan.actors.len() {
                    continue;
                }
                let from = prop_position_for_actor(&plan.actors[first], &positions[first], prop);
                let to = prop_position_for_actor(&plan.actors[second], &positions[second], prop);
                emit_phase(
                    app,
                    &plan.scene_id,
                    "interaction",
                    &phase_actors_at(plan, "interaction", Some(&positions), Some(first)),
                );
                if !move_prop_window(
                    app,
                    prop_label,
                    from,
                    to.clone(),
                    if plan.scene == "tug" { 8.0 } else { 20.0 },
                    if plan.scene == "tug" { 300 } else { 520 },
                    cancel,
                )
                .await
                {
                    return false;
                }
                emit_phase(
                    app,
                    &plan.scene_id,
                    "interaction",
                    &phase_actors_at(plan, "interaction", Some(&positions), Some(second)),
                );
                if !sleep_or_cancel(Duration::from_millis(300), cancel).await {
                    return false;
                }
            }
        }
        _ => {}
    }
    true
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
    emit_phase(
        &app,
        &plan.scene_id,
        "approach",
        &phase_actors(&plan, "approach"),
    );
    let prop_label = plan
        .prop
        .as_deref()
        .and_then(|kind| create_prop_window(&app, &plan.scene_id, kind, prop_stage_center(&plan)));
    let chased = matches!(
        plan.scene.as_str(),
        "chase" | "tag" | "chain-chase" | "follow" | "kick-and-chase"
    );
    let arrived = if chased {
        run_chase_movement(&app, &plan, &cancel, prop_label.as_deref()).await
    } else {
        move_actors(&app, &plan, &cancel).await
    };
    if arrived {
        if !chased {
            update_scene_runtime_positions(&app, &plan);
        }
        let prop_played = if !chased && prop_scene(&plan.scene) {
            if let Some(label) = prop_label.as_deref() {
                run_prop_play(&app, &plan, label, &cancel).await
            } else {
                false
            }
        } else {
            false
        };
        if prop_played {
            if !cancel.load(Ordering::Relaxed) {
                emit_phase(
                    &app,
                    &plan.scene_id,
                    "settle",
                    &phase_actors(&plan, "settle"),
                );
                let _ = sleep_or_cancel(Duration::from_millis(600), &cancel).await;
            }
        } else if chased {
            // The chase already emitted its interactive lines while moving;
            // a short settle still leaves a readable beat before the end.
            if !cancel.load(Ordering::Relaxed) {
                emit_phase(
                    &app,
                    &plan.scene_id,
                    "settle",
                    &phase_actors(&plan, "settle"),
                );
                let _ = sleep_or_cancel(Duration::from_millis(900), &cancel).await;
            }
        } else {
            // Movement and speech are separate phases. The explicit face
            // beat gives the browser a frame to render the new look before
            // the speech windows are shown.
            emit_phase(&app, &plan.scene_id, "face", &phase_actors(&plan, "face"));
            if sleep_or_cancel(Duration::from_millis(360), &cancel).await
                && !cancel.load(Ordering::Relaxed)
            {
                emit_phase(
                    &app,
                    &plan.scene_id,
                    "interaction",
                    &phase_actors(&plan, "interaction"),
                );
                let _ = sleep_or_cancel(Duration::from_millis(2_400), &cancel).await;
            }
            if !cancel.load(Ordering::Relaxed) {
                emit_phase(
                    &app,
                    &plan.scene_id,
                    "settle",
                    &phase_actors(&plan, "settle"),
                );
                let _ = sleep_or_cancel(Duration::from_millis(600), &cancel).await;
            }
        }
        if let Some(label) = prop_label.as_deref() {
            if let Some(window) = app.get_webview_window(label) {
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
                prop: plan.prop.clone(),
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
    // Cancellation can happen while pets are still approaching. Always tear
    // down the transparent prop window, including that early-exit path.
    if let Some(label) = prop_label.as_deref() {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.destroy();
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
        let feelings = relationship
            .directional
            .entry(signal.from.clone())
            .or_default();
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
        relationship
            .affinity
            .saturating_sub((-affinity_delta) as u8)
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

fn claim_proximity_cooldown(app: &tauri::AppHandle, first: &str, second: &str) -> bool {
    let Ok((pair_id, _, _)) = pair_key(first, second) else {
        return false;
    };
    let state = app.state::<AppState>();
    let Ok(mut cooldowns) = state.social.proximity_cooldowns.lock() else {
        return false;
    };
    let now = now_ms();
    cooldowns.retain(|_, until| *until > now);
    if cooldowns.contains_key(&pair_id) {
        return false;
    }
    cooldowns.insert(pair_id, now.saturating_add(PROXIMITY_COOLDOWN_MS));
    true
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
    if !claim_proximity_cooldown(app, pet_id, &target.pet_id) {
        return;
    }
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
pub(crate) fn cancel_social_scene(app: tauri::AppHandle, scene_id: String) -> Result<(), String> {
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
        super::desktop_windows::cancel_for_instance(&app, &instance_id);
    }
    let safe_position = position.map(|position| PetPosition {
        x: position.x,
        y: position.y,
    });
    // Boot and scene recovery can expose two persisted windows at the same
    // coordinates. Repair that state at the shared backend boundary before
    // publishing it to the social coordinator.
    let safe_position = if !dragging && !busy {
        safe_position
            .as_ref()
            .and_then(|requested| {
                super::set_pet_position_safely(
                    app.clone(),
                    instance_id.clone(),
                    requested.x,
                    requested.y,
                )
                .ok()
            })
            .or(safe_position)
    } else {
        safe_position
    };
    let state = app.state::<AppState>();
    if let Ok(mut runtime) = state.social.runtime.lock() {
        runtime.insert(
            instance_id.clone(),
            RuntimePetState {
                instance_id,
                pet_id: pet_id.clone(),
                position: safe_position,
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
pub(crate) fn get_social_settings(app: tauri::AppHandle) -> Result<SocialSettings, String> {
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
        .filter(|entry| first.is_none_or(|id| entry.participants.iter().any(|item| item == id)))
        .filter(|entry| second.is_none_or(|id| entry.participants.iter().any(|item| item == id)))
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
        assert!(scene_options(2).contains(&"kick-and-chase"));
        assert!(scene_options(2).contains(&"pass"));
        assert!(scene_options(4).contains(&"group-pile"));
        assert!(scene_options(4).contains(&"share-snack"));
        assert!(!scene_options(2).contains(&"group-pile"));
    }

    #[test]
    fn built_in_props_keep_legacy_aliases_but_use_named_resources() {
        assert_eq!(valid_prop(Some("ball")), Some("football".to_string()));
        assert_eq!(valid_prop(Some("football")), Some("football".to_string()));
        assert_eq!(valid_prop(Some("toy")), Some("plush".to_string()));
        assert_eq!(valid_prop(Some("snack")), Some("snack".to_string()));
        assert_eq!(valid_prop(Some("unknown")), None);
    }

    #[test]
    fn prop_scene_defaults_match_the_built_in_interactions() {
        assert_eq!(default_prop("kick-and-chase"), Some("football".to_string()));
        assert_eq!(default_prop("share-snack"), Some("snack".to_string()));
        assert_eq!(default_prop("tug"), Some("plush".to_string()));
        assert!(prop_scene("pass"));
        assert!(!prop_scene("whisper"));
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

    #[test]
    fn look_direction_is_derived_from_relative_position() {
        assert_eq!(look_direction_toward((0.0, 0.0), (40.0, 0.0)), "right");
        assert_eq!(look_direction_toward((40.0, 0.0), (0.0, 0.0)), "left");
        assert_eq!(look_direction_toward((0.0, 0.0), (0.0, -40.0)), "up");
        assert_eq!(
            look_direction_toward((0.0, 0.0), (40.0, 40.0)),
            "down-right"
        );
    }

    fn test_snapshot(id: &str, x: f64, y: f64, width: f64, height: f64) -> Snapshot {
        Snapshot {
            instance_id: id.to_string(),
            pet_id: id.to_string(),
            position: PetPosition { x, y },
            monitor_key: "test".to_string(),
            bounds: Rect {
                left: x,
                top: y,
                right: x + width,
                bottom: y + height,
            },
        }
    }

    fn has_collision_gap(
        first: &Snapshot,
        first_position: &PetPosition,
        second: &Snapshot,
        second_position: &PetPosition,
    ) -> bool {
        let first_right = first_position.x + first.bounds.width();
        let first_bottom = first_position.y + first.bounds.height();
        let second_right = second_position.x + second.bounds.width();
        let second_bottom = second_position.y + second.bounds.height();
        first_right + SOCIAL_COLLISION_GAP <= second_position.x
            || second_right + SOCIAL_COLLISION_GAP <= first_position.x
            || first_bottom + SOCIAL_COLLISION_GAP <= second_position.y
            || second_bottom + SOCIAL_COLLISION_GAP <= first_position.y
    }

    #[test]
    fn rectangle_separation_uses_real_width_and_height() {
        let snapshots = vec![
            test_snapshot("small", 0.0, 0.0, 96.0, 104.0),
            test_snapshot("large", 4.0, 8.0, 192.0, 208.0),
            test_snapshot("wide", 12.0, 16.0, 260.0, 120.0),
        ];
        let mut positions = snapshots
            .iter()
            .map(|snapshot| snapshot.position.clone())
            .collect::<Vec<_>>();
        separate_snapshot_positions(&mut positions, &snapshots);
        for first in 0..snapshots.len() {
            for second in (first + 1)..snapshots.len() {
                assert!(has_collision_gap(
                    &snapshots[first],
                    &positions[first],
                    &snapshots[second],
                    &positions[second]
                ));
            }
        }
    }

    #[test]
    fn stack_targets_are_vertical_without_window_overlap() {
        let snapshots = vec![
            test_snapshot("first", 100.0, 100.0, 192.0, 208.0),
            test_snapshot("second", 100.0, 100.0, 96.0, 104.0),
        ];
        let targets = target_positions("stack", &snapshots);
        assert!(has_collision_gap(
            &snapshots[0],
            &targets[0],
            &snapshots[1],
            &targets[1]
        ));
    }
}

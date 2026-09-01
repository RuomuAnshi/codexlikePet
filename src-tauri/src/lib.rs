use base64::Engine;
#[cfg(target_os = "macos")]
use objc2::{runtime::AnyObject, ClassType};
#[cfg(target_os = "macos")]
use objc2_app_kit::{
    NSFloatingWindowLevel, NSPanel, NSScreenSaverWindowLevel, NSWindow, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{
    Emitter, LogicalPosition, LogicalSize, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent,
    Wry,
};
use zip::ZipArchive;

mod ai;
use ai::{AiRuntime, AiSettings};

const LOOK_MARGIN_LOGICAL: f64 = 72.0;
const LOOK_DEADZONE_LOGICAL: f64 = 60.0;
const PET_WIDTH: f64 = 192.0;
const PET_HEIGHT: f64 = 208.0;
const SPRITESHEET_WIDTH: u32 = 1536;
const SPRITESHEET_HEIGHT: u32 = 2288;
const CONFIG_FILE_NAME: &str = "config.json";
// Transparent always-on-top webviews get misjudged as occluded by Chromium,
// which freezes timers (walker), rAF (animation) and input handling. Keep the
// throttling disabled on every webview, and keep wry's default feature flags.
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-backgrounding-occluded-windows --disable-renderer-backgrounding --disable-background-timer-throttling";

#[cfg(target_os = "macos")]
mod macos_overlay {
    use super::*;

    /// Tauri creates `NSWindow` instances. macOS's fullscreen compositor is
    /// stricter than the normal window z-order and requires an actual
    /// non-activating `NSPanel` for a third-party overlay to remain visible.
    ///
    /// The panel class has the same instance size as NSWindow on supported
    /// AppKit versions. Tauri's window is commonly wrapped in AppKit's
    /// `NSKVONotifying_TaoWindow` subclass, so the class check accepts any
    /// NSWindow subclass with the same layout instead of requiring the exact
    /// NSWindow class.
    fn as_panel(native_window: &NSWindow) -> Option<&NSPanel> {
        let current_class = native_window.class();
        let panel_class = NSPanel::class();
        if current_class != panel_class {
            let mut class = Some(current_class);
            let is_ns_window_subclass = std::iter::from_fn(|| {
                let current = class.take()?;
                class = current.superclass();
                Some(current)
            })
            .any(|class| class == NSWindow::class());
            if !is_ns_window_subclass
                || current_class.instance_size() != panel_class.instance_size()
            {
                eprintln!(
                    "cannot promote pet window to NSPanel (class={:?}, panel_size={}, window_size={})",
                    current_class.name(),
                    panel_class.instance_size(),
                    current_class.instance_size()
                );
                return None;
            }
            // SAFETY: Tauri's AppKit window subclasses do not add ivars to the
            // NSWindow layout, NSPanel is an AppKit NSWindow subclass, and
            // both runtime classes have equal instance sizes. No ivars or
            // layout are added by this class promotion.
            unsafe {
                AnyObject::set_class(native_window, panel_class);
            }
        }

        // SAFETY: The object was either already an NSPanel or was just
        // promoted to NSPanel above, and NSPanel has the same representation
        // as its NSWindow superclass.
        Some(unsafe { &*(native_window as *const NSWindow as *const NSPanel) })
    }

    pub(super) fn apply(
        window: &tauri::WebviewWindow,
        show_in_fullscreen: bool,
    ) -> Result<(), String> {
        window
            .with_webview(move |webview| unsafe {
                let native_window: &NSWindow = &*webview.ns_window().cast();
                let Some(panel) = as_panel(native_window) else {
                    return;
                };

                let fullscreen_behavior = NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::CanJoinAllApplications
                    | NSWindowCollectionBehavior::FullScreenAuxiliary
                    | NSWindowCollectionBehavior::Stationary
                    | NSWindowCollectionBehavior::IgnoresCycle;
                let mut behavior = panel.collectionBehavior();
                if show_in_fullscreen {
                    behavior |= fullscreen_behavior;
                } else {
                    behavior &= !fullscreen_behavior;
                }
                panel.setCollectionBehavior(behavior);

                let mut style_mask = panel.styleMask();
                if show_in_fullscreen {
                    style_mask |= NSWindowStyleMask::NonactivatingPanel;
                } else {
                    style_mask &= !NSWindowStyleMask::NonactivatingPanel;
                }
                panel.setStyleMask(style_mask);
                panel.setFloatingPanel(show_in_fullscreen);
                panel.setBecomesKeyOnlyIfNeeded(true);
                panel.setWorksWhenModal(true);
                panel.setHidesOnDeactivate(false);
                panel.setLevel(if show_in_fullscreen {
                    NSScreenSaverWindowLevel
                } else {
                    NSFloatingWindowLevel
                });
                if show_in_fullscreen && panel.isVisible() {
                    panel.orderFrontRegardless();
                }
            })
            .map_err(|error| format!("failed to configure macOS pet panel: {error}"))
    }
}

#[cfg(target_os = "windows")]
mod windows_overlay {
    use std::ffi::c_void;
    use windows::core::{IUnknown, Interface, GUID, HRESULT};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, IServiceProvider, CLSCTX_LOCAL_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE,
        SWP_NOMOVE, SWP_NOSIZE,
    };

    // Windows exposes the desktop manager for inspection and moving windows,
    // but the "show on all desktops" operation lives in this Explorer COM
    // service and is not part of the public Win32 SDK.
    windows::core::imp::define_interface!(
        IApplicationViewCollection,
        IApplicationViewCollection_Vtbl,
        0x1841C6D7_4F9D_42C0_AF41_8747538F10E5
    );
    windows::core::imp::define_interface!(
        IVirtualDesktopPinnedApps,
        IVirtualDesktopPinnedApps_Vtbl,
        0x4CE81583_1E4C_4632_A621_07A53543148F
    );

    #[repr(C)]
    #[allow(non_snake_case)]
    pub struct IApplicationViewCollection_Vtbl {
        base__: windows::core::IUnknown_Vtbl,
        GetViews: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
        GetViewsByZOrder: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
        GetViewsByAppUserModelId:
            unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> HRESULT,
        GetViewForHwnd: unsafe extern "system" fn(*mut c_void, HWND, *mut *mut c_void) -> HRESULT,
    }

    #[repr(C)]
    #[allow(non_snake_case)]
    pub struct IVirtualDesktopPinnedApps_Vtbl {
        base__: windows::core::IUnknown_Vtbl,
        IsAppIdPinned: unsafe extern "system" fn(*mut c_void, *const u16, *mut i32) -> HRESULT,
        PinAppID: unsafe extern "system" fn(*mut c_void, *const u16) -> HRESULT,
        UnpinAppID: unsafe extern "system" fn(*mut c_void, *const u16) -> HRESULT,
        IsViewPinned: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut i32) -> HRESULT,
        PinView: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
        UnpinView: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
    }

    const CLSID_IMMERSIVE_SHELL: GUID = GUID::from_u128(0xC2F03A33_21F5_47FA_B4BB_156362A2F239);
    const CLSID_VIRTUAL_DESKTOP_PINNED_APPS: GUID =
        GUID::from_u128(0xB5A399E7_1C87_46B8_88E9_FC5747B171BD);
    const RPC_E_CHANGED_MODE: HRESULT = HRESULT(0x80010106u32 as i32);

    impl IApplicationViewCollection {
        unsafe fn get_view_for_hwnd(&self, hwnd: HWND) -> windows::core::Result<Option<IUnknown>> {
            let mut raw_view = std::ptr::null_mut();
            (self.vtable().GetViewForHwnd)(self.as_raw(), hwnd, &mut raw_view).ok()?;
            Ok((!raw_view.is_null()).then(|| <IUnknown as Interface>::from_raw(raw_view)))
        }
    }

    impl IVirtualDesktopPinnedApps {
        unsafe fn is_view_pinned(&self, view: &IUnknown) -> windows::core::Result<bool> {
            let mut pinned = 0;
            (self.vtable().IsViewPinned)(self.as_raw(), view.as_raw(), &mut pinned).ok()?;
            Ok(pinned != 0)
        }

        unsafe fn pin_view(&self, view: &IUnknown) -> windows::core::Result<()> {
            (self.vtable().PinView)(self.as_raw(), view.as_raw()).ok()
        }

        unsafe fn unpin_view(&self, view: &IUnknown) -> windows::core::Result<()> {
            (self.vtable().UnpinView)(self.as_raw(), view.as_raw()).ok()
        }
    }

    pub(super) fn apply(
        window: &tauri::WebviewWindow,
        show_in_fullscreen: bool,
    ) -> Result<(), String> {
        let hwnd = window
            .hwnd()
            .map_err(|error| format!("failed to get Windows pet handle: {error}"))?;

        // Apply z-order from the fullscreen preference. The configured
        // window starts topmost so the pet can be shown immediately, but the
        // preference must be authoritative once the app state is restored.
        unsafe {
            SetWindowPos(
                hwnd,
                Some(if show_in_fullscreen {
                    HWND_TOPMOST
                } else {
                    HWND_NOTOPMOST
                }),
                0,
                0,
                0,
                0,
                SWP_ASYNCWINDOWPOS | SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        }
        .map_err(|error| format!("failed to set HWND_TOPMOST: {error}"))?;

        if let Err(error) = sync_virtual_desktop_pin(hwnd, show_in_fullscreen) {
            // Explorer's pinning service is undocumented and can be absent or
            // temporarily unavailable. The topmost window remains functional.
            eprintln!("Windows virtual desktop pinning unavailable: {error}");
        }

        Ok(())
    }

    pub(super) fn reassert(
        window: &tauri::WebviewWindow,
        show_in_fullscreen: bool,
    ) -> Result<(), String> {
        let hwnd = window
            .hwnd()
            .map_err(|error| format!("failed to get Windows pet handle: {error}"))?;
        unsafe {
            SetWindowPos(
                hwnd,
                Some(if show_in_fullscreen {
                    HWND_TOPMOST
                } else {
                    HWND_NOTOPMOST
                }),
                0,
                0,
                0,
                0,
                SWP_ASYNCWINDOWPOS | SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        }
        .map_err(|error| format!("failed to reassert pet z-order: {error}"))?;
        // Explorer may not have registered the view when the window is first
        // created. Retrying from the overlay guard lets the view become
        // pinnable without blocking the UI thread during window creation.
        let _ = sync_virtual_desktop_pin(hwnd, show_in_fullscreen);
        Ok(())
    }

    fn sync_virtual_desktop_pin(hwnd: HWND, should_pin: bool) -> windows::core::Result<()> {
        let com_status = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let should_uninitialize = com_status.is_ok();
        if com_status.is_err() && com_status != RPC_E_CHANGED_MODE {
            return Err(windows::core::Error::from(com_status));
        }

        let result = unsafe { sync_virtual_desktop_pin_inner(hwnd, should_pin) };
        if should_uninitialize {
            unsafe { CoUninitialize() };
        }
        result
    }

    unsafe fn sync_virtual_desktop_pin_inner(
        hwnd: HWND,
        should_pin: bool,
    ) -> windows::core::Result<()> {
        let shell: IServiceProvider =
            CoCreateInstance(&CLSID_IMMERSIVE_SHELL, None, CLSCTX_LOCAL_SERVER)?;
        let views: IApplicationViewCollection =
            shell.QueryService(&IApplicationViewCollection::IID)?;
        let pinned_apps: IVirtualDesktopPinnedApps =
            shell.QueryService(&CLSID_VIRTUAL_DESKTOP_PINNED_APPS)?;
        let Some(view) = views.get_view_for_hwnd(hwnd)? else {
            return Ok(());
        };

        let is_pinned = pinned_apps.is_view_pinned(&view)?;
        match (should_pin, is_pinned) {
            (true, false) => pinned_apps.pin_view(&view)?,
            (false, true) => pinned_apps.unpin_view(&view)?,
            _ => {}
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod macos_dock_menu {
    use super::show_pet_manager;
    use objc2::ffi;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Sel};
    use objc2::{sel, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
    use objc2_foundation::NSString;
    use std::ffi::CStr;
    use std::sync::OnceLock;

    static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

    extern "C-unwind" fn open_pet_manager_from_dock(
        _this: &AnyObject,
        _cmd: Sel,
        _sender: &AnyObject,
    ) {
        if let Some(app) = APP_HANDLE.get() {
            if let Err(error) = show_pet_manager(app) {
                eprintln!("failed to open pet manager from Dock menu: {error}");
            }
        }
    }

    extern "C-unwind" fn application_dock_menu(
        this: &AnyObject,
        _cmd: Sel,
        _sender: &NSApplication,
    ) -> *mut NSMenu {
        let Some(mtm) = MainThreadMarker::new() else {
            return std::ptr::null_mut();
        };
        let menu_title = NSString::from_str("SakiPet");
        let menu = NSMenu::initWithTitle(mtm.alloc(), &menu_title);
        let item_title = NSString::from_str("管理宠物");
        let empty_key = NSString::from_str("");
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &item_title,
                Some(sel!(sakiPetOpenManager:)),
                &empty_key,
            )
        };
        unsafe { item.setTarget(Some(this)) };
        menu.addItem(&item);
        Retained::autorelease_return(menu)
    }

    pub(super) fn install(app: &tauri::AppHandle) -> Result<(), String> {
        let _ = APP_HANDLE.set(app.clone());
        let Some(mtm) = MainThreadMarker::new() else {
            return Err("Dock 菜单必须在 macOS 主线程安装".to_string());
        };
        let ns_app = NSApplication::sharedApplication(mtm);
        let delegate = ns_app
            .delegate()
            .ok_or_else(|| "找不到 macOS 应用代理".to_string())?;
        let delegate_object: &AnyObject = delegate.as_ref();
        let class = delegate_object.class() as *const _ as *mut _;
        let encoding = unsafe { CStr::from_bytes_with_nul_unchecked(b"@@:@\0") };
        let dock_menu_added = unsafe {
            ffi::class_addMethod(
                class,
                sel!(applicationDockMenu:),
                std::mem::transmute::<
                    extern "C-unwind" fn(&AnyObject, Sel, &NSApplication) -> *mut NSMenu,
                    unsafe extern "C-unwind" fn(),
                >(application_dock_menu),
                encoding.as_ptr(),
            )
        };
        if !dock_menu_added.as_bool() {
            return Err("无法向 macOS 应用代理添加 Dock 菜单".to_string());
        }
        let action_added = unsafe {
            ffi::class_addMethod(
                class,
                sel!(sakiPetOpenManager:),
                std::mem::transmute::<
                    extern "C-unwind" fn(&AnyObject, Sel, &AnyObject),
                    unsafe extern "C-unwind" fn(),
                >(open_pet_manager_from_dock),
                encoding.as_ptr(),
            )
        };
        if !action_added.as_bool() {
            return Err("无法向 macOS 应用代理添加 Dock 菜单动作".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PetManifest {
    id: String,
    display_name: String,
    description: String,
    sprite_version_number: u8,
    spritesheet_path: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
struct PetSettings {
    scale: f64,
    opacity: f64,
    speed: f64,
    wander_enabled: bool,
    click_through: bool,
    lock_position: bool,
    quiet_mode: bool,
    show_in_fullscreen: bool,
    paused: bool,
}

impl Default for PetSettings {
    fn default() -> Self {
        Self {
            scale: 1.0,
            opacity: 1.0,
            speed: 95.0,
            wander_enabled: true,
            click_through: false,
            lock_position: false,
            quiet_mode: false,
            show_in_fullscreen: false,
            paused: false,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct PetDialogue {
    version: u8,
    double_click: Vec<String>,
    click: Vec<String>,
    right_click: Vec<String>,
    walk: Vec<String>,
    drag: Vec<String>,
    idle: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CharacterCard {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    personality: String,
    #[serde(default)]
    scenario: String,
    #[serde(rename = "first_mes", default)]
    first_mes: String,
    #[serde(rename = "mes_example", default)]
    mes_example: String,
    #[serde(rename = "creator_notes", default)]
    creator_notes: String,
    #[serde(rename = "system_prompt", default)]
    system_prompt: String,
    #[serde(rename = "post_history_instructions", default)]
    post_history_instructions: String,
    #[serde(rename = "alternate_greetings", default)]
    alternate_greetings: Vec<String>,
    #[serde(rename = "character_book", default)]
    character_book: Option<CharacterBook>,
    #[serde(default)]
    extensions: serde_json::Value,
    #[serde(default)]
    dialogue: PetDialogue,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
struct CharacterBook {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    scan_depth: usize,
    #[serde(default)]
    token_budget: usize,
    #[serde(default)]
    recursive_scanning: bool,
    #[serde(default)]
    extensions: serde_json::Value,
    #[serde(default)]
    entries: Vec<CharacterBookEntry>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
struct CharacterBookEntry {
    #[serde(default)]
    keys: Vec<String>,
    #[serde(default)]
    secondary_keys: Vec<String>,
    #[serde(default)]
    content: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    insertion_order: i64,
    #[serde(default)]
    case_sensitive: bool,
    #[serde(default)]
    selective: bool,
    #[serde(default)]
    constant: bool,
    #[serde(default)]
    priority: i64,
    #[serde(default)]
    position: String,
    #[serde(default)]
    extensions: serde_json::Value,
}

fn default_true() -> bool {
    true
}

impl CharacterCard {
    fn relevant_lorebook(&self, query: &str) -> String {
        let Some(book) = &self.character_book else {
            return String::new();
        };
        let query = query.to_lowercase();
        let mut entries: Vec<&CharacterBookEntry> = book
            .entries
            .iter()
            .filter(|entry| {
                if !entry.enabled || entry.content.trim().is_empty() {
                    return false;
                }
                if entry.constant {
                    return true;
                }
                let matches_key = |key: &str| {
                    if entry.case_sensitive {
                        query.contains(key)
                    } else {
                        query.contains(&key.to_lowercase())
                    }
                };
                let primary = entry.keys.iter().any(|key| matches_key(key));
                let secondary = entry.secondary_keys.iter().any(|key| matches_key(key));
                primary && (!entry.selective || secondary)
            })
            .collect();
        entries.sort_by_key(|entry| (entry.priority, entry.insertion_order));
        let budget = if book.token_budget == 0 {
            1_200
        } else {
            book.token_budget * 4
        };
        let mut output = String::new();
        for entry in entries {
            if output.len() + entry.content.len() > budget {
                break;
            }
            output.push_str(&entry.content);
            output.push('\n');
        }
        output
    }
}

impl Default for PetDialogue {
    fn default() -> Self {
        Self {
            version: 1,
            double_click: vec!["嗯？找我吗？".to_string(), "今天也一起玩吧。".to_string()],
            click: vec!["怎么啦？".to_string(), "我在这里哦。".to_string()],
            right_click: vec!["轻一点嘛。".to_string()],
            walk: vec!["我去附近转转。".to_string(), "散步时间到了！".to_string()],
            drag: vec!["要带我去哪里呀？".to_string(), "我来啦！".to_string()],
            idle: vec![
                "这里待着也很舒服。".to_string(),
                "要不要陪我说说话？".to_string(),
            ],
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct PetPosition {
    x: f64,
    y: f64,
}

#[derive(Clone, Serialize, Deserialize)]
struct PetInstanceConfig {
    id: String,
    pet_id: String,
    visible: bool,
    position: Option<PetPosition>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct AppConfig {
    // Kept for migrating configurations created before per-pet settings existed.
    settings: PetSettings,
    #[serde(default)]
    pet_settings: HashMap<String, PetSettings>,
    instances: Vec<PetInstanceConfig>,
    disabled_pet_ids: Vec<String>,
    next_instance_id: u64,
    #[serde(default)]
    ai: AiSettings,
    #[serde(default)]
    chat_positions: HashMap<String, PetPosition>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            settings: PetSettings::default(),
            pet_settings: HashMap::new(),
            instances: vec![PetInstanceConfig {
                id: "main".to_string(),
                pet_id: "sakimiao".to_string(),
                visible: true,
                position: None,
            }],
            disabled_pet_ids: Vec::new(),
            next_instance_id: 2,
            ai: AiSettings::default(),
            chat_positions: HashMap::new(),
        }
    }
}

struct AppState {
    config: Mutex<AppConfig>,
    ai: AiRuntime,
    ready: AtomicBool,
}

impl AppState {
    fn new() -> Self {
        Self {
            config: Mutex::new(AppConfig::default()),
            ai: AiRuntime::default(),
            ready: AtomicBool::new(false),
        }
    }
}

#[derive(Clone)]
struct BundledPet {
    id: String,
    path: String,
    manifest: PetManifest,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstalledPet {
    id: String,
    display_name: String,
    description: String,
    sprite_version_number: u8,
    spritesheet_path: String,
    source: String,
    enabled: bool,
    preview_data_url: Option<String>,
    path: Option<String>,
    settings: PetSettings,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PetInstanceInfo {
    id: String,
    pet_id: String,
    visible: bool,
    is_main: bool,
    position: Option<PetPosition>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeConfig {
    instance_id: String,
    pet_id: String,
    source: String,
    path: Option<String>,
    manifest: Option<PetManifest>,
    spritesheet_data_url: Option<String>,
    settings: PetSettings,
    dialogue: PetDialogue,
    character: CharacterCard,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PetSettingsEvent {
    pet_id: String,
    settings: PetSettings,
}

fn is_safe_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
}

fn is_safe_relative_path(value: &str) -> bool {
    !value.is_empty()
        && !Path::new(value).is_absolute()
        && !value
            .split('/')
            .any(|part| part.is_empty() || part == ".." || part == ".")
}

/// Discover bundled pets from their own `pet.json` records. The frontend
/// assets are copied into the Tauri resource directory for packaged builds;
/// the source directory is the fallback used by `tauri dev`.
fn bundled_pet_roots(app: &tauri::AppHandle) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        roots.push(resource_dir.join("pets"));
    }
    roots.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../public/pets")
            .to_path_buf(),
    );
    let mut unique = HashSet::new();
    roots.retain(|root| unique.insert(root.clone()));
    roots
}

fn discover_bundled_pets(app: &tauri::AppHandle) -> Vec<BundledPet> {
    let mut pets = Vec::new();
    let mut seen_ids = HashSet::new();
    for root in bundled_pet_roots(app) {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !is_safe_id(id) {
                continue;
            }
            let Ok(manifest) = serde_json::from_slice::<PetManifest>(
                &fs::read(path.join("pet.json")).unwrap_or_default(),
            ) else {
                continue;
            };
            if manifest.id != id
                || manifest.sprite_version_number != 2
                || !is_safe_relative_path(&manifest.spritesheet_path)
                || !path.join(&manifest.spritesheet_path).is_file()
                || !seen_ids.insert(id.to_string())
            {
                continue;
            }
            pets.push(BundledPet {
                id: id.to_string(),
                path: id.to_string(),
                manifest,
            });
        }
    }
    pets.sort_by(|left, right| left.id.cmp(&right.id));
    pets
}

fn config_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|directory| directory.join(CONFIG_FILE_NAME))
        .map_err(|error| format!("failed to locate app config directory: {error}"))
}

fn copy_directory_contents(source: &Path, target: &Path) -> Result<(), String> {
    if !source.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(target)
        .map_err(|error| format!("failed to create migrated data directory: {error}"))?;
    for entry in fs::read_dir(source)
        .map_err(|error| format!("failed to read legacy data directory: {error}"))?
    {
        let entry = entry.map_err(|error| format!("failed to inspect legacy data: {error}"))?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to inspect legacy data entry: {error}"))?;
        if file_type.is_dir() {
            copy_directory_contents(&source_path, &target_path)?;
        } else if file_type.is_file() && !target_path.exists() {
            fs::copy(&source_path, &target_path)
                .map_err(|error| format!("failed to migrate legacy data: {error}"))?;
        }
    }
    Ok(())
}

fn find_legacy_config_dir(new_config_dir: &Path) -> Option<PathBuf> {
    let parent = new_config_dir.parent()?;
    let entries = fs::read_dir(parent).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path();
        if candidate == new_config_dir || !candidate.is_dir() {
            continue;
        }
        let config_path = candidate.join(CONFIG_FILE_NAME);
        let Ok(bytes) = fs::read(config_path) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        // Requiring the instances field avoids treating an unrelated app's
        // JSON settings as SakiPet data during the identifier migration.
        if value.get("instances").is_none() || serde_json::from_value::<AppConfig>(value).is_err() {
            continue;
        }
        return Some(candidate);
    }
    None
}

/// Migrate data from the pre-public-release identifier without embedding the
/// old personal identifier in the source or future binaries. The legacy
/// directory is recognized by the SakiPet config shape instead.
fn migrate_legacy_app_data(app: &tauri::AppHandle) -> Result<(), String> {
    let new_config_dir = app
        .path()
        .app_config_dir()
        .map_err(|error| format!("failed to locate app config directory: {error}"))?;
    let new_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("failed to locate app data directory: {error}"))?;
    let Some(legacy_config_dir) = find_legacy_config_dir(&new_config_dir) else {
        return Ok(());
    };

    let legacy_config = legacy_config_dir.join(CONFIG_FILE_NAME);
    let new_config = new_config_dir.join(CONFIG_FILE_NAME);
    if !new_config.exists() {
        fs::create_dir_all(&new_config_dir)
            .map_err(|error| format!("failed to create app config directory: {error}"))?;
        fs::copy(&legacy_config, &new_config)
            .map_err(|error| format!("failed to migrate app config: {error}"))?;
    }

    if let (Some(legacy_name), Some(data_parent)) =
        (legacy_config_dir.file_name(), new_data_dir.parent())
    {
        let legacy_data_dir = data_parent.join(legacy_name);
        copy_directory_contents(&legacy_data_dir, &new_data_dir)?;
    }
    Ok(())
}

fn imported_pets_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|directory| directory.join("pets"))
        .map_err(|error| format!("failed to locate app data directory: {error}"))
}

fn load_config(app: &tauri::AppHandle) -> AppConfig {
    let Ok(path) = config_path(app) else {
        return AppConfig::default();
    };
    let Ok(bytes) = fs::read(path) else {
        return AppConfig::default();
    };
    let mut config = serde_json::from_slice::<AppConfig>(&bytes).unwrap_or_default();
    normalize_config(&mut config);
    config
}

fn normalize_config(config: &mut AppConfig) {
    if !config
        .instances
        .iter()
        .any(|instance| instance.id == "main")
    {
        config.instances.insert(
            0,
            PetInstanceConfig {
                id: "main".to_string(),
                pet_id: "sakimiao".to_string(),
                visible: true,
                position: None,
            },
        );
    }
    config
        .instances
        .retain(|instance| is_safe_id(&instance.id) && is_safe_id(&instance.pet_id));
    let mut seen_pet_ids = HashSet::new();
    config
        .instances
        .retain(|instance| seen_pet_ids.insert(instance.pet_id.clone()));
    config.disabled_pet_ids.retain(|id| is_safe_id(id));
    config.settings = clamp_settings(config.settings.clone());
    config.pet_settings.retain(|id, _| is_safe_id(id));
    config
        .chat_positions
        .retain(|id, position| is_safe_id(id) && position.x.is_finite() && position.y.is_finite());
    for settings in config.pet_settings.values_mut() {
        *settings = clamp_settings(settings.clone());
    }
    let instance_pet_ids: Vec<String> = config
        .instances
        .iter()
        .map(|instance| instance.pet_id.clone())
        .collect();
    for pet_id in instance_pet_ids {
        let legacy_settings = config.settings.clone();
        config.pet_settings.entry(pet_id).or_insert(legacy_settings);
    }
}

fn save_config(app: &tauri::AppHandle, config: &AppConfig) -> Result<(), String> {
    let path = config_path(app)?;
    let directory = path
        .parent()
        .ok_or_else(|| "invalid app config path".to_string())?;
    fs::create_dir_all(directory)
        .map_err(|error| format!("failed to create config directory: {error}"))?;
    let temp_path = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(config)
        .map_err(|error| format!("failed to encode config: {error}"))?;
    fs::write(&temp_path, bytes).map_err(|error| format!("failed to write config: {error}"))?;
    #[cfg(target_os = "windows")]
    if path.exists() {
        fs::remove_file(&path).map_err(|error| format!("failed to replace config: {error}"))?;
    }
    fs::rename(&temp_path, &path).map_err(|error| format!("failed to replace config: {error}"))
}

fn config_snapshot(app: &tauri::AppHandle) -> Result<AppConfig, String> {
    app.state::<AppState>()
        .config
        .lock()
        .map(|config| config.clone())
        .map_err(|_| "app config lock is poisoned".to_string())
}

#[tauri::command]
fn is_app_ready(app: tauri::AppHandle) -> bool {
    app.try_state::<AppState>()
        .map(|state| state.ready.load(Ordering::Acquire))
        .unwrap_or(false)
}

fn update_config<F>(app: &tauri::AppHandle, update: F) -> Result<AppConfig, String>
where
    F: FnOnce(&mut AppConfig) -> Result<(), String>,
{
    let snapshot = {
        let state = app.state::<AppState>();
        let mut config = state
            .config
            .lock()
            .map_err(|_| "app config lock is poisoned".to_string())?;
        update(&mut config)?;
        normalize_config(&mut config);
        config.clone()
    };
    save_config(app, &snapshot)?;
    Ok(snapshot)
}

fn clamp_settings(mut settings: PetSettings) -> PetSettings {
    settings.scale = settings.scale.clamp(0.5, 2.5);
    settings.opacity = settings.opacity.clamp(0.2, 1.0);
    settings.speed = settings.speed.clamp(30.0, 240.0);
    settings
}

fn settings_for_pet(config: &AppConfig, pet_id: &str) -> PetSettings {
    config.pet_settings.get(pet_id).cloned().unwrap_or_default()
}

fn instance_label(instance_id: &str) -> Result<String, String> {
    if instance_id == "main" {
        return Ok("main".to_string());
    }
    if !is_safe_id(instance_id) {
        return Err("invalid pet instance id".to_string());
    }
    Ok(format!("pet-instance-{instance_id}"))
}

fn instance_id_from_label(label: &str) -> Option<String> {
    if label == "main" {
        Some("main".to_string())
    } else {
        label.strip_prefix("pet-instance-").map(str::to_string)
    }
}

fn pet_is_bundled(app: &tauri::AppHandle, pet_id: &str) -> Option<BundledPet> {
    discover_bundled_pets(app)
        .into_iter()
        .find(|pet| pet.id == pet_id)
}

fn pet_is_imported(app: &tauri::AppHandle, pet_id: &str) -> Option<(PetManifest, Vec<u8>)> {
    if !is_safe_id(pet_id) {
        return None;
    }
    let root = imported_pets_path(app).ok()?.join(pet_id);
    let manifest_path = root.join("pet.json");
    let manifest = serde_json::from_slice::<PetManifest>(&fs::read(manifest_path).ok()?).ok()?;
    if manifest.id != pet_id
        || manifest.sprite_version_number != 2
        || !is_safe_relative_path(&manifest.spritesheet_path)
    {
        return None;
    }
    let sprite = fs::read(root.join(&manifest.spritesheet_path)).ok()?;
    Some((manifest, sprite))
}

fn pet_exists(app: &tauri::AppHandle, pet_id: &str) -> bool {
    pet_is_bundled(app, pet_id).is_some() || pet_is_imported(app, pet_id).is_some()
}

fn mime_for_path(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        _ => "image/webp",
    }
}

fn data_url(path: &str, bytes: &[u8]) -> String {
    format!(
        "data:{};base64,{}",
        mime_for_path(path),
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

fn normalize_dialogue(mut dialogue: PetDialogue) -> PetDialogue {
    let normalize_lines = |lines: Vec<String>| {
        lines
            .into_iter()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .take(32)
            .collect::<Vec<_>>()
    };
    dialogue.double_click = normalize_lines(dialogue.double_click);
    dialogue.click = normalize_lines(dialogue.click);
    dialogue.right_click = normalize_lines(dialogue.right_click);
    dialogue.walk = normalize_lines(dialogue.walk);
    dialogue.drag = normalize_lines(dialogue.drag);
    dialogue.idle = normalize_lines(dialogue.idle);
    if dialogue.double_click.is_empty() {
        dialogue.double_click = PetDialogue::default().double_click;
    }
    dialogue
}

fn decode_character(bytes: &[u8]) -> Result<CharacterCard, String> {
    if bytes.len() > 1024 * 1024 {
        return Err("character.json 不能超过 1 MB".to_string());
    }
    let value = serde_json::from_slice::<serde_json::Value>(bytes)
        .map_err(|error| format!("character.json 格式错误: {error}"))?;
    let is_v2 = value.get("spec").and_then(serde_json::Value::as_str) == Some("chara_card_v2")
        && value.get("data").is_some();
    let mut card = if is_v2 {
        let data = value.get("data").cloned().unwrap_or_default();
        let extensions = data
            .get("extensions")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let dialogue = extensions
            .get("sakipet")
            .and_then(|value| value.get("dialogue"))
            .cloned()
            .unwrap_or_else(|| serde_json::json!({ "version": 1 }));
        let mut card = serde_json::from_value::<CharacterCard>(data)
            .map_err(|error| format!("character.json V2 格式错误: {error}"))?;
        card.extensions = extensions;
        card.dialogue = serde_json::from_value(dialogue)
            .map_err(|error| format!("character.json 静态台词格式错误: {error}"))?;
        card
    } else {
        let dialogue = serde_json::from_value::<PetDialogue>(value.clone())
            .map_err(|error| format!("character.json V1 格式错误: {error}"))?;
        CharacterCard {
            dialogue,
            ..CharacterCard::default()
        }
    };
    let dialogue = normalize_dialogue(card.dialogue.clone());
    let too_long = [
        &dialogue.double_click,
        &dialogue.click,
        &dialogue.right_click,
        &dialogue.walk,
        &dialogue.drag,
        &dialogue.idle,
    ]
    .into_iter()
    .any(|lines| lines.iter().any(|line| line.chars().count() > 240));
    if too_long {
        return Err("character.json 中的单句台词不能超过 240 个字符".to_string());
    }
    card.dialogue = dialogue;
    Ok(card)
}

fn decode_dialogue(bytes: &[u8]) -> Result<PetDialogue, String> {
    Ok(decode_character(bytes)?.dialogue)
}

fn bundled_character_bytes(app: &tauri::AppHandle, pet_id: &str) -> Option<Vec<u8>> {
    let bundled = pet_is_bundled(app, pet_id)?;
    bundled_pet_roots(app)
        .into_iter()
        .map(|root| root.join(&bundled.path).join("character.json"))
        .find_map(|path| fs::read(path).ok())
}

fn load_pet_character(app: &tauri::AppHandle, pet_id: &str) -> Result<CharacterCard, String> {
    let bytes = if let Some(root) = imported_pets_path(app).ok().map(|path| path.join(pet_id)) {
        fs::read(root.join("character.json")).ok()
    } else {
        None
    };
    let bytes = bytes.or_else(|| bundled_character_bytes(app, pet_id));
    Ok(bytes
        .and_then(|bytes| decode_character(&bytes).ok())
        .unwrap_or_default())
}

fn installed_pets(app: &tauri::AppHandle, config: &AppConfig) -> Vec<InstalledPet> {
    let disabled: HashSet<&str> = config.disabled_pet_ids.iter().map(String::as_str).collect();
    let mut result: Vec<InstalledPet> = discover_bundled_pets(app)
        .into_iter()
        .map(|pet| {
            let id = pet.id;
            let manifest = pet.manifest;
            InstalledPet {
                id: id.clone(),
                display_name: manifest.display_name,
                description: manifest.description,
                sprite_version_number: manifest.sprite_version_number,
                spritesheet_path: manifest.spritesheet_path,
                source: "bundled".to_string(),
                enabled: !disabled.contains(id.as_str()),
                preview_data_url: None,
                path: Some(pet.path),
                settings: settings_for_pet(config, &id),
            }
        })
        .collect();
    let Ok(root) = imported_pets_path(app) else {
        return result;
    };
    let Ok(entries) = fs::read_dir(root) else {
        return result;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some((manifest, sprite)) = pet_is_imported(app, id) else {
            continue;
        };
        result.push(InstalledPet {
            id: manifest.id.clone(),
            display_name: manifest.display_name,
            description: manifest.description,
            sprite_version_number: manifest.sprite_version_number,
            spritesheet_path: manifest.spritesheet_path.clone(),
            source: "imported".to_string(),
            enabled: !disabled.contains(id),
            preview_data_url: Some(data_url(&manifest.spritesheet_path, &sprite)),
            path: None,
            settings: settings_for_pet(config, id),
        });
    }
    result.sort_by(|left, right| left.id.cmp(&right.id));
    result
}

fn pet_display_name(app: &tauri::AppHandle, pet_id: &str) -> String {
    installed_pets(app, &config_snapshot(app).unwrap_or_default())
        .into_iter()
        .find(|pet| pet.id == pet_id)
        .map(|pet| {
            if pet.display_name.is_empty() {
                pet.id
            } else {
                pet.display_name
            }
        })
        .unwrap_or_else(|| pet_id.to_string())
}

fn apply_window_settings(
    window: &tauri::WebviewWindow,
    settings: &PetSettings,
) -> Result<(), String> {
    window
        .set_size(LogicalSize::new(
            PET_WIDTH * settings.scale,
            PET_HEIGHT * settings.scale,
        ))
        .map_err(|error| format!("failed to set pet size: {error}"))?;
    window
        .set_ignore_cursor_events(settings.click_through)
        .map_err(|error| format!("failed to set click-through mode: {error}"))?;
    apply_fullscreen_visibility(window, settings.show_in_fullscreen)
}

fn apply_fullscreen_visibility(
    window: &tauri::WebviewWindow,
    show_in_fullscreen: bool,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    windows_overlay::apply(window, show_in_fullscreen)?;

    #[cfg(not(target_os = "windows"))]
    window
        .set_visible_on_all_workspaces(show_in_fullscreen)
        .map_err(|error| format!("failed to set workspace visibility: {error}"))?;

    #[cfg(target_os = "macos")]
    macos_overlay::apply(window, show_in_fullscreen)?;

    Ok(())
}

fn sync_macos_activation_policy(app: &tauri::AppHandle, config: &AppConfig) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        // macOS only allows a non-activating overlay to reliably join another
        // app's native fullscreen Space when the owning app is an accessory.
        // SakiPet intentionally uses the menu bar tray as its app entry point;
        // this keeps the pet visible in fullscreen while avoiding a Dock item.
        let _ = config;
        app.set_activation_policy(tauri::ActivationPolicy::Accessory)
            .map_err(|error| format!("failed to set macOS activation policy: {error}"))?;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, config);
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn reassert_fullscreen_overlay(
    window: &tauri::WebviewWindow,
    show_in_fullscreen: bool,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    windows_overlay::reassert(window, show_in_fullscreen)?;

    #[cfg(target_os = "macos")]
    {
        window
            .set_visible_on_all_workspaces(true)
            .map_err(|error| format!("failed to refresh macOS workspace visibility: {error}"))?;
    }

    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn start_fullscreen_overlay_guard(app: &tauri::AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let Ok(config) = config_snapshot(&app) else {
            continue;
        };
        let mut overlay_windows = Vec::new();
        for instance in config.instances.iter().filter(|instance| instance.visible) {
            let settings = settings_for_pet(&config, &instance.pet_id);
            if !settings.show_in_fullscreen
                || config
                    .disabled_pet_ids
                    .iter()
                    .any(|id| id == &instance.pet_id)
            {
                continue;
            }
            let Ok(label) = instance_label(&instance.id) else {
                continue;
            };
            let Some(window) = app.get_webview_window(&label) else {
                continue;
            };
            overlay_windows.push(window);
        }
        if !overlay_windows.is_empty() {
            for window in overlay_windows {
                if window.is_visible().unwrap_or(false) {
                    if let Err(error) = reassert_fullscreen_overlay(&window, true) {
                        eprintln!("failed to reassert fullscreen pet overlay: {error}");
                    }
                }
            }
        }
    });
}

fn is_pet_window_label(label: &str) -> bool {
    label == "main" || label.starts_with("pet-instance-")
}

fn pet_window_position(app: &tauri::AppHandle, config: &AppConfig) -> (f64, f64) {
    let offset = config
        .instances
        .iter()
        .filter(|instance| instance.id != "main" && instance.position.is_none())
        .count() as f64;
    if let Some(main) = app.get_webview_window("main") {
        if let (Ok(position), Ok(scale_factor)) = (main.outer_position(), main.scale_factor()) {
            return (
                position.x as f64 / scale_factor + 220.0 * (offset + 1.0),
                position.y as f64 / scale_factor,
            );
        }
    }
    (900.0 + 220.0 * offset, 240.0)
}

fn create_pet_window(
    app: &tauri::AppHandle,
    instance: &PetInstanceConfig,
    config: &AppConfig,
) -> Result<(), String> {
    if instance.id == "main" || !pet_exists(app, &instance.pet_id) {
        return Ok(());
    }
    let label = instance_label(&instance.id)?;
    if app.get_webview_window(&label).is_some() {
        return Ok(());
    }
    let (x, y) = instance
        .position
        .as_ref()
        .map(|position| (position.x, position.y))
        .unwrap_or_else(|| pet_window_position(app, config));
    let pet_settings = settings_for_pet(config, &instance.pet_id);
    sync_macos_activation_policy(app, config)?;
    let window = WebviewWindowBuilder::new(
        app,
        &label,
        WebviewUrl::App(format!("index.html?instance={}", instance.id).into()),
    )
    .title("SakiPet")
    .additional_browser_args(BROWSER_ARGS)
    .inner_size(
        PET_WIDTH * pet_settings.scale,
        PET_HEIGHT * pet_settings.scale,
    )
    .position(x, y)
    .transparent(true)
    .decorations(false)
    .always_on_top(true)
    .accept_first_mouse(true)
    .skip_taskbar(true)
    .shadow(false)
    .resizable(false)
    .focused(false)
    .visible(false)
    .build()
    .map_err(|error| format!("failed to create pet window: {error}"))?;
    apply_window_settings(&window, &pet_settings)?;
    if instance.visible
        && !config
            .disabled_pet_ids
            .iter()
            .any(|id| id == &instance.pet_id)
    {
        window
            .show()
            .map_err(|error| format!("failed to show pet window: {error}"))?;
        apply_fullscreen_visibility(&window, pet_settings.show_in_fullscreen)?;
    }
    Ok(())
}

fn visible_instances(app: &tauri::AppHandle, config: &AppConfig) -> Vec<PetInstanceInfo> {
    config
        .instances
        .iter()
        .filter(|instance| {
            instance.visible
                && !config
                    .disabled_pet_ids
                    .iter()
                    .any(|id| id == &instance.pet_id)
                && app
                    .get_webview_window(&instance_label(&instance.id).unwrap_or_default())
                    .and_then(|window| window.is_visible().ok())
                    .unwrap_or(false)
        })
        .map(|instance| PetInstanceInfo {
            id: instance.id.clone(),
            pet_id: instance.pet_id.clone(),
            visible: true,
            is_main: instance.id == "main",
            position: app
                .get_webview_window(&instance_label(&instance.id).unwrap_or_default())
                .and_then(|window| {
                    let position = window.outer_position().ok()?;
                    let scale_factor = window.scale_factor().ok()?.max(1.0);
                    Some(PetPosition {
                        x: position.x as f64 / scale_factor,
                        y: position.y as f64 / scale_factor,
                    })
                }),
        })
        .collect()
}

fn has_visible_pet_config(app: &tauri::AppHandle, config: &AppConfig) -> bool {
    config.instances.iter().any(|instance| {
        instance.visible
            && !config
                .disabled_pet_ids
                .iter()
                .any(|id| id == &instance.pet_id)
            && pet_exists(app, &instance.pet_id)
    })
}

fn all_instance_info(config: &AppConfig) -> Vec<PetInstanceInfo> {
    config
        .instances
        .iter()
        .map(|instance| PetInstanceInfo {
            id: instance.id.clone(),
            pet_id: instance.pet_id.clone(),
            visible: instance.visible,
            is_main: instance.id == "main",
            position: None,
        })
        .collect()
}

fn show_pet_manager(app: &tauri::AppHandle) -> Result<(), String> {
    let Some(window) = app.get_webview_window("pet-manager") else {
        return Err("pet manager window is not available".to_string());
    };
    // show() alone cannot restore a minimized window on Windows.
    if window.is_minimized().unwrap_or(false) {
        let _ = window.unminimize();
    }
    window
        .show()
        .map_err(|error| format!("failed to show pet manager: {error}"))?;
    window
        .set_focus()
        .map_err(|error| format!("failed to focus pet manager: {error}"))?;
    Ok(())
}

fn show_ai_settings(app: &tauri::AppHandle) -> Result<(), String> {
    let Some(window) = app.get_webview_window("ai-settings") else {
        return Err("AI settings window is not available".to_string());
    };
    // show() alone cannot restore a minimized window on Windows.
    if window.is_minimized().unwrap_or(false) {
        let _ = window.unminimize();
    }
    window
        .show()
        .map_err(|error| format!("failed to show AI settings: {error}"))?;
    window
        .set_focus()
        .map_err(|error| format!("failed to focus AI settings: {error}"))?;
    Ok(())
}

fn chat_window_label(pet_id: &str) -> Result<String, String> {
    if !is_safe_id(pet_id) {
        return Err("invalid pet id".to_string());
    }
    Ok(format!("pet-chat-{pet_id}"))
}

#[tauri::command]
async fn open_pet_chat(app: tauri::AppHandle, pet_id: String) -> Result<(), String> {
    if !pet_exists(&app, &pet_id) {
        return Err("宠物资源不存在或校验失败".to_string());
    }
    let label = chat_window_label(&pet_id)?;
    if let Some(window) = app.get_webview_window(&label) {
        // show() alone cannot restore a minimized window on Windows.
        if window.is_minimized().unwrap_or(false) {
            let _ = window.unminimize();
        }
        window.show().map_err(|error| error.to_string())?;
        window.set_focus().map_err(|error| error.to_string())?;
        return Ok(());
    }
    let config = config_snapshot(&app)?;
    let position = config
        .chat_positions
        .get(&pet_id)
        .map(|position| (position.x, position.y))
        .or_else(|| {
            config
                .instances
                .iter()
                .find(|instance| instance.pet_id == pet_id)
                .and_then(|instance| instance.position.clone())
                .map(|position| (position.x - 114.0, position.y + PET_HEIGHT + 12.0))
        })
        .unwrap_or((240.0, 390.0));
    let window = WebviewWindowBuilder::new(
        &app,
        &label,
        WebviewUrl::App(format!("chat.html?petId={pet_id}").into()),
    )
    .title("")
    .inner_size(320.0, 230.0)
    .position(position.0, position.1)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(false)
    .resizable(false)
    .additional_browser_args(BROWSER_ARGS)
    // The window is borderless, so it has no native title-bar buttons. The
    // page closes it through `hide_pet_chat`.
    .visible(true)
    .build()
    .map_err(|error| format!("failed to create pet chat window: {error}"))?;
    // Close/hide and position persistence for this window are handled by the
    // global `on_window_event` hook; per-window listeners registered after
    // creation never fire.
    window.set_focus().map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
fn hide_pet_chat(app: tauri::AppHandle, pet_id: String) -> Result<(), String> {
    if !is_safe_id(&pet_id) {
        return Err("invalid pet id".to_string());
    }
    let window = app
        .get_webview_window(&chat_window_label(&pet_id)?)
        .ok_or_else(|| "chat window is not available".to_string())?;
    // Persist the closing position so the next open restores it.
    if let (Ok(position), Ok(scale_factor)) = (window.outer_position(), window.scale_factor()) {
        let logical = PetPosition {
            x: position.x as f64 / scale_factor,
            y: position.y as f64 / scale_factor,
        };
        let _ = update_config(&app, |config| {
            config.chat_positions.insert(pet_id.clone(), logical);
            Ok(())
        });
    }
    window.hide().map_err(|error| error.to_string())
}

#[tauri::command]
fn open_pet_manager(app: tauri::AppHandle) -> Result<(), String> {
    show_pet_manager(&app)
}

#[tauri::command]
fn open_ai_settings(app: tauri::AppHandle) -> Result<(), String> {
    show_ai_settings(&app)
}

#[tauri::command]
fn get_pet_catalog(app: tauri::AppHandle) -> Result<Vec<InstalledPet>, String> {
    Ok(installed_pets(&app, &config_snapshot(&app)?))
}

#[tauri::command]
fn get_pet_settings(app: tauri::AppHandle, pet_id: String) -> Result<PetSettings, String> {
    if !pet_exists(&app, &pet_id) {
        return Err("宠物资源不存在或校验失败".to_string());
    }
    Ok(settings_for_pet(&config_snapshot(&app)?, &pet_id))
}

#[tauri::command]
fn get_pet_instances(app: tauri::AppHandle) -> Result<Vec<PetInstanceInfo>, String> {
    Ok(all_instance_info(&config_snapshot(&app)?))
}

#[tauri::command]
fn get_visible_pets(app: tauri::AppHandle) -> Result<Vec<PetInstanceInfo>, String> {
    let config = config_snapshot(&app)?;
    Ok(visible_instances(&app, &config))
}

#[tauri::command]
fn get_runtime_config(
    app: tauri::AppHandle,
    window_label: String,
) -> Result<RuntimeConfig, String> {
    let instance_id = instance_id_from_label(&window_label)
        .ok_or_else(|| "invalid pet window label".to_string())?;
    let config = config_snapshot(&app)?;
    let instance = config
        .instances
        .iter()
        .find(|instance| instance.id == instance_id)
        .ok_or_else(|| "pet instance is not configured".to_string())?;
    if let Some((manifest, sprite)) = pet_is_imported(&app, &instance.pet_id) {
        let character = load_pet_character(&app, &instance.pet_id).unwrap_or_default();
        return Ok(RuntimeConfig {
            instance_id,
            pet_id: instance.pet_id.clone(),
            source: "imported".to_string(),
            path: None,
            manifest: Some(manifest.clone()),
            spritesheet_data_url: Some(data_url(&manifest.spritesheet_path, &sprite)),
            settings: settings_for_pet(&config, &instance.pet_id),
            dialogue: character.dialogue.clone(),
            character,
        });
    }
    let bundled = pet_is_bundled(&app, &instance.pet_id)
        .ok_or_else(|| "pet resource is not installed".to_string())?;
    let character = load_pet_character(&app, &instance.pet_id).unwrap_or_default();
    Ok(RuntimeConfig {
        instance_id,
        pet_id: instance.pet_id.clone(),
        source: "bundled".to_string(),
        path: Some(bundled.path),
        manifest: None,
        spritesheet_data_url: None,
        settings: settings_for_pet(&config, &instance.pet_id),
        dialogue: character.dialogue.clone(),
        character,
    })
}

fn set_instance_visible_internal(
    app: &tauri::AppHandle,
    instance_id: &str,
    visible: bool,
) -> Result<(), String> {
    let config = update_config(app, |config| {
        let instance = config
            .instances
            .iter_mut()
            .find(|instance| instance.id == instance_id)
            .ok_or_else(|| "pet instance is not configured".to_string())?;
        instance.visible = visible;
        Ok(())
    })?;
    sync_macos_activation_policy(app, &config)?;
    let instance = config
        .instances
        .iter()
        .find(|instance| instance.id == instance_id)
        .ok_or_else(|| "pet instance is not configured".to_string())?;
    let window = app
        .get_webview_window(&instance_label(instance_id)?)
        .ok_or_else(|| "pet window is not available".to_string())?;
    if visible
        && !config
            .disabled_pet_ids
            .iter()
            .any(|id| id == &instance.pet_id)
    {
        window.show().map_err(|error| error.to_string())?;
        apply_fullscreen_visibility(
            &window,
            settings_for_pet(&config, &instance.pet_id).show_in_fullscreen,
        )?;
    } else {
        window.hide().map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn set_pet_instance_visible(
    app: tauri::AppHandle,
    instance_id: String,
    visible: bool,
) -> Result<Vec<PetInstanceInfo>, String> {
    set_instance_visible_internal(&app, &instance_id, visible)?;
    rebuild_tray_menu(&app).map_err(|error| error.to_string())?;
    get_pet_instances(app)
}

fn add_pet_instance_internal(
    app: &tauri::AppHandle,
    pet_id: &str,
) -> Result<PetInstanceInfo, String> {
    if !pet_exists(app, pet_id) {
        return Err("宠物资源不存在或校验失败".to_string());
    }
    let config = update_config(app, |config| {
        if config.disabled_pet_ids.iter().any(|id| id == pet_id) {
            return Err("这只宠物已停用，请先启用它".to_string());
        }
        if config
            .instances
            .iter()
            .any(|instance| instance.pet_id == pet_id)
        {
            return Err("每种宠物只能显示一只".to_string());
        }
        let instance_id = format!("instance-{}", config.next_instance_id);
        config.next_instance_id += 1;
        config.instances.push(PetInstanceConfig {
            id: instance_id,
            pet_id: pet_id.to_string(),
            visible: true,
            position: None,
        });
        Ok(())
    })?;
    let instance = config
        .instances
        .last()
        .cloned()
        .ok_or_else(|| "failed to create pet instance".to_string())?;
    create_pet_window(app, &instance, &config)?;
    rebuild_tray_menu(app).map_err(|error| error.to_string())?;
    Ok(PetInstanceInfo {
        id: instance.id,
        pet_id: instance.pet_id,
        visible: instance.visible,
        is_main: false,
        position: None,
    })
}

#[tauri::command]
fn add_pet_instance(app: tauri::AppHandle, pet_id: String) -> Result<PetInstanceInfo, String> {
    add_pet_instance_internal(&app, &pet_id)
}

#[tauri::command]
fn remove_pet_instance(
    app: tauri::AppHandle,
    instance_id: String,
) -> Result<Vec<PetInstanceInfo>, String> {
    if instance_id == "main" {
        return Err("默认宠物不能删除，只能隐藏".to_string());
    }
    let previous_config = config_snapshot(&app)?;
    let removed_pet_id = previous_config
        .instances
        .iter()
        .find(|instance| instance.id == instance_id)
        .map(|instance| instance.pet_id.clone());
    let config = update_config(&app, |config| {
        let before = config.instances.len();
        config
            .instances
            .retain(|instance| instance.id != instance_id);
        if config.instances.len() == before {
            return Err("pet instance is not configured".to_string());
        }
        Ok(())
    })?;
    if let Some(window) = app.get_webview_window(&instance_label(&instance_id)?) {
        window.close().map_err(|error| error.to_string())?;
    }
    if let Some(pet_id) = removed_pet_id {
        let pet_still_has_instance = config
            .instances
            .iter()
            .any(|instance| instance.pet_id == pet_id);
        if !pet_still_has_instance {
            if let Some(window) = app.get_webview_window(&chat_window_label(&pet_id)?) {
                window
                    .destroy()
                    .map_err(|error| format!("failed to close pet chat window: {error}"))?;
            }
        }
    }
    sync_macos_activation_policy(&app, &config)?;
    rebuild_tray_menu(&app).map_err(|error| error.to_string())?;
    Ok(all_instance_info(&config))
}

#[tauri::command]
fn save_pet_position(
    app: tauri::AppHandle,
    instance_id: String,
    x: f64,
    y: f64,
) -> Result<(), String> {
    if !x.is_finite() || !y.is_finite() {
        return Err("invalid pet position".to_string());
    }
    update_config(&app, |config| {
        let instance = config
            .instances
            .iter_mut()
            .find(|instance| instance.id == instance_id)
            .ok_or_else(|| "pet instance is not configured".to_string())?;
        instance.position = Some(PetPosition { x, y });
        Ok(())
    })?;
    Ok(())
}

fn broadcast_settings(
    app: &tauri::AppHandle,
    pet_id: &str,
    settings: &PetSettings,
) -> Result<(), String> {
    let config = config_snapshot(app)?;
    for (label, window) in app.webview_windows() {
        let Some(instance_id) = instance_id_from_label(&label) else {
            continue;
        };
        if is_pet_window_label(&label)
            && config
                .instances
                .iter()
                .any(|instance| instance.id == instance_id && instance.pet_id == pet_id)
        {
            apply_window_settings(&window, settings)?;
        }
    }
    app.emit(
        "pet://settings",
        PetSettingsEvent {
            pet_id: pet_id.to_string(),
            settings: settings.clone(),
        },
    )
    .map_err(|error| format!("failed to broadcast pet settings: {error}"))
}

#[tauri::command]
fn update_pet_settings(
    app: tauri::AppHandle,
    pet_id: String,
    settings: PetSettings,
) -> Result<PetSettings, String> {
    if !pet_exists(&app, &pet_id) {
        return Err("宠物资源不存在或校验失败".to_string());
    }
    let settings = clamp_settings(settings);
    let config = update_config(&app, |config| {
        config.pet_settings.insert(pet_id.clone(), settings.clone());
        Ok(())
    })?;
    sync_macos_activation_policy(&app, &config)?;
    let saved = settings_for_pet(&config, &pet_id);
    broadcast_settings(&app, &pet_id, &saved)?;
    Ok(saved)
}

fn toggle_pet_pause_internal(app: &tauri::AppHandle, pet_id: &str) -> Result<PetSettings, String> {
    if !pet_exists(app, pet_id) {
        return Err("宠物资源不存在或校验失败".to_string());
    }
    let config = update_config(app, |config| {
        let mut settings = settings_for_pet(config, pet_id);
        settings.paused = !settings.paused;
        config.pet_settings.insert(pet_id.to_string(), settings);
        Ok(())
    })?;
    let settings = settings_for_pet(&config, pet_id);
    broadcast_settings(app, pet_id, &settings)?;
    Ok(settings)
}

#[tauri::command]
fn toggle_pet_pause(app: tauri::AppHandle, pet_id: String) -> Result<PetSettings, String> {
    toggle_pet_pause_internal(&app, &pet_id)
}

fn toggle_all_pause_internal(app: &tauri::AppHandle) -> Result<(), String> {
    let before = config_snapshot(app)?;
    let pet_ids: Vec<String> = before
        .instances
        .iter()
        .map(|instance| instance.pet_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let pause = pet_ids
        .iter()
        .any(|pet_id| !settings_for_pet(&before, pet_id).paused);
    let config = update_config(app, |config| {
        for pet_id in &pet_ids {
            let mut settings = settings_for_pet(config, pet_id);
            settings.paused = pause;
            config.pet_settings.insert(pet_id.clone(), settings);
        }
        Ok(())
    })?;
    for pet_id in pet_ids {
        let settings = settings_for_pet(&config, &pet_id);
        broadcast_settings(app, &pet_id, &settings)?;
    }
    Ok(())
}

#[tauri::command]
fn set_pet_enabled(
    app: tauri::AppHandle,
    pet_id: String,
    enabled: bool,
) -> Result<Vec<InstalledPet>, String> {
    if !pet_exists(&app, &pet_id) {
        return Err("宠物资源不存在或校验失败".to_string());
    }
    let config = update_config(&app, |config| {
        config.disabled_pet_ids.retain(|id| id != &pet_id);
        if !enabled {
            config.disabled_pet_ids.push(pet_id.clone());
            for instance in &mut config.instances {
                if instance.pet_id == pet_id {
                    instance.visible = false;
                }
            }
        }
        Ok(())
    })?;
    sync_macos_activation_policy(&app, &config)?;
    for instance in config
        .instances
        .iter()
        .filter(|instance| instance.pet_id == pet_id)
    {
        if let Some(window) = app.get_webview_window(&instance_label(&instance.id)?) {
            if enabled && instance.visible {
                window.show().map_err(|error| error.to_string())?;
                apply_fullscreen_visibility(
                    &window,
                    settings_for_pet(&config, &instance.pet_id).show_in_fullscreen,
                )?;
            } else {
                window.hide().map_err(|error| error.to_string())?;
            }
        }
    }
    rebuild_tray_menu(&app).map_err(|error| error.to_string())?;
    Ok(installed_pets(&app, &config))
}

fn validate_imported_manifest(
    manifest: &PetManifest,
    pet_id: &str,
    sprite_path: &str,
    sprite: &[u8],
) -> Result<(), String> {
    if !is_safe_id(pet_id) || manifest.id != pet_id {
        return Err(
            "pet.json 的 id 只能包含字母、数字、短横线或下划线，并且必须和目录一致".to_string(),
        );
    }
    if manifest.sprite_version_number != 2 {
        return Err("只支持 spriteVersionNumber: 2 的宠物资源".to_string());
    }
    if manifest.display_name.trim().is_empty() || manifest.description.trim().is_empty() {
        return Err("pet.json 缺少 displayName 或 description".to_string());
    }
    if manifest.spritesheet_path != sprite_path || !is_safe_relative_path(sprite_path) {
        return Err("spritesheetPath 不是安全的相对路径".to_string());
    }
    if sprite.is_empty() || sprite.len() > 50 * 1024 * 1024 {
        return Err("spritesheet 文件为空或超过 50 MB".to_string());
    }
    let decoded = image::load_from_memory(sprite)
        .map_err(|error| format!("无法解码 spritesheet: {error}"))?;
    if decoded.width() != SPRITESHEET_WIDTH || decoded.height() != SPRITESHEET_HEIGHT {
        return Err(format!(
            "spritesheet 尺寸为 {}x{}，应为 {}x{}",
            decoded.width(),
            decoded.height(),
            SPRITESHEET_WIDTH,
            SPRITESHEET_HEIGHT
        ));
    }
    Ok(())
}

#[tauri::command]
fn import_pet_package(
    app: tauri::AppHandle,
    file_name: String,
    data_base64: String,
) -> Result<InstalledPet, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_base64)
        .map_err(|error| format!("宠物包不是有效的 Base64: {error}"))?;
    if bytes.len() > 60 * 1024 * 1024 || !file_name.to_ascii_lowercase().ends_with(".zip") {
        return Err("请选择不超过 60 MB 的 .zip 宠物包".to_string());
    }
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|error| format!("无法打开宠物包: {error}"))?;
    let mut manifest_name = None;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("无法读取宠物包: {error}"))?;
        let name = entry.name().trim_end_matches('/');
        if name == "pet.json" || name.ends_with("/pet.json") {
            manifest_name = Some(name.to_string());
            break;
        }
    }
    let manifest_name = manifest_name.ok_or_else(|| "宠物包内缺少 pet.json".to_string())?;
    let root = manifest_name
        .rsplit_once('/')
        .map(|(prefix, _)| format!("{prefix}/"))
        .unwrap_or_default();
    let mut manifest_bytes = Vec::new();
    archive
        .by_name(&manifest_name)
        .map_err(|error| format!("无法读取 pet.json: {error}"))?
        .read_to_end(&mut manifest_bytes)
        .map_err(|error| format!("无法读取 pet.json: {error}"))?;
    let manifest = serde_json::from_slice::<PetManifest>(&manifest_bytes)
        .map_err(|error| format!("pet.json 格式错误: {error}"))?;
    let sprite_path = manifest.spritesheet_path.clone();
    let archive_sprite_path = format!("{root}{sprite_path}");
    let mut sprite = Vec::new();
    archive
        .by_name(&archive_sprite_path)
        .map_err(|error| format!("宠物包内缺少 spritesheet: {error}"))?
        .read_to_end(&mut sprite)
        .map_err(|error| format!("无法读取 spritesheet: {error}"))?;
    validate_imported_manifest(&manifest, &manifest.id, &sprite_path, &sprite)?;
    let character_path = format!("{root}character.json");
    let character = if archive.file_names().any(|name| name == character_path) {
        let mut character_bytes = Vec::new();
        archive
            .by_name(&character_path)
            .map_err(|error| format!("无法读取 character.json: {error}"))?
            .read_to_end(&mut character_bytes)
            .map_err(|error| format!("无法读取 character.json: {error}"))?;
        let dialogue = decode_dialogue(&character_bytes)?;
        Some((character_bytes, dialogue))
    } else {
        None
    };
    if pet_is_bundled(&app, &manifest.id).is_some() || pet_is_imported(&app, &manifest.id).is_some()
    {
        return Err(format!("宠物 id 已存在: {}", manifest.id));
    }
    let pet_root = imported_pets_path(&app)?.join(&manifest.id);
    let sprite_target = pet_root.join(&sprite_path);
    if let Some(parent) = sprite_target.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建宠物目录: {error}"))?;
    }
    fs::write(pet_root.join("pet.json"), &manifest_bytes)
        .map_err(|error| format!("无法保存 pet.json: {error}"))?;
    fs::write(&sprite_target, &sprite).map_err(|error| format!("无法保存 spritesheet: {error}"))?;
    if let Some((character_bytes, _)) = &character {
        fs::write(pet_root.join("character.json"), character_bytes)
            .map_err(|error| format!("无法保存 character.json: {error}"))?;
    }
    rebuild_tray_menu(&app).map_err(|error| error.to_string())?;
    let config = config_snapshot(&app)?;
    Ok(InstalledPet {
        id: manifest.id.clone(),
        display_name: manifest.display_name,
        description: manifest.description,
        sprite_version_number: manifest.sprite_version_number,
        spritesheet_path: manifest.spritesheet_path.clone(),
        source: "imported".to_string(),
        enabled: !config.disabled_pet_ids.iter().any(|id| id == &manifest.id),
        preview_data_url: Some(data_url(&manifest.spritesheet_path, &sprite)),
        path: None,
        settings: settings_for_pet(&config, &manifest.id),
    })
}

#[tauri::command]
fn remove_imported_pet(app: tauri::AppHandle, pet_id: String) -> Result<Vec<InstalledPet>, String> {
    if pet_is_bundled(&app, &pet_id).is_some() {
        return Err("内置宠物不能删除，可以选择停用".to_string());
    }
    let previous_config = config_snapshot(&app)?;
    let instance_ids: Vec<String> = previous_config
        .instances
        .iter()
        .filter(|instance| instance.pet_id == pet_id)
        .map(|instance| instance.id.clone())
        .collect();
    if instance_ids.iter().any(|id| id == "main") {
        return Err("默认宠物不能删除，请先更换默认宠物".to_string());
    }
    let path = imported_pets_path(&app)?.join(&pet_id);
    if !path.exists() {
        return Err("找不到这只导入的宠物".to_string());
    }

    // Removing a resource also removes its display instances. Otherwise the
    // manager appears to accept the confirmation but the old instance keeps
    // the resource referenced and the backend rejects the deletion.
    let config = update_config(&app, |config| {
        config
            .instances
            .retain(|instance| instance.pet_id != pet_id);
        config.disabled_pet_ids.retain(|id| id != &pet_id);
        config.pet_settings.remove(&pet_id);
        config.chat_positions.remove(&pet_id);
        Ok(())
    })?;

    if let Err(error) = fs::remove_dir_all(&path) {
        // Restore the configuration if the resource itself could not be
        // deleted, so a transient filesystem error does not lose instances.
        let restore_result = update_config(&app, |config| {
            *config = previous_config.clone();
            Ok(())
        });
        return Err(match restore_result {
            Ok(_) => format!("删除宠物失败: {error}"),
            Err(restore_error) => {
                format!("删除宠物失败: {error}；恢复配置失败: {restore_error}")
            }
        });
    }

    for instance_id in instance_ids {
        if let Some(window) = app.get_webview_window(&instance_label(&instance_id)?) {
            window
                .close()
                .map_err(|error| format!("宠物资源已删除，但关闭显示窗口失败: {error}"))?;
        }
    }
    if let Some(window) = app.get_webview_window(&chat_window_label(&pet_id)?) {
        window
            .destroy()
            .map_err(|error| format!("宠物资源已删除，但关闭聊天窗口失败: {error}"))?;
    }
    sync_macos_activation_policy(&app, &config)?;
    rebuild_tray_menu(&app).map_err(|error| error.to_string())?;
    Ok(installed_pets(&app, &config))
}

fn toggle_all_visibility(app: &tauri::AppHandle) -> Result<(), String> {
    let config = config_snapshot(app)?;
    let target = !config.instances.iter().any(|instance| instance.visible);
    let ids: Vec<String> = config
        .instances
        .iter()
        .map(|instance| instance.id.clone())
        .collect();
    for id in ids {
        set_instance_visible_internal(app, &id, target)?;
    }
    rebuild_tray_menu(app).map_err(|error| error.to_string())
}

fn build_app_menu(app: &tauri::AppHandle) -> tauri::Result<()> {
    let manage_pets = MenuItem::with_id(app, "app-manage-pets", "管理宠物", true, None::<&str>)?;
    let ai_settings = MenuItem::with_id(app, "app-ai-settings", "AI 设置", true, None::<&str>)?;
    let app_submenu = Submenu::with_id_and_items(
        app,
        "sakipet-app-menu",
        "SakiPet",
        true,
        &[&manage_pets, &ai_settings],
    )?;
    let menu = Menu::with_items(app, &[&app_submenu])?;
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| match event.id().as_ref() {
        "app-manage-pets" => {
            if let Err(error) = show_pet_manager(app) {
                eprintln!("{error}");
            }
        }
        "app-ai-settings" => {
            if let Err(error) = show_ai_settings(app) {
                eprintln!("{error}");
            }
        }
        _ => {}
    });
    Ok(())
}

fn build_tray_menu(app: &tauri::AppHandle) -> tauri::Result<Menu<Wry>> {
    let show_hide = MenuItem::with_id(
        app,
        "show-hide-all",
        "显示 / 隐藏全部宠物",
        true,
        None::<&str>,
    )?;
    let manage_pets = MenuItem::with_id(app, "manage-pets", "管理宠物", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "ai-settings", "AI 设置", true, None::<&str>)?;
    let toggle_pause = MenuItem::with_id(
        app,
        "toggle-pause",
        "暂停 / 继续全部宠物",
        true,
        None::<&str>,
    )?;
    let config = config_snapshot(app).unwrap_or_default();
    let catalog = installed_pets(app, &config);
    let mut add_items = Vec::new();
    for pet in catalog.iter().filter(|pet| {
        pet.enabled
            && !config
                .instances
                .iter()
                .any(|instance| instance.pet_id == pet.id)
    }) {
        let title = if pet.display_name.is_empty() {
            pet.id.clone()
        } else {
            pet.display_name.clone()
        };
        add_items.push(MenuItem::with_id(
            app,
            format!("add-pet:{}", pet.id),
            format!("添加 {title}"),
            true,
            None::<&str>,
        )?);
    }
    let add_refs: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = add_items
        .iter()
        .map(|item| item as &dyn tauri::menu::IsMenuItem<Wry>)
        .collect();
    let add_submenu = Submenu::with_id_and_items(
        app,
        "add-pet-menu",
        "添加宠物",
        !add_refs.is_empty(),
        &add_refs,
    )?;
    let mut instance_items = Vec::new();
    for instance in &config.instances {
        let state = if instance.visible { "隐藏" } else { "显示" };
        let name = pet_display_name(app, &instance.pet_id);
        instance_items.push(MenuItem::with_id(
            app,
            format!("toggle-instance:{}", instance.id),
            format!("{state} {name} ({})", instance.id),
            true,
            None::<&str>,
        )?);
    }
    let instance_refs: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = instance_items
        .iter()
        .map(|item| item as &dyn tauri::menu::IsMenuItem<Wry>)
        .collect();
    let instance_submenu = Submenu::with_id_and_items(
        app,
        "pet-instances-menu",
        "当前宠物",
        !instance_refs.is_empty(),
        &instance_refs,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let items: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = vec![
        &show_hide,
        &manage_pets,
        &settings,
        &toggle_pause,
        &add_submenu,
        &instance_submenu,
        &separator,
        &quit,
    ];
    Menu::with_items(app, &items)
}

fn rebuild_tray_menu(app: &tauri::AppHandle) -> tauri::Result<()> {
    let menu = build_tray_menu(app)?;
    let tray = app
        .tray_by_id("main")
        .ok_or_else(|| tauri::Error::AssetNotFound("main tray not found".to_string()))?;
    tray.set_menu(Some(menu))
}

fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let menu = build_tray_menu(app)?;
    let mut tray = TrayIconBuilder::with_id("main")
        .menu(&menu)
        .tooltip("SakiPet")
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| {
            let id = event.id().as_ref();
            if id == "show-hide-all" {
                if let Err(error) = toggle_all_visibility(app) {
                    eprintln!("failed to toggle pet visibility: {error}");
                }
            } else if id == "manage-pets" {
                if let Err(error) = show_pet_manager(app) {
                    eprintln!("{error}");
                }
            } else if id == "ai-settings" {
                if let Err(error) = show_ai_settings(app) {
                    eprintln!("{error}");
                }
            } else if id == "toggle-pause" {
                if let Err(error) = toggle_all_pause_internal(app) {
                    eprintln!("failed to toggle pet animation: {error}");
                }
            } else if let Some(pet_id) = id.strip_prefix("add-pet:") {
                if let Err(error) = add_pet_instance_internal(app, pet_id) {
                    eprintln!("failed to add pet: {error}");
                }
            } else if let Some(instance_id) = id.strip_prefix("toggle-instance:") {
                let visible = config_snapshot(app)
                    .ok()
                    .and_then(|config| {
                        config
                            .instances
                            .into_iter()
                            .find(|instance| instance.id == instance_id)
                    })
                    .map(|instance| !instance.visible)
                    .unwrap_or(false);
                if let Err(error) = set_instance_visible_internal(app, instance_id, visible) {
                    eprintln!("failed to toggle pet instance: {error}");
                }
                if let Err(error) = rebuild_tray_menu(app) {
                    eprintln!("failed to refresh tray menu: {error}");
                }
            } else if id == "quit" {
                app.exit(0);
            }
        });
    if let Some(icon) = app.default_window_icon().cloned() {
        tray = tray.icon(icon);
    }
    tray.build(app)?;
    Ok(())
}

fn restore_windows(app: &tauri::AppHandle, config: &AppConfig) -> Result<(), String> {
    sync_macos_activation_policy(app, config)?;
    if let Some(main) = app.get_webview_window("main") {
        let main_settings = config
            .instances
            .iter()
            .find(|instance| instance.id == "main")
            .map(|instance| settings_for_pet(config, &instance.pet_id))
            .unwrap_or_else(|| config.settings.clone());
        apply_window_settings(&main, &main_settings)?;
        if let Some(position) = config
            .instances
            .iter()
            .find(|instance| instance.id == "main")
            .and_then(|instance| instance.position.as_ref())
        {
            main.set_position(LogicalPosition::new(position.x, position.y))
                .map_err(|error| error.to_string())?;
        }
        if config
            .instances
            .iter()
            .find(|instance| instance.id == "main")
            .map(|instance| instance.visible)
            .unwrap_or(true)
        {
            main.show().map_err(|error| error.to_string())?;
            apply_fullscreen_visibility(&main, main_settings.show_in_fullscreen)?;
        } else {
            main.hide().map_err(|error| error.to_string())?;
        }
    }
    for instance in config
        .instances
        .iter()
        .filter(|instance| instance.id != "main")
    {
        create_pet_window(app, instance, config)?;
    }
    Ok(())
}

#[tauri::command]
fn look_direction(app: tauri::AppHandle, window_label: String) -> Option<u8> {
    let cursor = app.cursor_position().ok()?;
    let window = app.get_webview_window(&window_label)?;
    let pos = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    let scale_factor = window.scale_factor().ok()?;
    let left = pos.x as f64;
    let top = pos.y as f64;
    let right = left + size.width as f64;
    let bottom = top + size.height as f64;
    let dx_to_window = if cursor.x < left {
        left - cursor.x
    } else if cursor.x > right {
        cursor.x - right
    } else {
        0.0
    };
    let dy_to_window = if cursor.y < top {
        top - cursor.y
    } else if cursor.y > bottom {
        cursor.y - bottom
    } else {
        0.0
    };
    let look_margin = LOOK_MARGIN_LOGICAL * scale_factor;
    if dx_to_window * dx_to_window + dy_to_window * dy_to_window > look_margin * look_margin {
        return None;
    }
    let cx = left + size.width as f64 / 2.0;
    let cy = top + size.height as f64 / 2.0;
    let dx = cursor.x - cx;
    let dy = cursor.y - cy;
    let deadzone = LOOK_DEADZONE_LOGICAL * scale_factor;
    if dx * dx + dy * dy < deadzone * deadzone {
        return None;
    }
    let deg = dx.atan2(-dy).to_degrees().rem_euclid(360.0);
    Some((((deg + 11.25) / 22.5).floor() as i32 % 16) as u8)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Register state before the runtime creates configured WebViews. A
        // page can execute JavaScript before the setup hook is entered.
        .manage(AppState::new())
        .on_window_event(|window, event| {
            let label = window.label();
            match event {
                // WebView2 teardown can silently fail and leave the HWND
                // alive, so utility windows hide on close instead of being
                // destroyed. This must be a global hook: per-window listeners
                // attached after creation never receive events.
                WindowEvent::CloseRequested { api, .. }
                    if label == "pet-manager"
                        || label == "ai-settings"
                        || label.starts_with("pet-chat-") =>
                {
                    api.prevent_close();
                    let _ = window.hide();
                }
                _ => {}
            }
        })
        .setup(|app| {
            if let Err(error) = migrate_legacy_app_data(&app.handle()) {
                eprintln!("failed to migrate legacy app data: {error}");
            }
            let config = load_config(&app.handle());
            let state = app.state::<AppState>();
            let mut stored_config = state
                .config
                .lock()
                .map_err(|_| "app config lock is poisoned")?;
            *stored_config = config.clone();
            drop(stored_config);
            restore_windows(&app.handle(), &config)?;
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            start_fullscreen_overlay_guard(&app.handle());
            build_tray(&app.handle())?;
            build_app_menu(&app.handle())?;
            ai::start_heartbeat_scheduler(&app.handle());
            #[cfg(target_os = "macos")]
            if let Err(error) = macos_dock_menu::install(&app.handle()) {
                eprintln!("failed to install Dock menu: {error}");
            }
            // Belt and suspenders: the global `on_window_event` hook below
            // already prevents close for these windows; keep the per-window
            // handlers in case one dispatch path fails.
            if let Some(manager) = app.get_webview_window("pet-manager") {
                let manager_for_close = manager.clone();
                manager.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = manager_for_close.hide();
                    }
                });
            }
            if let Some(settings) = app.get_webview_window("ai-settings") {
                let settings_for_close = settings.clone();
                settings.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = settings_for_close.hide();
                    }
                });
            }
            if !has_visible_pet_config(&app.handle(), &config) {
                show_pet_manager(&app.handle())?;
            }
            state.ready.store(true, Ordering::Release);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            is_app_ready,
            look_direction,
            open_pet_manager,
            open_ai_settings,
            open_pet_chat,
            hide_pet_chat,
            get_pet_catalog,
            get_pet_settings,
            get_pet_instances,
            get_visible_pets,
            get_runtime_config,
            set_pet_instance_visible,
            add_pet_instance,
            remove_pet_instance,
            save_pet_position,
            update_pet_settings,
            toggle_pet_pause,
            set_pet_enabled,
            import_pet_package,
            remove_imported_pet,
            ai::get_ai_settings,
            ai::update_ai_settings,
            ai::set_ai_secret,
            ai::delete_ai_secret,
            ai::test_ai_provider,
            ai::capture_desktop,
            ai::get_pet_state,
            ai::record_pet_interaction,
            ai::settle_pet_activity,
            ai::get_chat_history,
            ai::send_chat_message,
            ai::cancel_chat_response,
            ai::clear_chat_history,
            ai::get_memories,
            ai::delete_memory,
            ai::update_memory,
            ai::clear_memories,
            ai::get_shared_memories,
            ai::delete_shared_memory,
            ai::update_shared_memory,
            ai::clear_shared_memories
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            #[cfg(not(target_os = "macos"))]
            let _ = (&app, &event);
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen {
                has_visible_windows: false,
                ..
            } = event
            {
                if let Err(error) = show_pet_manager(app) {
                    eprintln!("failed to reopen pet manager: {error}");
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_character_dialogue() {
        let card = decode_character(r#"{"version":1,"click":["你好"]}"#.as_bytes())
            .expect("legacy card should parse");
        assert_eq!(card.dialogue.click, vec!["你好"]);
        assert!(!card.dialogue.idle.is_empty());
    }

    #[test]
    fn parses_v2_prompt_and_preserves_extensions() {
        let card = decode_character(
            r#"{
          "spec":"chara_card_v2",
          "spec_version":"2.0",
          "data":{
            "name":"Saki",
            "description":"一只猫",
            "system_prompt":"保持简短",
            "extensions":{"sakipet":{"dialogue":{"version":1,"idle":["喵"]}},"custom":{"keep":true}}
          }
        }"#
            .as_bytes(),
        )
        .expect("v2 card should parse");
        assert_eq!(card.name, "Saki");
        assert_eq!(card.system_prompt, "保持简短");
        assert_eq!(card.dialogue.idle, vec!["喵"]);
        assert_eq!(card.extensions["custom"]["keep"], true);
    }

    #[test]
    fn lorebook_only_inserts_triggered_entries() {
        let card = CharacterCard {
            character_book: Some(CharacterBook {
                entries: vec![
                    CharacterBookEntry {
                        keys: vec!["加班".to_string()],
                        content: "提醒休息".to_string(),
                        enabled: true,
                        ..CharacterBookEntry::default()
                    },
                    CharacterBookEntry {
                        keys: vec!["旅行".to_string()],
                        content: "带上相机".to_string(),
                        enabled: true,
                        ..CharacterBookEntry::default()
                    },
                ],
                ..CharacterBook::default()
            }),
            ..CharacterCard::default()
        };
        let lore = card.relevant_lorebook("今天要加班");
        assert!(lore.contains("提醒休息"));
        assert!(!lore.contains("带上相机"));
    }

    #[test]
    fn character_file_limit_is_one_megabyte() {
        let bytes = vec![b' '; 1024 * 1024 + 1];
        assert!(decode_character(&bytes).is_err());
    }
}

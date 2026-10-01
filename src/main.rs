#[cfg(not(all(feature = "lock-mode", feature = "greet-mode")))]
compile_error!("wyrd-greet v0.1.0 requires both `lock-mode` and `greet-mode` features");

mod greetd;
mod lua_config;
mod pam;
mod sessions;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use log::{error, info, warn};
use std::collections::HashMap;
use std::ffi::CString;
use std::os::fd::AsRawFd;
use std::os::unix::io::{AsFd, FromRawFd};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;
use tiny_skia::{Color, Pixmap};
use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_buffer, wl_compositor, wl_keyboard, wl_output, wl_registry, wl_seat, wl_shm,
        wl_shm_pool, wl_surface,
    },
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
    ext_session_lock_v1::{self, ExtSessionLockV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};
use wyrd_engine::widgets::{LayoutConfig, StyleConfig, WidgetConfig};
use zeroize::Zeroize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GreetMode {
    #[value(alias = "locker")]
    Lock,
    #[value(alias = "greeter")]
    Greet,
}

#[derive(Parser, Debug)]
#[command(name = "wyrd-greet", about = "Wyrd unified greeter and session locker")]
struct Cli {
    /// Operating mode: 'lock' (PAM + session lock) or 'greet' / 'greeter' (greetd IPC)
    #[arg(short, long, value_enum, default_value_t = GreetMode::Lock)]
    mode: GreetMode,

    /// PAM service name (for --mode=lock)
    #[arg(short, long, default_value = "login")]
    service: String,

    /// Default session command to launch after login (for --mode=greet)
    #[arg(short, long)]
    cmd: Option<String>,

    /// Default username to pre-fill (for --mode=greet)
    #[arg(short, long)]
    user: Option<String>,

    /// Run in development mode without requiring a greetd daemon
    #[arg(long, default_value_t = false)]
    dev: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ThemeState {
    #[serde(default = "default_theme_name")]
    pub theme: String,
    #[serde(default = "default_accent")]
    pub accent: String,
    #[serde(default = "default_bg")]
    pub background: String,
    #[serde(default = "default_surface")]
    pub surface: String,
    #[serde(default = "default_surface_alt")]
    pub surface_alt: String,
    #[serde(default = "default_fg")]
    pub foreground: String,
    #[serde(default = "default_fg_muted")]
    pub foreground_muted: String,
    #[serde(default = "default_danger")]
    pub danger: String,
    #[serde(default = "default_radius")]
    pub radius: f32,
}

fn default_theme_name() -> String {
    "aetheria".to_string()
}
fn default_accent() -> String {
    "#c72548".to_string()
}
fn default_bg() -> String {
    "#0d060f".to_string()
}
fn default_surface() -> String {
    "#160a17f2".to_string()
}
fn default_surface_alt() -> String {
    "#250e20".to_string()
}
fn default_fg() -> String {
    "#ede2e6".to_string()
}
fn default_fg_muted() -> String {
    "#a58994".to_string()
}
fn default_danger() -> String {
    "#ff3c5a".to_string()
}
fn default_radius() -> f32 {
    16.0
}

impl Default for ThemeState {
    fn default() -> Self {
        Self::for_preset("aetheria")
    }
}

impl ThemeState {
    pub fn for_preset(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "catppuccin" => Self {
                theme: "catppuccin".to_string(),
                accent: "#cba6f7".to_string(),
                background: "#11111b".to_string(),
                surface: "#1e1e2ef2".to_string(),
                surface_alt: "#313244".to_string(),
                foreground: "#cdd6f4".to_string(),
                foreground_muted: "#a6adc8".to_string(),
                danger: "#f38ba8".to_string(),
                radius: 16.0,
            },
            "tokyo-night" | "tokyonight" => Self {
                theme: "tokyo-night".to_string(),
                accent: "#7aa2f7".to_string(),
                background: "#16161e".to_string(),
                surface: "#1a1b26f2".to_string(),
                surface_alt: "#24283b".to_string(),
                foreground: "#c0caf5".to_string(),
                foreground_muted: "#787c99".to_string(),
                danger: "#f7768e".to_string(),
                radius: 16.0,
            },
            "gruvbox" => Self {
                theme: "gruvbox".to_string(),
                accent: "#d79921".to_string(),
                background: "#1d2021".to_string(),
                surface: "#282828f2".to_string(),
                surface_alt: "#3c3836".to_string(),
                foreground: "#ebdbb2".to_string(),
                foreground_muted: "#a89984".to_string(),
                danger: "#fb4934".to_string(),
                radius: 14.0,
            },
            "nord" => Self {
                theme: "nord".to_string(),
                accent: "#88c0d0".to_string(),
                background: "#242933".to_string(),
                surface: "#2e3440f2".to_string(),
                surface_alt: "#3b4252".to_string(),
                foreground: "#eceff4".to_string(),
                foreground_muted: "#d8dee9".to_string(),
                danger: "#bf616a".to_string(),
                radius: 14.0,
            },
            _ => Self {
                theme: "aetheria".to_string(),
                accent: default_accent(),
                background: default_bg(),
                surface: default_surface(),
                surface_alt: default_surface_alt(),
                foreground: default_fg(),
                foreground_muted: default_fg_muted(),
                danger: default_danger(),
                radius: default_radius(),
            },
        }
    }
}

fn resolve_user_home(username: &str) -> String {
    if !username.is_empty() && username != "user" {
        if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
            for line in passwd.lines() {
                let mut parts = line.split(':');
                if let Some(name) = parts.next() {
                    if name == username {
                        if let Some(home_dir) = parts.nth(4) {
                            if !home_dir.is_empty() {
                                return home_dir.to_string();
                            }
                        }
                    }
                }
            }
        }
        format!("/home/{}", username)
    } else {
        std::env::var("HOME").unwrap_or_else(|_| "/root".to_string())
    }
}

pub fn load_theme_and_wallpaper(
    mode: GreetMode,
    username: &str,
    lua_cfg: &lua_config::GreeterLuaConfig,
) -> (ThemeState, Option<String>) {
    let user_home = resolve_user_home(username);

    let mut preset_name = lua_cfg.theme_override.clone().unwrap_or_else(|| {
        let settings_candidates = match mode {
            GreetMode::Lock => vec![
                std::path::PathBuf::from(&user_home).join(".config/wyrd/settings.toml"),
                std::path::PathBuf::from("/etc/wyrd/settings.toml"),
            ],
            GreetMode::Greet => vec![
                std::path::PathBuf::from("/etc/wyrd/settings.toml"),
                std::path::PathBuf::from(&user_home).join(".config/wyrd/settings.toml"),
            ],
        };
        for settings_path in settings_candidates {
            if let Ok(content) = std::fs::read_to_string(&settings_path) {
                if let Ok(val) = toml::from_str::<toml::Value>(&content) {
                    if let Some(t) = val.get("theme").and_then(|v| v.as_str()) {
                        return t.to_string();
                    }
                }
            }
        }
        "dynamic".to_string()
    });

    if preset_name.is_empty() {
        preset_name = "dynamic".to_string();
    }

    let mut theme = ThemeState::for_preset(&preset_name);
    if let Some(r) = lua_cfg.card_radius {
        theme.radius = r;
    }

    let mut wallpaper_path = lua_cfg.wallpaper_override.clone();

    let mut state_candidates = Vec::new();
    match mode {
        GreetMode::Lock => {
            if let Ok(xdg_runtime) = std::env::var("XDG_RUNTIME_DIR") {
                state_candidates
                    .push(std::path::PathBuf::from(&xdg_runtime).join("wyrd/wallpaper.toml"));
                state_candidates
                    .push(std::path::PathBuf::from(&xdg_runtime).join("wyrd/theme.toml"));
            }
            state_candidates.push(
                std::path::PathBuf::from(&user_home).join(".local/state/wyrd/wallpaper.toml"),
            );
            state_candidates
                .push(std::path::PathBuf::from(&user_home).join(".config/wyrd/wallpaper.toml"));
            state_candidates.push(std::path::PathBuf::from("/var/lib/wyrd/wallpaper.toml"));
            state_candidates.push(std::path::PathBuf::from("/etc/wyrd/wallpaper.toml"));
        }
        GreetMode::Greet => {
            state_candidates.push(std::path::PathBuf::from("/var/lib/wyrd/wallpaper.toml"));
            state_candidates.push(std::path::PathBuf::from("/etc/wyrd/wallpaper.toml"));
            state_candidates.push(
                std::path::PathBuf::from(&user_home).join(".local/state/wyrd/wallpaper.toml"),
            );
            state_candidates
                .push(std::path::PathBuf::from(&user_home).join(".config/wyrd/wallpaper.toml"));
        }
    }

    for path in state_candidates {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(val) = toml::from_str::<toml::Value>(&content) {
                if wallpaper_path.is_none() {
                    if let Some(outputs) = val.get("outputs").and_then(|v| v.as_table()) {
                        for (_, wp) in outputs {
                            if let Some(s) = wp.as_str() {
                                if std::path::Path::new(s).is_file() {
                                    wallpaper_path = Some(s.to_string());
                                    break;
                                }
                            }
                        }
                    }
                }
                if preset_name == "dynamic" {
                    theme.theme = "dynamic".to_string();
                    if let Some(acc) = val.get("accent").and_then(|v| v.as_str()) {
                        theme.accent = acc.to_string();
                    }
                    if let Some(pal) = val.get("palette").and_then(|v| v.as_table()) {
                        if let Some(bg) = pal.get("background").and_then(|v| v.as_str()) {
                            theme.background = bg.to_string();
                        }
                        if let Some(sc) = pal.get("surface_container").and_then(|v| v.as_str()) {
                            theme.surface = format!("{}e6", sc.trim_end_matches('f'));
                        }
                        if let Some(sv) = pal.get("surface_variant").and_then(|v| v.as_str()) {
                            theme.surface_alt = format!("{}bf", sv.trim_end_matches('f'));
                        }
                        if let Some(fg) = pal.get("on_surface").and_then(|v| v.as_str()) {
                            theme.foreground = fg.to_string();
                        }
                        if let Some(fgm) = pal.get("on_surface_variant").and_then(|v| v.as_str()) {
                            theme.foreground_muted = fgm.to_string();
                        }
                    }
                }
            }
        }
    }

    (theme, wallpaper_path)
}

fn read_hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "wyrd".to_string())
}

fn render_wallpaper_backdrop(
    src: &image::RgbaImage,
    width: u32,
    height: u32,
    dim: f32,
) -> Option<Pixmap> {
    let w = width.max(1);
    let h = height.max(1);
    let resized = image::imageops::resize(src, w, h, image::imageops::FilterType::Triangle);
    let mut pixmap = Pixmap::new(w, h)?;
    let dst = pixmap.data_mut();
    let keep = (1.0 - dim.clamp(0.0, 0.92)).clamp(0.08, 1.0);
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let inv_rx = 1.0 / cx.max(1.0);
    let inv_ry = 1.0 / cy.max(1.0);

    for (idx, px) in resized.pixels().enumerate() {
        let idx = idx as u64;
        let x = (idx % w as u64) as f32;
        let y = (idx / w as u64) as f32;
        let dx = (x - cx) * inv_rx;
        let dy = (y - cy) * inv_ry;
        let dist2 = (dx * dx + dy * dy).min(1.8);
        let vignette = (1.0 - dist2 * 0.28).clamp(0.45, 1.0);
        let factor = keep * vignette;
        let off = idx as usize * 4;
        dst[off] = (px[0] as f32 * factor + 4.0) as u8;
        dst[off + 1] = (px[1] as f32 * factor + 5.0) as u8;
        dst[off + 2] = (px[2] as f32 * factor + 8.0) as u8;
        dst[off + 3] = 255;
    }
    Some(pixmap)
}

enum AuthResult {
    Lock {
        success: bool,
        elapsed_ms: f64,
    },
    Greet {
        success: bool,
        client: Option<greetd::GreetdClient>,
        elapsed_ms: f64,
    },
}

struct ScreenOutput {
    output: wl_output::WlOutput,
    width: u32,
    height: u32,
    surface: Option<wl_surface::WlSurface>,
    lock_surface: Option<ExtSessionLockSurfaceV1>,
    xdg_surface: Option<XdgSurface>,
    xdg_toplevel: Option<XdgToplevel>,
    configured: bool,
    first_configure_logged: bool,
    first_commit_logged: bool,
}

fn detect_default_username(override_user: Option<&str>) -> String {
    if let Some(u) = override_user.map(str::trim).filter(|s| !s.is_empty()) {
        return u.to_string();
    }
    if let Ok(env_user) = std::env::var("USER") {
        let trimmed = env_user.trim();
        if !trimmed.is_empty()
            && !matches!(
                trimmed,
                "greeter" | "root" | "sddm" | "gdm" | "lightdm" | "nobody"
            )
        {
            return trimmed.to_string();
        }
    }
    if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
        let mut candidates: Vec<(u32, String)> = Vec::new();
        for line in passwd.lines() {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 7 {
                let name = parts[0];
                let shell = parts[6];
                if let Ok(uid) = parts[2].parse::<u32>() {
                    if (1000..60000).contains(&uid)
                        && !shell.ends_with("nologin")
                        && !shell.ends_with("false")
                    {
                        candidates.push((uid, name.to_string()));
                    }
                }
            }
        }
        candidates.sort_by_key(|(uid, _)| *uid);
        if let Some((_, name)) = candidates.into_iter().next() {
            return name;
        }
    }
    "user".to_string()
}

const MAX_PASSWORD_BYTES: usize = 256;

struct AppState {
    start_instant: Instant,
    mode: GreetMode,
    dev: bool,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    xdg_wm_base: Option<XdgWmBase>,
    session_lock_manager: Option<ExtSessionLockManagerV1>,
    session_lock: Option<ExtSessionLockV1>,
    locked: bool,
    unlocked: bool,
    outputs: HashMap<u32, ScreenOutput>,
    username: String,
    hostname: String,
    password: zeroize::Zeroizing<String>,
    auth_failed: bool,
    auth_pending: bool,
    spinner_step: usize,
    last_minute_str: String,
    theme: ThemeState,
    lua_cfg: lua_config::GreeterLuaConfig,
    wallpaper_img: Option<image::RgbaImage>,
    wallpaper_rx: Option<Receiver<image::RgbaImage>>,
    backdrop_cache: HashMap<(u32, u32), Pixmap>,
    service: String,
    sessions: Vec<sessions::DesktopSession>,
    active_session_idx: usize,
    greetd_client: Option<greetd::GreetdClient>,
    xkb_context: xkbcommon::xkb::Context,
    xkb_keymap: Option<xkbcommon::xkb::Keymap>,
    xkb_state: Option<xkbcommon::xkb::State>,
    render_ctx: Option<wyrd_engine::render::context::RenderContext>,
    font_rx: Option<Receiver<(wyrd_engine::render::context::RenderContext, f64)>>,
    auth_tx: Sender<AuthResult>,
    auth_rx: Receiver<AuthResult>,
}

impl AppState {
    fn new(
        start_instant: Instant,
        mode: GreetMode,
        service: String,
        dev: bool,
        greetd_client: Option<greetd::GreetdClient>,
        font_rx: Option<Receiver<(wyrd_engine::render::context::RenderContext, f64)>>,
    ) -> Self {
        let username = detect_default_username(None);
        let hostname = read_hostname();
        let lua_cfg = lua_config::GreeterLuaConfig::load(&username);
        let (theme, wallpaper_path) = load_theme_and_wallpaper(mode, &username, &lua_cfg);
        let wallpaper_rx = if let Some(wp) = wallpaper_path {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                if let Ok(bytes) = std::fs::read(&wp) {
                    if let Ok(img) = image::load_from_memory(&bytes) {
                        let _ = tx.send(img.to_rgba8());
                    }
                }
            });
            Some(rx)
        } else {
            None
        };
        let available_sessions = if mode == GreetMode::Greet {
            sessions::scan_available_sessions()
        } else {
            Vec::new()
        };
        let xkb_context = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
        let (auth_tx, auth_rx) = mpsc::channel();

        Self {
            start_instant,
            mode,
            dev,
            compositor: None,
            shm: None,
            xdg_wm_base: None,
            session_lock_manager: None,
            session_lock: None,
            locked: false,
            unlocked: false,
            outputs: HashMap::new(),
            username,
            hostname,
            password: zeroize::Zeroizing::new(String::with_capacity(MAX_PASSWORD_BYTES)),
            auth_failed: false,
            auth_pending: false,
            spinner_step: 0,
            last_minute_str: String::new(),
            theme,
            lua_cfg,
            wallpaper_img: None,
            wallpaper_rx,
            backdrop_cache: HashMap::new(),
            service,
            sessions: available_sessions,
            active_session_idx: 0,
            greetd_client,
            xkb_context,
            xkb_keymap: None,
            xkb_state: None,
            render_ctx: None,
            font_rx,
            auth_tx,
            auth_rx,
        }
    }

    fn elapsed_ms(&self) -> f64 {
        self.start_instant.elapsed().as_secs_f64() * 1000.0
    }

    fn poll_font_context(&mut self) -> bool {
        if self.render_ctx.is_some() {
            return false;
        }
        if let Some(rx) = self.font_rx.as_ref() {
            if let Ok((mut ctx, font_ms)) = rx.try_recv() {
                info!(
                    "[TIMING] after RenderContext::new ready on main loop at +{:.2}ms (font scan duration: {:.2}ms)",
                    self.elapsed_ms(),
                    font_ms
                );
                // Start smooth fade/scale-in animations for all active outputs via wyrd-engine Animator
                for &global_name in self.outputs.keys() {
                    let idx = global_name as usize;
                    ctx.animator.start_named_spring(
                        &format!("surf_scale_{}", idx),
                        0.93,
                        1.0,
                        260.0,
                        22.0,
                    );
                    ctx.animator.start_named_spring(
                        &format!("surf_oy_{}", idx),
                        -16.0,
                        0.0,
                        260.0,
                        22.0,
                    );
                    ctx.animator.start_named_spring(
                        &format!("surf_opacity_{}", idx),
                        0.0,
                        1.0,
                        280.0,
                        24.0,
                    );
                }
                self.render_ctx = Some(ctx);
                self.font_rx = None;
                return true;
            }
        }
        false
    }

    #[cfg(test)]
    fn ensure_render_ctx_for_tests(&mut self) {
        if self.render_ctx.is_none() {
            if let Some(rx) = self.font_rx.take() {
                if let Ok((ctx, _)) = rx.recv() {
                    self.render_ctx = Some(ctx);
                    return;
                }
            }
            self.render_ctx = Some(wyrd_engine::render::context::RenderContext::new(1.0));
        }
    }

    /// Starts authentication asynchronously on a background thread (`submit_auth_async`)
    /// while providing a synchronous wrapper (`submit_auth`) for unit tests and `--dev` mode.
    fn submit_auth_async(&mut self) {
        if self.auth_pending {
            return;
        }
        self.auth_failed = false;

        match self.mode {
            GreetMode::Lock => {
                let mut password_copy = String::with_capacity(MAX_PASSWORD_BYTES);
                std::mem::swap(&mut password_copy, &mut *self.password);
                self.password.zeroize();
                self.password.clear();

                let service = self.service.clone();
                let username = self.username.clone();
                let tx = self.auth_tx.clone();
                self.auth_pending = true;

                std::thread::spawn(move || {
                    let t0 = Instant::now();
                    let ok = pam::authenticate_user(&service, &username, password_copy);
                    let _ = tx.send(AuthResult::Lock {
                        success: ok,
                        elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
                    });
                });
            }
            GreetMode::Greet => {
                if let Some(mut client) = self.greetd_client.take() {
                    let mut password_copy = String::with_capacity(MAX_PASSWORD_BYTES);
                    std::mem::swap(&mut password_copy, &mut *self.password);
                    self.password.zeroize();
                    self.password.clear();

                    let username = self.username.clone();
                    let session_cmd = self
                        .sessions
                        .get(self.active_session_idx)
                        .map(|s| s.exec.clone())
                        .unwrap_or_else(|| "Hyprland".to_string());
                    let tx = self.auth_tx.clone();
                    self.auth_pending = true;

                    std::thread::spawn(move || {
                        let t0 = Instant::now();
                        info!("Authenticating via greetd for session '{}'", session_cmd);
                        let res = client
                            .create_session(&username)
                            .and_then(|_| client.post_auth_response(Some(password_copy)))
                            .and_then(|_| {
                                let cmd_parts = session_cmd
                                    .split_whitespace()
                                    .map(String::from)
                                    .collect::<Vec<_>>();
                                client.start_session(cmd_parts, vec![])
                            });

                        let ok = matches!(res, Ok(greetd::GreetdResponse::Success));
                        if !ok {
                            warn!("greetd authentication result: {:?}", res);
                        }
                        let _ = tx.send(AuthResult::Greet {
                            success: ok,
                            client: Some(client),
                            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
                        });
                    });
                } else if self.dev {
                    info!("Dev mode: simulating authentication success without greetd");
                    self.password.zeroize();
                    self.password.clear();
                    self.unlocked = true;
                } else {
                    error!("No greetd client connected and not running with --dev");
                    self.password.zeroize();
                    self.password.clear();
                    self.auth_failed = true;
                    self.trigger_failure_shake();
                }
            }
        }
    }

    #[allow(dead_code)]
    fn submit_auth(&mut self) -> bool {
        self.submit_auth_async();
        if self.auth_pending {
            if let Ok(res) = self.auth_rx.recv() {
                self.handle_auth_result(res);
            }
        }
        self.unlocked
    }

    fn trigger_failure_shake(&mut self) {
        if let Some(ctx) = self.render_ctx.as_mut() {
            for &global_name in self.outputs.keys() {
                let idx = global_name as usize;
                ctx.animator.start_named_spring(
                    &format!("surf_ox_{}", idx),
                    -22.0,
                    0.0,
                    580.0,
                    13.0,
                );
                ctx.animator.start_named_spring(
                    &format!("surf_scale_{}", idx),
                    0.97,
                    1.0,
                    380.0,
                    18.0,
                );
            }
        }
    }

    fn handle_auth_result(&mut self, result: AuthResult) -> bool {
        self.auth_pending = false;
        match result {
            AuthResult::Lock {
                success,
                elapsed_ms,
            } => {
                info!(
                    "[TIMING] off-thread PAM authentication finished in {:.2}ms (success={})",
                    elapsed_ms, success
                );
                if success {
                    info!("PAM authentication succeeded for user '{}'", self.username);
                    if let Some(lock) = self.session_lock.take() {
                        lock.unlock_and_destroy();
                    }
                    self.locked = false;
                    self.unlocked = true;
                    true
                } else {
                    warn!("PAM authentication failed for user '{}'", self.username);
                    self.auth_failed = true;
                    self.trigger_failure_shake();
                    false
                }
            }
            AuthResult::Greet {
                success,
                client,
                elapsed_ms,
            } => {
                info!(
                    "[TIMING] off-thread greetd authentication finished in {:.2}ms (success={})",
                    elapsed_ms, success
                );
                self.greetd_client = client;
                if success {
                    info!("greetd session launched successfully!");
                    self.unlocked = true;
                    true
                } else {
                    self.auth_failed = true;
                    self.trigger_failure_shake();
                    false
                }
            }
        }
    }

    fn poll_wallpaper(&mut self) -> bool {
        if let Some(rx) = self.wallpaper_rx.as_ref() {
            if let Ok(img) = rx.try_recv() {
                self.wallpaper_img = Some(img);
                self.wallpaper_rx = None;
                self.backdrop_cache.clear();
                return true;
            }
        }
        false
    }

    fn rgba_str(hex: &str, alpha: f32) -> String {
        let s = hex.trim().trim_start_matches('#');
        if s.len() >= 6 {
            if let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&s[0..2], 16),
                u8::from_str_radix(&s[2..4], 16),
                u8::from_str_radix(&s[4..6], 16),
            ) {
                return format!("rgba({}, {}, {}, {:.2})", r, g, b, alpha);
            }
        }
        format!("rgba(26, 17, 16, {:.2})", alpha)
    }

    fn build_styles(&self) -> HashMap<String, StyleConfig> {
        let mut styles = HashMap::new();
        let border_col = if self.auth_failed {
            Self::rgba_str(&self.theme.danger, 0.85)
        } else if self.auth_pending {
            Self::rgba_str(&self.theme.accent, 0.75)
        } else {
            Self::rgba_str(&self.theme.accent, 0.35)
        };

        let input_border_col = if self.auth_failed {
            self.theme.danger.clone()
        } else if !self.password.is_empty() || self.auth_pending {
            self.theme.accent.clone()
        } else {
            Self::rgba_str(&self.theme.accent, 0.42)
        };

        let backdrop_bg = if self.wallpaper_img.is_some() {
            "rgba(0, 0, 0, 0.0)".to_string()
        } else {
            self.theme.background.clone()
        };

        styles.insert(
            "greet_backdrop".to_string(),
            StyleConfig {
                background: Some(backdrop_bg),
                foreground: Some(self.theme.foreground.clone()),
                font: Some(self.lua_cfg.font.clone()),
                font_size: Some(14.0),
                ..Default::default()
            },
        );

        styles.insert(
            "greet_top_bar".to_string(),
            StyleConfig {
                background: Some(format!(
                    "linear-gradient(135deg, {}, {})",
                    Self::rgba_str(&self.theme.surface, 0.84),
                    Self::rgba_str(&self.theme.background, 0.78)
                )),
                foreground: Some(self.theme.foreground.clone()),
                outline: Some(format!(
                    "1px solid {}",
                    Self::rgba_str(&self.theme.accent, 0.24)
                )),
                radius: Some(9999.0),
                padding: Some(vec![4.0, 10.0, 4.0, 10.0]),
                shadow: Some(wyrd_engine::config::ShadowConfig {
                    offset_x: Some(0.0),
                    offset_y: Some(10.0),
                    radius: 28.0,
                    opacity: 0.45,
                    color: Some("rgba(0, 0, 0, 0.45)".to_string()),
                }),
                ..Default::default()
            },
        );

        styles.insert(
            "bar_chip".to_string(),
            StyleConfig {
                background: Some(Self::rgba_str(&self.theme.surface_alt, 0.65)),
                foreground: Some(self.theme.foreground.clone()),
                outline: Some("1px solid rgba(255, 255, 255, 0.07)".to_string()),
                radius: Some(9999.0),
                padding: Some(vec![4.0, 12.0, 4.0, 12.0]),
                font: Some(self.lua_cfg.font.clone()),
                font_size: Some(12.0),
                ..Default::default()
            },
        );

        styles.insert(
            "bar_chip_accent".to_string(),
            StyleConfig {
                background: Some(Self::rgba_str(&self.theme.accent, 0.16)),
                foreground: Some(self.theme.accent.clone()),
                outline: Some(format!(
                    "1px solid {}",
                    Self::rgba_str(&self.theme.accent, 0.34)
                )),
                radius: Some(9999.0),
                padding: Some(vec![4.0, 12.0, 4.0, 12.0]),
                font: Some(self.lua_cfg.font.clone()),
                font_size: Some(12.0),
                ..Default::default()
            },
        );

        styles.insert(
            "clock_time".to_string(),
            StyleConfig {
                foreground: Some(self.theme.foreground.clone()),
                font: Some(self.lua_cfg.font.clone()),
                font_size: Some(64.0),
                shadow: Some(wyrd_engine::config::ShadowConfig {
                    offset_x: Some(0.0),
                    offset_y: Some(6.0),
                    radius: 20.0,
                    opacity: 0.55,
                    color: Some("rgba(0, 0, 0, 0.55)".to_string()),
                }),
                ..Default::default()
            },
        );
        styles.insert(
            "clock_date".to_string(),
            StyleConfig {
                foreground: Some(self.theme.accent.clone()),
                font: Some(self.lua_cfg.font.clone()),
                font_size: Some(15.0),
                ..Default::default()
            },
        );
        styles.insert(
            "greet_card".to_string(),
            StyleConfig {
                background: Some(format!(
                    "linear-gradient(145deg, {}, {})",
                    Self::rgba_str(&self.theme.surface, 0.88),
                    Self::rgba_str(&self.theme.background, 0.92)
                )),
                foreground: Some(self.theme.foreground.clone()),
                outline: Some(format!("1.5px solid {}", border_col)),
                radius: Some(self.lua_cfg.card_radius.unwrap_or(self.theme.radius)),
                padding: Some(vec![26.0, 30.0, 26.0, 30.0]),
                shadow: Some(wyrd_engine::config::ShadowConfig {
                    offset_x: Some(0.0),
                    offset_y: Some(18.0),
                    radius: 42.0,
                    opacity: 0.60,
                    color: Some("rgba(0, 0, 0, 0.60)".to_string()),
                }),
                ..Default::default()
            },
        );
        styles.insert(
            "greet_title".to_string(),
            StyleConfig {
                foreground: Some(self.theme.foreground.clone()),
                font: Some(self.lua_cfg.font.clone()),
                font_size: Some(17.0),
                ..Default::default()
            },
        );
        styles.insert(
            "greet_user".to_string(),
            StyleConfig {
                background: Some(Self::rgba_str(&self.theme.accent, 0.16)),
                foreground: Some(self.theme.accent.clone()),
                outline: Some(format!(
                    "1px solid {}",
                    Self::rgba_str(&self.theme.accent, 0.34)
                )),
                radius: Some(9999.0),
                padding: Some(vec![6.0, 16.0, 6.0, 16.0]),
                font: Some(self.lua_cfg.font.clone()),
                font_size: Some(13.0),
                ..Default::default()
            },
        );
        styles.insert(
            "greet_session".to_string(),
            StyleConfig {
                background: Some(Self::rgba_str(&self.theme.surface_alt, 0.72)),
                foreground: Some(self.theme.foreground.clone()),
                outline: Some("1px solid rgba(255, 255, 255, 0.10)".to_string()),
                radius: Some(9999.0),
                padding: Some(vec![6.0, 16.0, 6.0, 16.0]),
                font: Some(self.lua_cfg.font.clone()),
                font_size: Some(12.5),
                ..Default::default()
            },
        );
        styles.insert(
            "greet_input".to_string(),
            StyleConfig {
                background: Some(Self::rgba_str(&self.theme.background, 0.78)),
                foreground: Some(if self.password.is_empty() && !self.auth_pending {
                    self.theme.foreground_muted.clone()
                } else {
                    self.theme.foreground.clone()
                }),
                outline: Some(format!("1.5px solid {}", input_border_col)),
                radius: Some(14.0),
                padding: Some(vec![10.0, 18.0, 10.0, 18.0]),
                font: Some(self.lua_cfg.font.clone()),
                font_size: Some(14.0),
                ..Default::default()
            },
        );
        styles.insert(
            "greet_status_err".to_string(),
            StyleConfig {
                foreground: Some(self.theme.danger.clone()),
                font: Some(self.lua_cfg.font.clone()),
                font_size: Some(12.5),
                ..Default::default()
            },
        );
        styles.insert(
            "greet_status_hint".to_string(),
            StyleConfig {
                foreground: Some(self.theme.foreground_muted.clone()),
                font: Some(self.lua_cfg.font.clone()),
                font_size: Some(12.0),
                ..Default::default()
            },
        );

        for (k, v) in &self.lua_cfg.custom_styles {
            styles.insert(k.clone(), v.clone());
        }

        styles
    }

    fn build_top_pill_bar(
        &self,
        col_w: f32,
        time_str: &str,
        date_str: &str,
        session_name: &str,
    ) -> WidgetConfig {
        let bar_w = (col_w - 64.0).clamp(620.0, 1140.0);
        WidgetConfig {
            ty: wyrd_engine::widgets::WidgetKind::Container,
            id: Some("greet_top_bar".to_string()),
            style: Some("greet_top_bar".to_string()),
            layout: Some(LayoutConfig {
                mode: Some("flex_row".to_string()),
                justify: Some("space-between".to_string()),
                align: Some("center".to_string()),
                width: Some(bar_w),
                height: Some(40.0),
                ..Default::default()
            }),
            children: vec![
                WidgetConfig {
                    ty: wyrd_engine::widgets::WidgetKind::Container,
                    layout: Some(LayoutConfig {
                        mode: Some("flex_row".to_string()),
                        justify: Some("start".to_string()),
                        align: Some("center".to_string()),
                        gap: Some(8.0),
                        width: Some(330.0),
                        height: Some(32.0),
                        ..Default::default()
                    }),
                    children: vec![
                        WidgetConfig {
                            ty: wyrd_engine::widgets::WidgetKind::Button,
                            style: Some("bar_chip_accent".to_string()),
                            text: Some(format!("󰣇  {}", self.hostname)),
                            layout: Some(LayoutConfig {
                                width: Some(176.0),
                                height: Some(28.0),
                                justify: Some("center".to_string()),
                                align: Some("center".to_string()),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        WidgetConfig {
                            ty: wyrd_engine::widgets::WidgetKind::Button,
                            style: Some("bar_chip".to_string()),
                            text: Some(format!("󰨡  {}", session_name)),
                            layout: Some(LayoutConfig {
                                width: Some(140.0),
                                height: Some(28.0),
                                justify: Some("center".to_string()),
                                align: Some("center".to_string()),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
                WidgetConfig {
                    ty: wyrd_engine::widgets::WidgetKind::Button,
                    style: Some("bar_chip".to_string()),
                    text: Some(format!("󰃭  {}   •   󰥔  {}", date_str, time_str)),
                    layout: Some(LayoutConfig {
                        width: Some(320.0),
                        height: Some(28.0),
                        justify: Some("center".to_string()),
                        align: Some("center".to_string()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                WidgetConfig {
                    ty: wyrd_engine::widgets::WidgetKind::Container,
                    layout: Some(LayoutConfig {
                        mode: Some("flex_row".to_string()),
                        justify: Some("end".to_string()),
                        align: Some("center".to_string()),
                        gap: Some(8.0),
                        width: Some(330.0),
                        height: Some(32.0),
                        ..Default::default()
                    }),
                    children: vec![
                        WidgetConfig {
                            ty: wyrd_engine::widgets::WidgetKind::Button,
                            style: Some("bar_chip".to_string()),
                            text: Some(format!("🎨 {}", self.theme.theme)),
                            layout: Some(LayoutConfig {
                                width: Some(134.0),
                                height: Some(28.0),
                                justify: Some("center".to_string()),
                                align: Some("center".to_string()),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        WidgetConfig {
                            ty: wyrd_engine::widgets::WidgetKind::Button,
                            style: Some("bar_chip_accent".to_string()),
                            text: Some(format!("  {}", self.username)),
                            layout: Some(LayoutConfig {
                                width: Some(180.0),
                                height: Some(28.0),
                                justify: Some("center".to_string()),
                                align: Some("center".to_string()),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_monitor_column(
        &self,
        col_w: f32,
        col_h: f32,
        time_str: &str,
        date_str: &str,
        session_name: &str,
        card_height: f32,
        card_children: Vec<WidgetConfig>,
    ) -> WidgetConfig {
        let card_w = self.lua_cfg.card_width;
        let center_stack = WidgetConfig {
            ty: wyrd_engine::widgets::WidgetKind::Container,
            layout: Some(LayoutConfig {
                mode: Some("flex_col".to_string()),
                justify: Some("center".to_string()),
                align: Some("center".to_string()),
                gap: Some(24.0),
                width: Some(card_w.max(440.0)),
                height: Some(card_height + 130.0),
                ..Default::default()
            }),
            children: vec![
                WidgetConfig {
                    ty: wyrd_engine::widgets::WidgetKind::Container,
                    id: Some("clock_box".to_string()),
                    layout: Some(LayoutConfig {
                        mode: Some("flex_col".to_string()),
                        justify: Some("center".to_string()),
                        align: Some("center".to_string()),
                        gap: Some(6.0),
                        width: Some(card_w.max(440.0)),
                        height: Some(100.0),
                        ..Default::default()
                    }),
                    children: vec![
                        WidgetConfig {
                            ty: wyrd_engine::widgets::WidgetKind::Text,
                            id: Some("clock_time".to_string()),
                            style: Some("clock_time".to_string()),
                            text: Some(time_str.to_string()),
                            layout: Some(LayoutConfig {
                                width: Some(card_w.max(440.0)),
                                height: Some(68.0),
                                justify: Some("center".to_string()),
                                align: Some("center".to_string()),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                        WidgetConfig {
                            ty: wyrd_engine::widgets::WidgetKind::Text,
                            id: Some("clock_date".to_string()),
                            style: Some("clock_date".to_string()),
                            text: Some(date_str.to_string()),
                            layout: Some(LayoutConfig {
                                width: Some(card_w.max(440.0)),
                                height: Some(24.0),
                                justify: Some("center".to_string()),
                                align: Some("center".to_string()),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
                WidgetConfig {
                    ty: wyrd_engine::widgets::WidgetKind::Container,
                    id: Some("greet_card".to_string()),
                    style: Some("greet_card".to_string()),
                    layout: Some(LayoutConfig {
                        mode: Some("flex_col".to_string()),
                        justify: Some("center".to_string()),
                        align: Some("center".to_string()),
                        gap: Some(14.0),
                        width: Some(card_w),
                        height: Some(card_height),
                        ..Default::default()
                    }),
                    children: card_children,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        if self.lua_cfg.show_top_bar && col_h >= 600.0 {
            let top_bar = self.build_top_pill_bar(col_w, time_str, date_str, session_name);
            let bottom_pill = WidgetConfig {
                ty: wyrd_engine::widgets::WidgetKind::Button,
                style: Some("bar_chip".to_string()),
                text: Some(
                    "󰌌  Tab: Switch Session   •   󰌑  Enter: Login   •   󱊷  Esc: Clear".to_string(),
                ),
                layout: Some(LayoutConfig {
                    width: Some(530.0),
                    height: Some(30.0),
                    justify: Some("center".to_string()),
                    align: Some("center".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            };

            WidgetConfig {
                ty: wyrd_engine::widgets::WidgetKind::Container,
                style: Some("greet_backdrop".to_string()),
                layout: Some(LayoutConfig {
                    mode: Some("flex_col".to_string()),
                    justify: Some("space-between".to_string()),
                    align: Some("center".to_string()),
                    width: Some(col_w),
                    height: Some(col_h),
                    padding: Some(vec![16.0, 24.0, 20.0, 24.0]),
                    ..Default::default()
                }),
                children: vec![top_bar, center_stack, bottom_pill],
                ..Default::default()
            }
        } else {
            WidgetConfig {
                ty: wyrd_engine::widgets::WidgetKind::Container,
                style: Some("greet_backdrop".to_string()),
                layout: Some(LayoutConfig {
                    mode: Some("flex_col".to_string()),
                    justify: Some("center".to_string()),
                    align: Some("center".to_string()),
                    width: Some(col_w),
                    height: Some(col_h),
                    ..Default::default()
                }),
                children: vec![center_stack],
                ..Default::default()
            }
        }
    }

    fn build_widget_tree(&self, width: f32, height: f32) -> wyrd_engine::widgets::WidgetTree {
        let now = chrono::Local::now();
        let time_str = now.format(&self.lua_cfg.clock_format).to_string();
        let date_str = now.format(&self.lua_cfg.date_format).to_string();

        let title_str = if let Some(ref custom_title) = self.lua_cfg.title_override {
            custom_title.clone()
        } else if self.mode == GreetMode::Greet {
            "󰍂  WYRD GREETER".to_string()
        } else {
            "󰌾  SESSION LOCKED".to_string()
        };

        let cur_session = if !self.sessions.is_empty() {
            Some(&self.sessions[self.active_session_idx % self.sessions.len()])
        } else {
            None
        };
        let session_name = cur_session.map(|s| s.name.as_str()).unwrap_or("Hyprland");
        let session_exec = cur_session.map(|s| s.exec.as_str()).unwrap_or("Hyprland");

        let user_str = format!("  {}   •   🎨 {}", self.username, self.theme.theme);

        let spinner_frames = ["󰑐", "󰪞", "󰪟", "󰪠", "󰪡", "󰪢", "󰪣", "󰪤"];
        let spinner_glyph = spinner_frames[self.spinner_step % spinner_frames.len()];

        let masked_dots = {
            let count = self.password.chars().count().min(18);
            std::iter::repeat_n("●", count)
                .collect::<Vec<_>>()
                .join(" ")
        };

        let input_display = if self.auth_pending {
            format!("{}  Verifying credentials...", spinner_glyph)
        } else if self.password.is_empty() {
            self.lua_cfg.placeholder.clone()
        } else {
            masked_dots.clone()
        };

        let (status_text, status_style) = if self.auth_failed {
            (
                "󰅚  Authentication failed - please try again".to_string(),
                "greet_status_err",
            )
        } else if self.auth_pending {
            (
                format!(
                    "{}  Authenticating asynchronously via {}...",
                    spinner_glyph,
                    if self.mode == GreetMode::Lock {
                        "PAM"
                    } else {
                        "greetd"
                    }
                ),
                "greet_status_hint",
            )
        } else {
            (
                "󰌑  Press Enter to login   •   󱊷  Esc to clear".to_string(),
                "greet_status_hint",
            )
        };

        let styles = self.build_styles();

        if self.lua_cfg.has_custom_surface() {
            let bar_date = now.format(&self.lua_cfg.bar_date_format).to_string();
            let lua_state = lua_config::GreeterSurfaceState {
                width,
                height,
                mode: if self.mode == GreetMode::Greet {
                    "greet".to_string()
                } else {
                    "lock".to_string()
                },
                username: self.username.clone(),
                hostname: self.hostname.clone(),
                session: session_name.to_string(),
                session_name: session_name.to_string(),
                session_exec: session_exec.to_string(),
                password_dots: masked_dots.clone(),
                password_masked: masked_dots,
                password_len: self.password.chars().count(),
                password_empty: self.password.is_empty(),
                auth_pending: self.auth_pending,
                auth_failed: self.auth_failed,
                status_text: status_text.clone(),
                time: time_str.clone(),
                date: date_str.clone(),
                bar_date,
                theme: self.theme.theme.clone(),
                theme_name: self.theme.theme.clone(),
                accent: self.theme.accent.clone(),
            };
            if let Some(custom_widgets) = self.lua_cfg.build_custom_widgets(&lua_state) {
                if !custom_widgets.is_empty() {
                    return wyrd_engine::widgets::from_config(&custom_widgets, &styles);
                }
            }
        }

        let card_w = self.lua_cfg.card_width;
        let inner_w = (card_w - 56.0).max(280.0);

        let mut card_children = vec![
            WidgetConfig {
                ty: wyrd_engine::widgets::WidgetKind::Text,
                id: Some("greet_title".to_string()),
                style: Some("greet_title".to_string()),
                text: Some(title_str),
                layout: Some(LayoutConfig {
                    width: Some(inner_w),
                    height: Some(28.0),
                    justify: Some("center".to_string()),
                    align: Some("center".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            WidgetConfig {
                ty: wyrd_engine::widgets::WidgetKind::Button,
                id: Some("greet_user".to_string()),
                style: Some("greet_user".to_string()),
                text: Some(user_str),
                layout: Some(LayoutConfig {
                    width: Some((inner_w - 30.0).max(260.0)),
                    height: Some(32.0),
                    justify: Some("center".to_string()),
                    align: Some("center".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ];

        if self.mode == GreetMode::Greet && !self.sessions.is_empty() {
            card_children.push(WidgetConfig {
                ty: wyrd_engine::widgets::WidgetKind::Button,
                id: Some("greet_session".to_string()),
                style: Some("greet_session".to_string()),
                text: Some(format!("󰨡  Session: {}  (Tab)", session_name)),
                layout: Some(LayoutConfig {
                    width: Some((inner_w - 30.0).max(260.0)),
                    height: Some(32.0),
                    justify: Some("center".to_string()),
                    align: Some("center".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            });
        }

        card_children.push(WidgetConfig {
            ty: wyrd_engine::widgets::WidgetKind::Button,
            id: Some("greet_input".to_string()),
            style: Some("greet_input".to_string()),
            text: Some(input_display),
            layout: Some(LayoutConfig {
                width: Some((inner_w - 16.0).max(280.0)),
                height: Some(46.0),
                justify: Some("center".to_string()),
                align: Some("center".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        });

        card_children.push(WidgetConfig {
            ty: wyrd_engine::widgets::WidgetKind::Text,
            id: Some("greet_status".to_string()),
            style: Some(status_style.to_string()),
            text: Some(status_text),
            layout: Some(LayoutConfig {
                width: Some(inner_w),
                height: Some(22.0),
                justify: Some("center".to_string()),
                align: Some("center".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        });

        let card_height = if self.mode == GreetMode::Greet {
            286.0
        } else {
            246.0
        };

        let root_widget = if width >= 3000.0 && height > 0.0 && (width / height) > 2.8 {
            let half_w = (width / 2.0).floor();
            WidgetConfig {
                ty: wyrd_engine::widgets::WidgetKind::Container,
                id: Some("greet_root".to_string()),
                style: Some("greet_backdrop".to_string()),
                layout: Some(LayoutConfig {
                    mode: Some("flex_row".to_string()),
                    justify: Some("center".to_string()),
                    align: Some("center".to_string()),
                    width: Some(width),
                    height: Some(height),
                    ..Default::default()
                }),
                children: vec![
                    self.build_monitor_column(
                        half_w,
                        height,
                        &time_str,
                        &date_str,
                        session_name,
                        card_height,
                        card_children.clone(),
                    ),
                    self.build_monitor_column(
                        width - half_w,
                        height,
                        &time_str,
                        &date_str,
                        session_name,
                        card_height,
                        card_children,
                    ),
                ],
                ..Default::default()
            }
        } else {
            let mut single = self.build_monitor_column(
                width,
                height,
                &time_str,
                &date_str,
                session_name,
                card_height,
                card_children,
            );
            single.id = Some("greet_root".to_string());
            single
        };

        wyrd_engine::widgets::from_config(&[root_widget], &styles)
    }

    fn render_output(&mut self, qh: &QueueHandle<AppState>, global_name: u32) {
        self.poll_font_context();
        self.poll_wallpaper();

        let Some(shm) = self.shm.clone() else { return };
        let Some((width, height, surface)) = self.outputs.get(&global_name).and_then(|o| {
            o.surface
                .as_ref()
                .map(|s| (o.width.max(1), o.height.max(1), s.clone()))
        }) else {
            return;
        };

        let mut tree_opt = if self.render_ctx.is_some() {
            Some(self.build_widget_tree(width as f32, height as f32))
        } else {
            None
        };

        let scene_pixmap = if let (Some(ctx), Some(mut tree)) =
            (self.render_ctx.as_mut(), tree_opt.take())
        {
            wyrd_engine::widgets::tree::measure_tree(&mut tree, ctx, width as f32, height as f32);
            wyrd_engine::widgets::tree::layout_tree(
                &mut tree,
                0.0,
                0.0,
                width as f32,
                height as f32,
            );

            let idx = global_name as usize;
            let anim_opacity = ctx
                .animator
                .get_named_or(&format!("surf_opacity_{}", idx), 1.0);
            let mut damage = wyrd_engine::render::damage::DamageTracker::default();
            wyrd_engine::render::render_surface(
                idx,
                width,
                height,
                1.0,
                true,
                &tree,
                ctx,
                &mut damage,
                None,
                None,
                anim_opacity,
            )
            .map(|(_, p, _)| p)
        } else {
            let mut fast_pixmap = match Pixmap::new(width, height) {
                Some(p) => p,
                None => return,
            };
            let bg = wyrd_engine::widgets::parse_color(&self.theme.background)
                .unwrap_or_else(|| Color::from_rgba8(13, 6, 15, 255));
            fast_pixmap.fill(bg);
            Some(fast_pixmap)
        };

        let Some(scene_pixmap) = scene_pixmap else {
            return;
        };

        let pixmap = if let Some(ref wp_img) = self.wallpaper_img {
            if !self.backdrop_cache.contains_key(&(width, height)) {
                if let Some(backdrop) =
                    render_wallpaper_backdrop(wp_img, width, height, self.lua_cfg.wallpaper_dim)
                {
                    self.backdrop_cache.insert((width, height), backdrop);
                }
            }
            if let Some(cached_backdrop) = self.backdrop_cache.get(&(width, height)) {
                let mut composited = cached_backdrop.clone();
                composited.draw_pixmap(
                    0,
                    0,
                    scene_pixmap.as_ref(),
                    &tiny_skia::PixmapPaint::default(),
                    tiny_skia::Transform::identity(),
                    None,
                );
                composited
            } else {
                scene_pixmap
            }
        } else {
            scene_pixmap
        };

        let stride = (width * 4) as i32;
        let size = (stride * height as i32) as usize;

        if let Ok(mut file) = create_shm_file(size) {
            use std::io::Write;
            let bgra_data = wyrd_engine::render::to_wayland_bgra(pixmap.data());
            let _ = file.write_all(&bgra_data);
            let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
            let buffer = pool.create_buffer(
                0,
                width as i32,
                height as i32,
                stride,
                wl_shm::Format::Argb8888,
                qh,
                (),
            );
            surface.attach(Some(&buffer), 0, 0);
            surface.damage_buffer(0, 0, width as i32, height as i32);
            surface.commit();
            pool.destroy();

            let elapsed = self.elapsed_ms();
            if let Some(out) = self.outputs.get_mut(&global_name) {
                if !out.first_commit_logged {
                    out.first_commit_logged = true;
                    info!(
                        "[TIMING] at first successful surface.commit() for output={} at +{:.2}ms",
                        global_name, elapsed
                    );
                }
            }
        }
    }

    fn render_all_outputs(&mut self, qh: &QueueHandle<AppState>) {
        let keys = self.outputs.keys().copied().collect::<Vec<_>>();
        for key in keys {
            self.render_output(qh, key);
        }
    }
}

fn create_shm_file(size: usize) -> Result<std::fs::File> {
    let name = format!(
        "/wyrd-greet-shm-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let name_c = CString::new(name)?;
    let fd = unsafe {
        libc::shm_open(
            name_c.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    unsafe {
        libc::shm_unlink(name_c.as_ptr());
        libc::ftruncate(fd, size as libc::off_t);
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

// ------------------- Wayland Handlers -------------------

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for AppState {
    fn event(
        _state: &mut Self,
        _: &wl_compositor::WlCompositor,
        _: wl_compositor::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm::WlShm, ()> for AppState {
    fn event(
        _state: &mut Self,
        _: &wl_shm::WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for AppState {
    fn event(
        _state: &mut Self,
        _: &wl_shm_pool::WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for AppState {
    fn event(
        _state: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_buffer::Event::Release) {
            buffer.destroy();
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for AppState {
    fn event(
        _state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            if let Ok(caps) = capabilities.into_result() {
                if caps.contains(wl_seat::Capability::Keyboard) {
                    seat.get_keyboard(qh, ());
                }
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for AppState {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap { format, fd, size } => {
                if matches!(format, WEnum::Value(wl_keyboard::KeymapFormat::XkbV1)) {
                    let keymap = unsafe {
                        xkbcommon::xkb::Keymap::new_from_fd(
                            &state.xkb_context,
                            fd,
                            size as usize,
                            xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1,
                            xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
                        )
                    };
                    if let Ok(Some(keymap)) = keymap {
                        state.xkb_state = Some(xkbcommon::xkb::State::new(&keymap));
                        state.xkb_keymap = Some(keymap);
                    }
                }
            }
            wl_keyboard::Event::Key {
                key,
                state: key_state,
                ..
            } => {
                if matches!(key_state, WEnum::Value(wl_keyboard::KeyState::Pressed)) {
                    if let Some(xkb_state) = state.xkb_state.as_ref() {
                        let sym = xkb_state.key_get_one_sym(xkbcommon::xkb::Keycode::new(key + 8));
                        if sym == xkbcommon::xkb::keysyms::KEY_Return.into() {
                            state.submit_auth_async();
                        } else if sym == xkbcommon::xkb::keysyms::KEY_Tab.into() {
                            if state.mode == GreetMode::Greet && !state.sessions.is_empty() {
                                state.active_session_idx =
                                    (state.active_session_idx + 1) % state.sessions.len();
                            }
                        } else if sym == xkbcommon::xkb::keysyms::KEY_BackSpace.into() {
                            state.password.pop();
                            state.auth_failed = false;
                        } else if sym == xkbcommon::xkb::keysyms::KEY_Escape.into() {
                            state.password.zeroize();
                            state.password.clear();
                            state.auth_failed = false;
                        } else if !state.auth_pending {
                            let mut utf8 =
                                xkb_state.key_get_utf8(xkbcommon::xkb::Keycode::new(key + 8));
                            for c in utf8.chars() {
                                if !c.is_control()
                                    && state.password.len() + c.len_utf8() <= MAX_PASSWORD_BYTES
                                {
                                    state.password.push(c);
                                    state.auth_failed = false;
                                }
                            }
                            utf8.zeroize();
                        }
                        state.render_all_outputs(qh);
                    }
                }
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some(xkb_state) = state.xkb_state.as_mut() {
                    xkb_state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for AppState {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        &global_name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Mode { width, height, .. } = event {
            if let Some(out) = state.outputs.get_mut(&global_name) {
                out.width = width as u32;
                out.height = height as u32;
            }
        }
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for AppState {
    fn event(
        _state: &mut Self,
        _: &wl_surface::WlSurface,
        _: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtSessionLockManagerV1, ()> for AppState {
    fn event(
        _state: &mut Self,
        _: &ExtSessionLockManagerV1,
        _: wayland_protocols::ext::session_lock::v1::client::ext_session_lock_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtSessionLockV1, ()> for AppState {
    fn event(
        state: &mut Self,
        lock: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => {
                info!(
                    "[TIMING] at Locked event from compositor at +{:.2}ms",
                    state.elapsed_ms()
                );
                state.locked = true;
                let Some(compositor) = state.compositor.as_ref() else {
                    return;
                };

                let output_keys = state.outputs.keys().copied().collect::<Vec<_>>();
                for key in output_keys {
                    let out = state.outputs.get_mut(&key).unwrap();
                    let surface = compositor.create_surface(qh, ());
                    let lock_surface = lock.get_lock_surface(&surface, &out.output, qh, key);
                    out.surface = Some(surface);
                    out.lock_surface = Some(lock_surface);
                }
            }
            ext_session_lock_v1::Event::Finished => {
                info!("Session lock finished");
                state.locked = false;
                state.unlocked = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, u32> for AppState {
    fn event(
        state: &mut Self,
        lock_surface: &ExtSessionLockSurfaceV1,
        event: ext_session_lock_surface_v1::Event,
        &global_name: &u32,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let ext_session_lock_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        {
            lock_surface.ack_configure(serial);
            let elapsed = state.elapsed_ms();
            if let Some(out) = state.outputs.get_mut(&global_name) {
                out.width = width;
                out.height = height;
                out.configured = true;
                if !out.first_configure_logged {
                    out.first_configure_logged = true;
                    info!(
                        "[TIMING] at first Configure per output (output={}, {}x{}) at +{:.2}ms",
                        global_name, width, height, elapsed
                    );
                }
            }
            state.render_output(qh, global_name);
        }
    }
}

impl Dispatch<XdgWmBase, ()> for AppState {
    fn event(
        _state: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, u32> for AppState {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        &global_name: &u32,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            if let Some(out) = state.outputs.get_mut(&global_name) {
                out.configured = true;
            }
            state.render_output(qh, global_name);
        }
    }
}

impl Dispatch<XdgToplevel, u32> for AppState {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        &global_name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event {
            if width > 0 && height > 0 {
                if let Some(out) = state.outputs.get_mut(&global_name) {
                    out.width = width as u32;
                    out.height = height as u32;
                }
            }
        }
    }
}

fn ensure_runtime_dir() -> String {
    let euid = unsafe { libc::geteuid() };
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        let p = std::path::Path::new(&dir);
        if p.is_dir() {
            return dir;
        }
    }
    let fallback = format!("/tmp/wyrd-runtime-{}", euid);
    let p = std::path::Path::new(&fallback);
    let _ = std::fs::create_dir_all(p);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
    }
    std::env::set_var("XDG_RUNTIME_DIR", &fallback);
    fallback
}

fn main() -> Result<()> {
    let start_instant = Instant::now();
    env_logger::init();
    let mut cli = Cli::parse();
    if (std::env::var_os("GREETD_SOCK").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_none())
        && !std::env::args().any(|a| a == "lock" || a == "--mode=lock" || a == "locker")
    {
        cli.mode = GreetMode::Greet;
        if std::env::var_os("GREETD_SOCK").is_none() {
            cli.dev = true;
        }
    }

    let runtime_dir = ensure_runtime_dir();

    if cli.mode == GreetMode::Greet && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        let self_exe = std::env::current_exe()
            .unwrap_or_else(|_| std::path::PathBuf::from("/usr/bin/wyrd-greet"));
        let cage_bin = if std::path::Path::new("/usr/bin/cage").exists() {
            "/usr/bin/cage"
        } else {
            "cage"
        };
        let mut cmd = std::process::Command::new(cage_bin);
        cmd.args(["-s", "-d", "--"])
            .arg(&self_exe)
            .args(["--mode", "greet"]);
        if let Some(ref u) = cli.user {
            cmd.args(["--user", u]);
        }
        if let Some(ref c) = cli.cmd {
            cmd.args(["--cmd", c]);
        }
        if cli.dev {
            cmd.arg("--dev");
        }
        cmd.env("XDG_RUNTIME_DIR", &runtime_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let status = cmd.status().context(
            "Failed to launch Wayland kiosk compositor (cage); install `cage` (Arch: `sudo pacman -S cage`)",
        )?;
        std::process::exit(status.code().unwrap_or(0));
    }

    info!(
        "[TIMING] process start (+0.00ms) - mode: {:?} (service: {})",
        cli.mode, cli.service
    );

    let (font_tx, font_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let ctx = wyrd_engine::render::context::RenderContext::new(1.0);
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let _ = font_tx.send((ctx, elapsed_ms));
    });

    let conn = Connection::connect_to_env()
        .context("WAYLAND_DISPLAY not set; cannot connect to compositor")?;
    let (globals, mut event_queue) =
        registry_queue_init::<AppState>(&conn).context("failed to init Wayland registry")?;
    let qh = event_queue.handle();

    let compositor: wl_compositor::WlCompositor = globals
        .bind(&qh, 1..=6, ())
        .context("wl_compositor not available")?;
    let shm: wl_shm::WlShm = globals
        .bind(&qh, 1..=2, ())
        .context("wl_shm not available")?;
    let _seat: wl_seat::WlSeat = globals
        .bind(&qh, 1..=9, ())
        .context("wl_seat not available")?;
    let xdg_wm_base: Option<XdgWmBase> = globals.bind(&qh, 1..=6, ()).ok();

    let (session_lock_manager, session_lock) = if cli.mode == GreetMode::Lock {
        let lock_manager: ExtSessionLockManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .context("ext-session-lock-v1 protocol not supported by compositor")?;
        info!(
            "[TIMING] right before lock_manager.lock() at +{:.2}ms",
            start_instant.elapsed().as_secs_f64() * 1000.0
        );
        let lock = lock_manager.lock(&qh, ());
        let _ = conn.flush();
        (Some(lock_manager), Some(lock))
    } else {
        (None, None)
    };

    let greetd_client = if cli.mode == GreetMode::Greet {
        match greetd::GreetdClient::connect() {
            Ok(client) => Some(client),
            Err(e) => {
                if cli.dev {
                    warn!("Running in --dev mode without greetd socket: {}", e);
                    None
                } else {
                    eprintln!(
                        "Error: Failed to connect to greetd socket ($GREETD_SOCK): {}",
                        e
                    );
                    eprintln!("For local development/testing without greetd, run with --dev.");
                    std::process::exit(1);
                }
            }
        }
    } else {
        None
    };

    let mut state = AppState::new(
        start_instant,
        cli.mode,
        cli.service,
        cli.dev,
        greetd_client,
        Some(font_rx),
    );
    if cli.user.is_some() {
        state.username = detect_default_username(cli.user.as_deref());
    }
    if let Some(cmd) = cli.cmd.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(idx) = state
            .sessions
            .iter()
            .position(|s| s.exec.eq_ignore_ascii_case(cmd) || s.name.eq_ignore_ascii_case(cmd))
        {
            state.active_session_idx = idx;
        } else {
            state.sessions.insert(
                0,
                sessions::DesktopSession {
                    id: cmd.to_lowercase(),
                    name: cmd.to_string(),
                    exec: cmd.to_string(),
                    session_type: "wayland".to_string(),
                },
            );
            state.active_session_idx = 0;
        }
    }
    state.compositor = Some(compositor.clone());
    state.shm = Some(shm);
    state.xdg_wm_base = xdg_wm_base;
    state.session_lock_manager = session_lock_manager;
    state.session_lock = session_lock;

    for global in globals
        .contents()
        .clone_list()
        .into_iter()
        .filter(|g| g.interface == "wl_output")
    {
        let output = globals.registry().bind::<wl_output::WlOutput, _, _>(
            global.name,
            global.version.min(4),
            &qh,
            global.name,
        );
        state.outputs.insert(
            global.name,
            ScreenOutput {
                output,
                width: 1920,
                height: 1080,
                surface: None,
                lock_surface: None,
                xdg_surface: None,
                xdg_toplevel: None,
                configured: false,
                first_configure_logged: false,
                first_commit_logged: false,
            },
        );
    }

    if state.mode == GreetMode::Greet {
        let mut output_keys = state.outputs.keys().copied().collect::<Vec<_>>();
        output_keys.sort_unstable();
        if let Some(&primary_key) = output_keys.first() {
            let out = state.outputs.get_mut(&primary_key).unwrap();
            let surface = compositor.create_surface(&qh, ());
            if let Some(ref wm_base) = state.xdg_wm_base {
                let xdg_surf = wm_base.get_xdg_surface(&surface, &qh, primary_key);
                let toplevel = xdg_surf.get_toplevel(&qh, primary_key);
                toplevel.set_title("Wyrd Greeter".to_string());
                toplevel.set_app_id("wyrd-greet".to_string());
                toplevel.set_fullscreen(Some(&out.output));
                surface.commit();
                out.xdg_surface = Some(xdg_surf);
                out.xdg_toplevel = Some(toplevel);
            } else {
                out.configured = true;
            }
            out.surface = Some(surface);
            if out.configured {
                state.render_output(&qh, primary_key);
            }
        }
        let _ = conn.flush();
    }

    // Non-blocking 60fps event loop driving Wayland events, off-thread PAM/greetd results,
    // font readiness, clock updates, and wyrd-engine Animator transitions.
    loop {
        let _ = conn.flush();
        if let Some(guard) = event_queue.prepare_read() {
            let fd = guard.connection_fd().as_raw_fd();
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let timeout_ms = if state.auth_pending
                || state.render_ctx.is_none()
                || state.wallpaper_rx.is_some()
                || state
                    .render_ctx
                    .as_ref()
                    .is_some_and(|c| c.animator.has_active())
            {
                16
            } else {
                500
            };
            let poll_res = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
            if poll_res > 0 {
                let _ = guard.read();
            }
        }
        event_queue.dispatch_pending(&mut state)?;

        let mut needs_redraw = false;

        if state.poll_font_context() {
            needs_redraw = true;
        }
        if state.poll_wallpaper() {
            needs_redraw = true;
        }

        while let Ok(auth_res) = state.auth_rx.try_recv() {
            state.handle_auth_result(auth_res);
            needs_redraw = true;
        }

        if state.auth_pending {
            state.spinner_step = state.spinner_step.wrapping_add(1);
            needs_redraw = true;
        }

        if let Some(ctx) = state.render_ctx.as_mut() {
            if ctx.animator.tick(Instant::now()) || ctx.animator.has_active() {
                needs_redraw = true;
            }
        }

        let cur_min = chrono::Local::now().format("%H:%M").to_string();
        if cur_min != state.last_minute_str {
            state.last_minute_str = cur_min;
            needs_redraw = true;
        }

        if needs_redraw && !state.unlocked {
            state.render_all_outputs(&qh);
            let _ = conn.flush();
        }

        if state.unlocked && !state.locked {
            let _ = conn.flush();
            info!("Authentication complete. Exiting wyrd-greet.");
            break;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_dev_flag_parsing() {
        let cli_default = Cli::try_parse_from(["wyrd-greet", "--mode=greet"]).unwrap();
        assert_eq!(cli_default.mode, GreetMode::Greet);
        assert!(!cli_default.dev);

        let cli_dev = Cli::try_parse_from(["wyrd-greet", "--mode=greet", "--dev"]).unwrap();
        assert_eq!(cli_dev.mode, GreetMode::Greet);
        assert!(cli_dev.dev);

        let cli_lock = Cli::try_parse_from(["wyrd-greet", "--mode=lock"]).unwrap();
        assert_eq!(cli_lock.mode, GreetMode::Lock);
        assert!(!cli_lock.dev);
    }

    #[test]
    fn test_submit_auth_dev_mode_without_greetd() {
        let mut state = AppState::new(
            Instant::now(),
            GreetMode::Greet,
            "login".to_string(),
            true,
            None,
            None,
        );
        state.password.push_str("secret123");
        let success = state.submit_auth();
        assert!(success);
        assert!(state.unlocked);
        assert!(
            state.password.is_empty(),
            "Password must be zeroized and cleared"
        );
    }

    #[test]
    fn test_submit_auth_non_dev_mode_without_greetd_fails() {
        let mut state = AppState::new(
            Instant::now(),
            GreetMode::Greet,
            "login".to_string(),
            false,
            None,
            None,
        );
        state.password.push_str("secret123");
        let success = state.submit_auth();
        assert!(!success);
        assert!(!state.unlocked);
        assert!(state.auth_failed);
        assert!(
            state.password.is_empty(),
            "Password must be zeroized and cleared even on failure"
        );
    }

    #[test]
    fn test_widget_tree_and_theme_presets_build_cleanly() {
        for preset in [
            "aetheria",
            "catppuccin",
            "tokyo-night",
            "gruvbox",
            "nord",
            "dynamic",
        ] {
            let mut state = AppState::new(
                Instant::now(),
                GreetMode::Greet,
                "login".to_string(),
                true,
                None,
                None,
            );
            state.theme = ThemeState::for_preset(preset);
            state.ensure_render_ctx_for_tests();
            let mut tree = state.build_widget_tree(1920.0, 1080.0);
            let ctx = state.render_ctx.as_mut().unwrap();
            wyrd_engine::widgets::tree::measure_tree(&mut tree, ctx, 1920.0, 1080.0);
            wyrd_engine::widgets::tree::layout_tree(&mut tree, 0.0, 0.0, 1920.0, 1080.0);
            assert!(tree.root().is_some());
        }
    }

    #[test]
    fn test_lua_customization_and_wallpaper_compositing() {
        let mut state = AppState::new(
            Instant::now(),
            GreetMode::Greet,
            "login".to_string(),
            true,
            None,
            None,
        );
        state.ensure_render_ctx_for_tests();
        let _ = state.poll_wallpaper();
        if state.wallpaper_img.is_none() {
            state.wallpaper_img = Some(image::RgbaImage::from_pixel(
                64,
                64,
                image::Rgba([40, 28, 26, 255]),
            ));
        }

        let mut tree = state.build_widget_tree(1920.0, 1080.0);
        let ctx = state.render_ctx.as_mut().unwrap();
        wyrd_engine::widgets::tree::measure_tree(&mut tree, ctx, 1920.0, 1080.0);
        wyrd_engine::widgets::tree::layout_tree(&mut tree, 0.0, 0.0, 1920.0, 1080.0);
        let mut damage = wyrd_engine::render::damage::DamageTracker::default();
        let (_, scene_pixmap, _) = wyrd_engine::render::render_surface(
            0,
            1920,
            1080,
            1.0,
            true,
            &tree,
            ctx,
            &mut damage,
            None,
            None,
            1.0,
        )
        .expect("render_surface should succeed");

        let final_pixmap = if let Some(ref wp_img) = state.wallpaper_img {
            let mut backdrop =
                render_wallpaper_backdrop(wp_img, 1920, 1080, state.lua_cfg.wallpaper_dim)
                    .expect("render_wallpaper_backdrop should succeed");
            backdrop.draw_pixmap(
                0,
                0,
                scene_pixmap.as_ref(),
                &tiny_skia::PixmapPaint::default(),
                tiny_skia::Transform::identity(),
                None,
            );
            backdrop
        } else {
            scene_pixmap
        };

        assert_eq!(final_pixmap.width(), 1920);
        assert_eq!(final_pixmap.height(), 1080);

        if let Ok(preview_path) = std::env::var("WYRD_GREET_DUMP_PREVIEW") {
            let _ = final_pixmap.save_png(preview_path);
        }
    }
}

//! Lua 5.4 configuration and surface customization engine for `wyrd-greet`.
//!
//! Evaluates `/home/<user>/.config/wyrd/greeter.lua` or `/etc/wyrd/greeter.lua`,
//! providing `wyrd.theme(...)`, `wyrd.config({...})`, `wyrd.style(name, {...})`,
//! and `wyrd.surface("greeter", { build = function(state) ... end })`.

use log::{info, warn};
use mlua::{Lua, LuaSerdeExt, Table, Value as LuaValue};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use wyrd_engine::widgets::{StyleConfig, WidgetConfig};

#[derive(Debug, Clone)]
pub struct GreeterLuaConfig {
    pub font: String,
    pub placeholder: String,
    pub theme_override: Option<String>,
    pub wallpaper_override: Option<String>,
    pub wallpaper_dim: f32,
    pub show_top_bar: bool,
    pub clock_format: String,
    pub date_format: String,
    pub bar_date_format: String,
    pub title_override: Option<String>,
    pub subtitle_override: Option<String>,
    pub card_width: f32,
    pub card_radius: Option<f32>,
    pub custom_styles: HashMap<String, StyleConfig>,
    pub script_path: Option<PathBuf>,
    pub has_custom_builder: bool,
}

impl Default for GreeterLuaConfig {
    fn default() -> Self {
        Self {
            font: "MesloLGS Nerd Font, JetBrains Mono, sans-serif".to_string(),
            placeholder: "󰌋  Enter password...".to_string(),
            theme_override: None,
            wallpaper_override: None,
            wallpaper_dim: 0.56,
            show_top_bar: true,
            clock_format: "%H:%M".to_string(),
            date_format: "%A, %B %d".to_string(),
            bar_date_format: "%a, %d %b   •   %H:%M".to_string(),
            title_override: None,
            subtitle_override: None,
            card_width: 430.0,
            card_radius: Some(22.0),
            custom_styles: HashMap::new(),
            script_path: None,
            has_custom_builder: false,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GreeterSurfaceState {
    pub username: String,
    pub hostname: String,
    pub mode: String,
    pub time: String,
    pub date: String,
    pub bar_date: String,
    pub session: String,
    pub session_name: String,
    pub session_exec: String,
    pub auth_failed: bool,
    pub auth_pending: bool,
    pub password_dots: String,
    pub password_masked: String,
    pub password_len: usize,
    pub password_empty: bool,
    pub status_text: String,
    pub theme: String,
    pub theme_name: String,
    pub accent: String,
    pub width: f32,
    pub height: f32,
}

impl GreeterLuaConfig {
    pub fn load(username: &str) -> Self {
        let mut candidates = Vec::new();
        if let Ok(env_cfg) = std::env::var("WYRD_GREET_CONFIG") {
            if !env_cfg.trim().is_empty() {
                candidates.push(PathBuf::from(env_cfg));
            }
        }
        if !username.is_empty() {
            candidates.push(PathBuf::from(format!(
                "/home/{}/.config/wyrd/greeter.lua",
                username
            )));
        }
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg.trim().is_empty() {
                candidates.push(PathBuf::from(xdg).join("wyrd/greeter.lua"));
            }
        }
        if let Ok(home) = std::env::var("HOME") {
            if !home.trim().is_empty() {
                candidates.push(PathBuf::from(home).join(".config/wyrd/greeter.lua"));
            }
        }
        candidates.push(PathBuf::from("/etc/wyrd/greeter.lua"));

        for path in candidates {
            if path.is_file() {
                if let Some(cfg) = Self::eval_file(&path) {
                    info!("Loaded greeter Lua configuration from {}", path.display());
                    return cfg;
                }
            }
        }

        Self::default()
    }

    fn eval_file(path: &Path) -> Option<Self> {
        let script = std::fs::read_to_string(path).ok()?;
        let lua = Lua::new();
        let state = Arc::new(Mutex::new(Self {
            script_path: Some(path.to_path_buf()),
            ..Self::default()
        }));

        let wyrd_tbl = lua.create_table().ok()?;

        {
            let st = Arc::clone(&state);
            let theme_fn = lua
                .create_function(move |_, name: String| {
                    if let Ok(mut guard) = st.lock() {
                        guard.theme_override = Some(name);
                    }
                    Ok(())
                })
                .ok()?;
            wyrd_tbl.set("theme", theme_fn).ok()?;
        }

        {
            let st = Arc::clone(&state);
            let cfg_fn = lua
                .create_function(move |_, tbl: Table| {
                    if let Ok(mut guard) = st.lock() {
                        if let Ok(v) = tbl.get::<String>("font") {
                            if !v.trim().is_empty() {
                                guard.font = v;
                            }
                        }
                        if let Ok(v) = tbl.get::<String>("placeholder") {
                            guard.placeholder = v;
                        }
                        if let Ok(v) = tbl.get::<String>("theme") {
                            guard.theme_override = Some(v);
                        }
                        if let Ok(v) = tbl.get::<String>("wallpaper") {
                            guard.wallpaper_override = Some(v);
                        }
                        if let Ok(v) = tbl.get::<f32>("wallpaper_dim") {
                            guard.wallpaper_dim = v.clamp(0.0, 0.95);
                        }
                        if let Ok(v) = tbl.get::<f32>("backdrop_dim") {
                            guard.wallpaper_dim = v.clamp(0.0, 0.95);
                        }
                        if let Ok(v) = tbl.get::<bool>("show_top_bar") {
                            guard.show_top_bar = v;
                        }
                        if let Ok(v) = tbl.get::<String>("clock_format") {
                            guard.clock_format = v;
                        }
                        if let Ok(v) = tbl.get::<String>("date_format") {
                            guard.date_format = v;
                        }
                        if let Ok(v) = tbl.get::<String>("bar_date_format") {
                            guard.bar_date_format = v;
                        }
                        if let Ok(v) = tbl.get::<String>("title") {
                            guard.title_override = Some(v);
                        }
                        if let Ok(v) = tbl.get::<String>("subtitle") {
                            guard.subtitle_override = Some(v);
                        }
                        if let Ok(v) = tbl.get::<f32>("card_width") {
                            guard.card_width = v.clamp(300.0, 900.0);
                        }
                        if let Ok(v) = tbl.get::<f32>("card_radius") {
                            guard.card_radius = Some(v.clamp(0.0, 64.0));
                        }
                    }
                    Ok(())
                })
                .ok()?;
            wyrd_tbl.set("config", cfg_fn).ok()?;
        }

        {
            let st = Arc::clone(&state);
            let style_fn = lua
                .create_function(move |lua_ctx, (name, val): (String, LuaValue)| {
                    if let Ok(style_cfg) = lua_ctx.from_value::<StyleConfig>(val) {
                        if let Ok(mut guard) = st.lock() {
                            guard.custom_styles.insert(name, style_cfg);
                        }
                    }
                    Ok(())
                })
                .ok()?;
            wyrd_tbl.set("style", style_fn).ok()?;
        }

        {
            let st = Arc::clone(&state);
            let surface_fn = lua
                .create_function(move |_, (_name, tbl): (String, Table)| {
                    if tbl.contains_key("build").unwrap_or(false) {
                        if let Ok(mut guard) = st.lock() {
                            guard.has_custom_builder = true;
                        }
                    }
                    Ok(())
                })
                .ok()?;
            wyrd_tbl.set("surface", surface_fn).ok()?;
        }

        lua.globals().set("wyrd", wyrd_tbl).ok()?;
        if let Err(e) = lua.load(&script).exec() {
            warn!("Failed to evaluate {}: {}", path.display(), e);
            return None;
        }

        let res = state.lock().ok()?.clone();
        Some(res)
    }

    pub fn has_custom_surface(&self) -> bool {
        self.has_custom_builder
    }

    pub fn build_custom_widgets(&self, ctx: &GreeterSurfaceState) -> Option<Vec<WidgetConfig>> {
        if !self.has_custom_builder {
            return None;
        }
        let path = self.script_path.as_ref()?;
        let script = std::fs::read_to_string(path).ok()?;
        let lua = Lua::new();
        let builder_key = Arc::new(Mutex::new(None::<mlua::RegistryKey>));

        let wyrd_tbl = lua.create_table().ok()?;
        wyrd_tbl
            .set("theme", lua.create_function(|_, _: LuaValue| Ok(())).ok()?)
            .ok()?;
        wyrd_tbl
            .set("config", lua.create_function(|_, _: LuaValue| Ok(())).ok()?)
            .ok()?;
        wyrd_tbl
            .set(
                "style",
                lua.create_function(|_, (_n, _v): (String, LuaValue)| Ok(()))
                    .ok()?,
            )
            .ok()?;

        {
            let bk = Arc::clone(&builder_key);
            let surface_fn = lua
                .create_function(move |lua_ctx, (_name, tbl): (String, Table)| {
                    if let Ok(func) = tbl.get::<mlua::Function>("build") {
                        if let Ok(key) = lua_ctx.create_registry_value(func) {
                            if let Ok(mut g) = bk.lock() {
                                *g = Some(key);
                            }
                        }
                    }
                    Ok(())
                })
                .ok()?;
            wyrd_tbl.set("surface", surface_fn).ok()?;
        }

        lua.globals().set("wyrd", wyrd_tbl).ok()?;
        lua.load(&script).exec().ok()?;

        let key_opt = builder_key.lock().ok()?.take();
        let key = key_opt?;
        let func: mlua::Function = lua.registry_value(&key).ok()?;
        let lua_state = lua.to_value(ctx).ok()?;
        let ret: LuaValue = func.call(lua_state).ok()?;
        if let Ok(single) = lua.from_value::<WidgetConfig>(ret.clone()) {
            return Some(vec![single]);
        }
        lua.from_value::<Vec<WidgetConfig>>(ret).ok()
    }
}

//! Freedesktop session scanner for `--mode=greet`.

use std::fs;
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DesktopSession {
    pub id: String,
    pub name: String,
    pub exec: String,
    pub session_type: String, // "wayland" or "x11"
}

pub fn scan_available_sessions() -> Vec<DesktopSession> {
    let mut sessions = Vec::new();

    // 1. Wayland sessions (preferred)
    scan_session_dir(
        Path::new("/usr/share/wayland-sessions"),
        "wayland",
        &mut sessions,
    );
    scan_session_dir(
        Path::new("/usr/local/share/wayland-sessions"),
        "wayland",
        &mut sessions,
    );

    // 2. X11 sessions (fallback)
    scan_session_dir(Path::new("/usr/share/xsessions"), "x11", &mut sessions);
    scan_session_dir(
        Path::new("/usr/local/share/xsessions"),
        "x11",
        &mut sessions,
    );

    // If nothing found in system dirs, provide fallback defaults
    if sessions.is_empty() {
        sessions.push(DesktopSession {
            id: "hyprland".to_string(),
            name: "Hyprland".to_string(),
            exec: "Hyprland".to_string(),
            session_type: "wayland".to_string(),
        });
        sessions.push(DesktopSession {
            id: "wyrd-wm".to_string(),
            name: "Wyrd WM".to_string(),
            exec: "wyrd-wm".to_string(),
            session_type: "wayland".to_string(),
        });
    }

    sessions
}

fn executable_exists(cmd_or_path: &str) -> bool {
    let bin = cmd_or_path.split_whitespace().next().unwrap_or("").trim();
    if bin.is_empty() {
        return false;
    }
    let p = Path::new(bin);
    if p.is_absolute() {
        return p.is_file();
    }
    for dir in ["/usr/local/bin", "/usr/bin", "/bin"] {
        if Path::new(dir).join(bin).is_file() {
            return true;
        }
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(':') {
            if !dir.is_empty() && Path::new(dir).join(bin).is_file() {
                return true;
            }
        }
    }
    false
}

fn scan_session_dir(dir: &Path, session_type: &str, sessions: &mut Vec<DesktopSession>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("desktop") {
            if let Some(session) = parse_desktop_file(&path, session_type) {
                found.push(session);
            }
        }
    }
    found.sort_by(|a, b| a.id.len().cmp(&b.id.len()).then_with(|| a.id.cmp(&b.id)));
    for session in found {
        if !sessions.iter().any(|s| s.id == session.id) {
            sessions.push(session);
        }
    }
}

fn parse_desktop_file(path: &Path, session_type: &str) -> Option<DesktopSession> {
    let content = fs::read_to_string(path).ok()?;
    let id = path.file_stem()?.to_string_lossy().to_string();

    let mut name = None;
    let mut exec = None;
    let mut try_exec = None;
    let mut is_desktop_entry = false;

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line == "[Desktop Entry]" {
            is_desktop_entry = true;
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            is_desktop_entry = false;
            continue;
        }
        if !is_desktop_entry {
            continue;
        }

        if let Some((key, val)) = line.split_once('=') {
            let key = key.trim();
            let val = val.trim();
            match key {
                "Name" if name.is_none() => name = Some(val.to_string()),
                "Exec" if exec.is_none() => exec = Some(val.to_string()),
                "TryExec" if try_exec.is_none() => try_exec = Some(val.to_string()),
                _ => {}
            }
        }
    }

    let exec_str = exec?;
    if let Some(ref te) = try_exec {
        if !executable_exists(te) {
            return None;
        }
    }
    if !executable_exists(&exec_str) {
        return None;
    }

    Some(DesktopSession {
        id,
        name: name?,
        exec: exec_str,
        session_type: session_type.to_string(),
    })
}

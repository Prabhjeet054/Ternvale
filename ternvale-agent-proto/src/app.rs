//! Start-menu / packaged guest apps for host launchers.

use serde::{Deserialize, Serialize};

use crate::HexBytes;

/// Maximum apps in one `AppListOk`.
pub const MAX_APPS: usize = 256;
/// Longest app id (relative Start-menu path or AUMID).
pub const MAX_APP_ID: usize = 256;
/// Longest display name.
pub const MAX_APP_NAME: usize = 128;
/// Longest guest executable / shortcut path.
pub const MAX_APP_EXE: usize = 512;
/// Longest file extension (without a leading path).
pub const MAX_APP_EXT: usize = 16;
/// Max extensions advertised per app.
pub const MAX_APP_EXTS: usize = 16;
/// Icon bytes (same cap as window icons).
pub const MAX_APP_ICON: usize = crate::MAX_WINDOW_ICON;

/// One Start-menu shortcut or packaged app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GuestApp {
    /// Stable id (relative `.lnk` path or packaged AUMID).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Guest path of the shortcut / exe, or `shell:AppsFolder\\AUMID`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub exe: String,
    /// `start_menu` or `packaged`.
    #[serde(default, skip_serializing_if = "AppSource::is_start_menu")]
    pub source: AppSource,
    /// File extensions this app claims (no leading dot required).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    /// Small icon; `HexBytes` Debug is length only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<HexBytes>,
}

/// Where the guest found the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AppSource {
    /// Start Menu `.lnk` / Programs folder.
    #[default]
    StartMenu,
    /// Packaged / UWP (`shell:AppsFolder`).
    Packaged,
}

impl AppSource {
    /// Wire / log name.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StartMenu => "start_menu",
            Self::Packaged => "packaged",
        }
    }

    fn is_start_menu(&self) -> bool {
        matches!(self, Self::StartMenu)
    }
}

impl GuestApp {
    /// Range checks for wire-supplied fields.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(id = %self.id))]
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() || self.id.len() > MAX_APP_ID {
            return Err(format!("app id length must be 1..={MAX_APP_ID}"));
        }
        if self.id.contains('\0') || self.id.split(['/', '\\']).any(|part| part == "..") {
            return Err("app id must not contain NUL or '..'".into());
        }
        if self.name.len() > MAX_APP_NAME {
            return Err(format!("app name longer than {MAX_APP_NAME} bytes"));
        }
        if self.exe.len() > MAX_APP_EXE {
            return Err(format!("app exe longer than {MAX_APP_EXE} bytes"));
        }
        if self.exe.split(['/', '\\']).any(|part| part == "..") {
            return Err("app exe must not contain '..'".into());
        }
        if self.extensions.len() > MAX_APP_EXTS {
            return Err(format!("app extensions longer than {MAX_APP_EXTS}"));
        }
        for ext in &self.extensions {
            if ext.is_empty() || ext.len() > MAX_APP_EXT || ext.contains(['/', '\\', '\0', '.']) {
                return Err("app extension must be a bare token (txt, not .txt or a path)".into());
            }
        }
        if let Some(icon) = &self.icon {
            if icon.0.len() > MAX_APP_ICON {
                return Err(format!("app icon longer than {MAX_APP_ICON} bytes"));
            }
        }
        Ok(())
    }
}

/// Validate a full list before encoding.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(count = apps.len()))]
pub fn validate_apps(apps: &[GuestApp]) -> Result<(), String> {
    if apps.len() > MAX_APPS {
        return Err(format!("app list longer than {MAX_APPS}"));
    }
    for app in apps {
        app.validate()?;
    }
    Ok(())
}

/// `app_launch` / `host_open` target checks.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(id))]
pub fn validate_app_launch(id: &str, file: Option<&str>) -> Result<(), String> {
    if id.is_empty() || id.len() > MAX_APP_ID {
        return Err(format!("app_launch id length must be 1..={MAX_APP_ID}"));
    }
    if id.split(['/', '\\']).any(|part| part == "..") {
        return Err("app_launch id must not contain '..'".into());
    }
    if let Some(file) = file {
        if file.len() > crate::MAX_COPY_PATH {
            return Err("app_launch file path too long".into());
        }
        if !crate::is_guest_file_path(file) {
            return Err("app_launch file must be an absolute guest path without '..'".into());
        }
    }
    Ok(())
}

/// Guest → host open request (URL or path). `file:` is never a valid URL here.
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(kind))]
pub fn validate_host_open(kind: &str, target: &str) -> Result<(), String> {
    if target.is_empty() || target.len() > crate::MAX_COPY_PATH {
        return Err("host_open target length out of range".into());
    }
    if target.contains('\0') {
        return Err("host_open target must not contain NUL".into());
    }
    match kind {
        "url" | "file" => Ok(()),
        _ => Err(format!("host_open unknown kind {kind}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_menu_defaults_are_omitted() {
        let app = GuestApp {
            id: "Notepad.lnk".into(),
            name: "Notepad".into(),
            ..GuestApp::default()
        };
        app.validate().expect("ok");
        let json = serde_json::to_string(&app).expect("json");
        assert_eq!(json, r#"{"id":"Notepad.lnk","name":"Notepad"}"#);
    }

    #[test]
    fn rejects_path_escape_and_dot_extensions() {
        let mut app = GuestApp {
            id: "../evil".into(),
            name: "x".into(),
            ..GuestApp::default()
        };
        assert!(app.validate().is_err());
        app.id = "ok".into();
        app.extensions = vec![".txt".into()];
        assert!(app.validate().is_err());
        app.extensions = vec!["txt".into()];
        app.validate().expect("txt");
        assert!(validate_app_launch("id", Some("..\\x")).is_err());
        assert!(validate_host_open("url", "https://example").is_ok());
        assert!(validate_host_open("warp", "https://example").is_err());
    }
}

//! Start-menu / packaged guest app listing and launch.

use ternvale_agent_proto::{AppSource, GuestApp, MAX_APPS};

use crate::AgentError;

/// Environment override: a host directory of `.lnk` stand-ins for tests.
pub const FAKE_START_MENU_ENV: &str = "TERNVALE_FAKE_START_MENU";

/// List Start-menu shortcuts and packaged apps.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
pub fn list_guest_apps() -> Result<Vec<GuestApp>, AgentError> {
    let mut apps = Vec::new();
    if let Some(root) = std::env::var_os(FAKE_START_MENU_ENV).filter(|v| !v.is_empty()) {
        collect_lnk_tree(
            std::path::Path::new(&root),
            std::path::Path::new(&root),
            &mut apps,
        );
        tracing::info!(
            target: "ternvale::agent",
            count = apps.len(),
            "app list from fake Start Menu"
        );
        return Ok(truncate(apps));
    }
    #[cfg(windows)]
    {
        crate::win_apps::collect_start_menu(&mut apps);
        crate::win_apps::collect_packaged(&mut apps);
        tracing::info!(target: "ternvale::agent", count = apps.len(), "app list from Start Menu");
        return Ok(truncate(apps));
    }
    #[cfg(not(windows))]
    {
        tracing::debug!(target: "ternvale::agent", "no Start Menu on this OS; empty app list");
        Ok(apps)
    }
}

/// Launch a listed app, optionally opening a guest file.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(id, has_file = file.is_some()))]
pub fn launch_guest_app(id: &str, file: Option<&str>) -> Result<(), AgentError> {
    if let Some(root) = std::env::var_os(FAKE_START_MENU_ENV).filter(|v| !v.is_empty()) {
        let path = std::path::Path::new(&root).join(id);
        if path.is_file() {
            tracing::info!(
                target: "ternvale::agent",
                id,
                path = %path.display(),
                file = file.unwrap_or(""),
                "harness launch recorded"
            );
            return Ok(());
        }
        return Err(AgentError::Action {
            action: "app launch",
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "unknown app id"),
        });
    }
    #[cfg(windows)]
    {
        return crate::win_apps::launch(id, file);
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        tracing::warn!(target: "ternvale::agent", id, "app_launch ignored on this OS");
        Err(AgentError::Action {
            action: "app launch",
            source: std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "app launch needs a Windows guest",
            ),
        })
    }
}

fn truncate(mut apps: Vec<GuestApp>) -> Vec<GuestApp> {
    if apps.len() > MAX_APPS {
        tracing::warn!(
            target: "ternvale::agent",
            count = apps.len(),
            max = MAX_APPS,
            "truncating app list"
        );
        apps.truncate(MAX_APPS);
    }
    apps
}

fn collect_lnk_tree(root: &std::path::Path, dir: &std::path::Path, apps: &mut Vec<GuestApp>) {
    if apps.len() >= MAX_APPS {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if apps.len() >= MAX_APPS {
            break;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_lnk_tree(root, &path, apps);
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.to_ascii_lowercase().ends_with(".lnk") {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let id = rel.to_string_lossy().replace('\\', "/");
        let display = name.trim_end_matches(".lnk").trim_end_matches(".LNK");
        apps.push(GuestApp {
            id,
            name: display.to_string(),
            exe: path.to_string_lossy().into_owned(),
            source: AppSource::StartMenu,
            extensions: Vec::new(),
            icon: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_lnk_standins_from_env_tree() {
        let dir = std::env::temp_dir().join(format!("tv-start-{}", std::process::id()));
        let nested = dir.join("Accessories");
        std::fs::create_dir_all(&nested).expect("dir");
        std::fs::write(nested.join("Notepad.lnk"), b"stub").expect("lnk");
        std::env::set_var(FAKE_START_MENU_ENV, &dir);
        let apps = list_guest_apps().expect("list");
        std::env::remove_var(FAKE_START_MENU_ENV);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].id, "Accessories/Notepad.lnk");
        assert_eq!(apps[0].name, "Notepad");
        assert_eq!(apps[0].source, AppSource::StartMenu);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

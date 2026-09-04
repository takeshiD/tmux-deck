//! Best-effort persistence for small, runtime-only UI preferences.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::app::PresentationMode;

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct PersistedUiState {
    agent_monitor_mode: Option<String>,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

fn default_path() -> Option<PathBuf> {
    ProjectDirs::from("dev", "tkcd", "tmux-deck")
        .and_then(|dirs| dirs.state_dir().map(|dir| dir.join("ui-state.json")))
}

pub fn load_agent_monitor_mode() -> PresentationMode {
    let Some(path) = default_path() else {
        return PresentationMode::default();
    };
    match load_from(&path) {
        Ok(mode) => mode,
        Err(error) if error.kind() == io::ErrorKind::NotFound => PresentationMode::default(),
        Err(error) => {
            warn!("failed to load Agent Monitor mode: {error}");
            PresentationMode::default()
        }
    }
}

pub fn save_agent_monitor_mode(mode: PresentationMode) {
    let Some(path) = default_path() else {
        return;
    };
    if let Err(error) = save_to(&path, mode) {
        warn!("failed to persist Agent Monitor mode: {error}");
    }
}

/// Persist without blocking the terminal-owning task. A generation check
/// makes rapid Tab presses last-write-wins even if the blocking tasks start in
/// a different order.
pub fn save_agent_monitor_mode_later(mode: PresentationMode) {
    static GENERATION: AtomicU64 = AtomicU64::new(0);
    static WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    tokio::task::spawn_blocking(move || {
        let _guard = WRITE_LOCK.get_or_init(|| Mutex::new(())).lock();
        if GENERATION.load(Ordering::SeqCst) == generation {
            save_agent_monitor_mode(mode);
        }
    });
}

fn load_from(path: &Path) -> io::Result<PresentationMode> {
    let bytes = fs::read(path)?;
    let state: PersistedUiState = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(match state.agent_monitor_mode.as_deref() {
        Some("overview") => PresentationMode::Overview,
        _ => PresentationMode::Attention,
    })
}

fn save_to(path: &Path, mode: PresentationMode) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "state path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut state = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PersistedUiState>(&bytes).ok())
        .unwrap_or_default();
    state.agent_monitor_mode = Some(mode.as_str().to_string());
    let bytes = serde_json::to_vec_pretty(&state).map_err(io::Error::other)?;
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "tmux-deck-ui-state-{}-{name}.json",
            std::process::id()
        ))
    }

    #[test]
    fn mode_round_trips_and_unknown_values_fall_back() {
        let path = test_path("round-trip");
        save_to(&path, PresentationMode::Overview).unwrap();
        assert_eq!(load_from(&path).unwrap(), PresentationMode::Overview);

        fs::write(&path, br#"{"agent_monitor_mode":"future"}"#).unwrap();
        assert_eq!(load_from(&path).unwrap(), PresentationMode::Attention);

        fs::write(&path, br#"{"future_setting":true}"#).unwrap();
        save_to(&path, PresentationMode::Overview).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["future_setting"], true);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn malformed_or_unwritable_state_is_non_fatal_to_public_api() {
        let path = test_path("malformed");
        fs::write(&path, b"not json").unwrap();
        assert!(load_from(&path).is_err());
        let _ = fs::remove_file(path);

        let parent_file = test_path("not-a-directory");
        fs::write(&parent_file, b"file").unwrap();
        assert!(
            save_to(
                &parent_file.join("ui-state.json"),
                PresentationMode::Overview
            )
            .is_err()
        );
        let _ = fs::remove_file(parent_file);
    }
}

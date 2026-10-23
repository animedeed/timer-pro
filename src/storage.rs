use crate::models::{AppConfig, AppState};
use std::fs;
use std::path::PathBuf;

const CONFIG_FILE: &str = "config.json";
const DATA_FILE: &str = "data.json";

pub fn get_default_path() -> PathBuf {
    directories::ProjectDirs::from("com", "myname", "TimerPro")
        .map(|dirs| dirs.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Блокнот и PowerShell 5 сохраняют UTF-8 с BOM. serde_json такой файл не
/// разбирает, и настройки (а для data.json — вся история) молча сбрасывались
/// бы в значения по умолчанию, а при следующем сохранении затирались.
fn strip_bom(content: &str) -> &str {
    content.strip_prefix('\u{feff}').unwrap_or(content)
}

pub fn load_config() -> AppConfig {
    let path = get_default_path().join(CONFIG_FILE);
    if let Ok(content) = fs::read_to_string(path) {
        serde_json::from_str(strip_bom(&content)).unwrap_or_default()
    } else {
        AppConfig::default()
    }
}

pub fn save_config(config: &AppConfig) {
    let dir = get_default_path();
    fs::create_dir_all(&dir).ok();
    if let Ok(json) = serde_json::to_string_pretty(config) {
        fs::write(dir.join(CONFIG_FILE), json).ok();
    }
}

pub fn load_state(custom_path: Option<&PathBuf>) -> AppState {
    let dir = custom_path.cloned().unwrap_or_else(get_default_path);
    let path = dir.join(DATA_FILE);
    if let Ok(content) = fs::read_to_string(path) {
        serde_json::from_str(strip_bom(&content)).unwrap_or_default()
    } else {
        AppState::default()
    }
}

pub fn save_state(state: &AppState, custom_path: Option<&PathBuf>) {
    let dir = custom_path.cloned().unwrap_or_else(get_default_path);
    fs::create_dir_all(&dir).ok();
    if let Ok(json) = serde_json::to_string_pretty(state) {
        fs::write(dir.join(DATA_FILE), json).ok();
    }
}

/// Путь к файлу с историей — показываем его в настройках, чтобы было видно,
/// куда именно пишутся данные.
pub fn state_file_path(custom_path: Option<&PathBuf>) -> PathBuf {
    custom_path.cloned().unwrap_or_else(get_default_path).join(DATA_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_with_bom_is_read_not_reset() {
        let json = "\u{feff}{\"music_above\": true, \"block_gap\": 9.0}";
        let config: AppConfig = serde_json::from_str(strip_bom(json)).expect("BOM не должен ломать разбор");
        assert!(config.music_above);
        assert_eq!(config.block_gap, 9.0);
    }

    #[test]
    fn text_without_bom_is_untouched() {
        assert_eq!(strip_bom("{}"), "{}");
    }
}

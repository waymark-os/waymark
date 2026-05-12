use std::env;
use std::path::PathBuf;

pub fn home_dir() -> Option<PathBuf> {
    env_path("HOME").or_else(default_home_dir)
}

pub fn cache_dir() -> Option<PathBuf> {
    env_path("XDG_CACHE_HOME").or_else(|| home_dir().map(|path| path.join(".cache")))
}

pub fn config_dir() -> Option<PathBuf> {
    env_path("XDG_CONFIG_HOME").or_else(|| home_dir().map(|path| path.join(".config")))
}

pub fn config_local_dir() -> Option<PathBuf> {
    config_dir()
}

pub fn data_dir() -> Option<PathBuf> {
    env_path("XDG_DATA_HOME").or_else(|| home_dir().map(|path| path.join(".local/share")))
}

pub fn data_local_dir() -> Option<PathBuf> {
    data_dir()
}

pub fn executable_dir() -> Option<PathBuf> {
    data_dir().map(|path| path.join("bin"))
}

pub fn preference_dir() -> Option<PathBuf> {
    config_dir()
}

pub fn runtime_dir() -> Option<PathBuf> {
    env_path("XDG_RUNTIME_DIR")
}

pub fn state_dir() -> Option<PathBuf> {
    env_path("XDG_STATE_HOME").or_else(|| home_dir().map(|path| path.join(".local/state")))
}

pub fn audio_dir() -> Option<PathBuf> {
    user_dir("XDG_MUSIC_DIR", "Music")
}

pub fn desktop_dir() -> Option<PathBuf> {
    user_dir("XDG_DESKTOP_DIR", "Desktop")
}

pub fn document_dir() -> Option<PathBuf> {
    user_dir("XDG_DOCUMENTS_DIR", "Documents")
}

pub fn download_dir() -> Option<PathBuf> {
    user_dir("XDG_DOWNLOAD_DIR", "Downloads")
}

pub fn font_dir() -> Option<PathBuf> {
    data_dir().map(|path| path.join("fonts"))
}

pub fn picture_dir() -> Option<PathBuf> {
    user_dir("XDG_PICTURES_DIR", "Pictures")
}

pub fn public_dir() -> Option<PathBuf> {
    user_dir("XDG_PUBLICSHARE_DIR", "Public")
}

pub fn template_dir() -> Option<PathBuf> {
    user_dir("XDG_TEMPLATES_DIR", "Templates")
}

pub fn video_dir() -> Option<PathBuf> {
    user_dir("XDG_VIDEOS_DIR", "Videos")
}

fn env_path(name: &str) -> Option<PathBuf> {
    let value = env::var_os(name)?;
    if value.is_empty() {
        return None;
    }

    Some(PathBuf::from(value))
}

fn user_dir(env_name: &str, fallback: &str) -> Option<PathBuf> {
    env_path(env_name).or_else(|| home_dir().map(|path| path.join(fallback)))
}

fn default_home_dir() -> Option<PathBuf> {
    #[cfg(target_os = "hermit")]
    {
        return Some(PathBuf::from("/work"));
    }

    #[cfg(not(target_os = "hermit"))]
    {
        None
    }
}

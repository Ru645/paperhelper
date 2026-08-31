use std::fs;
use std::path::PathBuf;

use anyhow::Result;

const DATA_DIR: &str = ".paperhelper";

pub fn data_dir() -> PathBuf {
    PathBuf::from(DATA_DIR)
}

pub fn ensure_data_dir() -> Result<()> {
    if !data_dir().exists() {
        fs::create_dir_all(data_dir())?;
    }
    Ok(())
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.toml")
}

pub fn knowledge_path() -> PathBuf {
    data_dir().join("knowledge.json")
}

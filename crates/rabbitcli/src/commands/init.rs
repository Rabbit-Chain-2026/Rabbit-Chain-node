//! Init command - Placeholder

use crate::Result;
use rabbitapi::rpc::ComputeBackend;
use rabbitapi::ApiConfig;
use std::fs;
use std::path::Path;

pub fn init_data_dir(data_dir: &str) -> Result<()> {
    fs::create_dir_all(data_dir)
        .map_err(|e| anyhow::anyhow!("failed to create data directory {}: {}", data_dir, e))?;
    Ok(())
}

/// Load API config from JSON file.
pub fn load_api_config(path: &str) -> Result<ApiConfig> {
    let content = fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read config file {}: {}", path, e))?;
    let cfg: ApiConfig = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("failed to parse config JSON {}: {}", path, e))?;
    Ok(cfg)
}

/// Write API config to a JSON file.
pub fn write_api_config(path: &str, cfg: &ApiConfig) -> Result<()> {
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("failed to create config directory: {}", e))?;
        }
    }

    match cfg.http_rpc.compute_backend {
        ComputeBackend::Mem => {}
        ComputeBackend::RocksDb => fs::create_dir_all(&cfg.http_rpc.compute_db_path)
            .map_err(|e| anyhow::anyhow!("failed to create compute db directory: {}", e))?,
        ComputeBackend::Redb => {
            if let Some(parent) = Path::new(&cfg.http_rpc.compute_db_path).parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent).map_err(|e| {
                        anyhow::anyhow!("failed to create redb parent directory: {}", e)
                    })?;
                }
            }
        }
    }

    let json = serde_json::to_string_pretty(cfg)
        .map_err(|e| anyhow::anyhow!("failed to serialize API config: {}", e))?;
    fs::write(path, json)
        .map_err(|e| anyhow::anyhow!("failed to write config file {}: {}", path, e))?;
    Ok(())
}

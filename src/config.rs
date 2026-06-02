use std::error::Error;
use std::fs;
use std::path::Path;

use crate::models::Config;

/// The file name for the application configuration.
pub const CONFIG_FILE: &str = "config.toml";

/// Loads the application configuration from a TOML file, assuming it exists.
pub fn load_config(path: &Path) -> Result<Config, Box<dyn Error>> {
    let file_content = fs::read_to_string(path)?;
    let config: Config = toml::from_str(&file_content)?;
    Ok(config)
}

/// Saves the application configuration to a TOML file.
pub fn save_config(path: &Path, config: &Config) -> Result<(), Box<dyn Error>> {
    let file_content = toml::to_string_pretty(config)?;
    fs::write(path, file_content)?;
    println!("Configuration saved to {}.", path.display());
    Ok(())
}

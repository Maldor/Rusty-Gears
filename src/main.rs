// main.rs
// Entry point that checks config for `Use_TUI` to decide between the
// interactive TUI and the original automated/scripted workflow.
// The `-s` flag forces automated mode regardless of config.

mod models;
mod config;
mod auth;
mod manifest;
mod api;
mod download;
mod tui;

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::api::{check_for_updates, process_new_mods_file, NEW_MODS_FILE};
use crate::auth::get_valid_token;
use crate::config::{load_config, save_config, CONFIG_FILE};
use crate::download::download_file;
use crate::manifest::{
    load_local_manifest, save_local_manifest, DOWNLOAD_DIRECTORY, VERSION_MANIFEST_FILE,
};
use crate::models::{Config, LocalVersionInfo};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    let scripted = args.iter().any(|a| a == "-s");

    // The -s flag always forces automated mode.
    if scripted {
        return run_automated().await;
    }

    // Check config for Use_TUI flag.
    let config_path = PathBuf::from(CONFIG_FILE);
    if config_path.exists() {
        if let Ok(config) = load_config(&config_path) {
            if config.use_tui {
                return tui::run_tui().await;
            }
        }
    }

    // Default to automated.
    run_automated().await
}

/// The original automated workflow — unchanged from before.
async fn run_automated() -> Result<(), Box<dyn Error>> {
    let config_path = PathBuf::from(CONFIG_FILE);

    if !config_path.exists() {
        println!(
            "Configuration file '{}' not found. Creating a template...",
            config_path.display()
        );
        let default_config = Config {
            username: "your-username-here".to_string(),
            password: "your-password-here".to_string(),
            last_login: String::new(),
            last_session_token: String::new(),
            use_tui: false,
        };
        let file_content = serde_json::to_string_pretty(&default_config)?;
        fs::write(&config_path, file_content)?;
        println!("\nA template config has been created. Launching TUI so you can configure it.");
        return tui::run_tui().await;
    }

    println!(
        "Loading configuration from '{}'...",
        config_path.display()
    );
    let mut config = load_config(&config_path)?;
    let client = reqwest::Client::new();
    let token = get_valid_token(&client, &config).await?;

    if token != config.last_session_token {
        config.last_session_token = token.clone();
        config.last_login = Utc::now().to_rfc3339();
        save_config(&config_path, &config)?;
    }

    let manifest_path = PathBuf::from(DOWNLOAD_DIRECTORY).join(VERSION_MANIFEST_FILE);
    let mut local_manifest = load_local_manifest(&manifest_path, DOWNLOAD_DIRECTORY)?;

    // Process the new mods file and update the manifest before checking for updates.
    let failed_new_mods = process_new_mods_file(&client, &mut local_manifest).await?;

    let files_to_download = check_for_updates(&client, &mut local_manifest).await?;

    if files_to_download.is_empty() {
        println!("\nAll checked local files and their dependencies are up-to-date.");
    } else {
        println!(
            "\nStarting downloads for {} files...",
            files_to_download.len()
        );
        for file in &files_to_download {
            if let Err(e) = download_file(
                &client,
                file,
                DOWNLOAD_DIRECTORY,
                &config.username,
                &token,
            )
            .await
            {
                eprintln!("ERROR downloading {}: {}", file.full_new_name, e);
            } else {
                // Update the manifest with the new version info.
                let extension = Path::new(&file.full_new_name)
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("zip")
                    .to_string();

                let info = LocalVersionInfo {
                    version: file.new_version.clone(),
                    extension,
                };
                local_manifest.insert(file.base_name.clone(), info);
            }
        }
    }

    // Save the main manifest after all checks and potential updates are complete.
    println!("\nUpdate process complete.");
    save_local_manifest(&manifest_path, &local_manifest)?;

    // Overwrite New_mods.json with any mods that failed validation.
    if let Some(failed_mods) = failed_new_mods {
        println!(
            "Updating '{}' with any remaining invalid mods...",
            NEW_MODS_FILE
        );
        let new_content = serde_json::to_string_pretty(&failed_mods)?;
        fs::write(NEW_MODS_FILE, new_content)?;
        if failed_mods.is_empty() {
            println!(
                "'{}' has been cleared as all mods were processed successfully.",
                NEW_MODS_FILE
            );
        } else {
            println!(
                "'{}' has been updated. Please correct the invalid entries.",
                NEW_MODS_FILE
            );
        }
    }

    Ok(())
}

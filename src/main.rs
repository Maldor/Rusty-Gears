// main.rs
// updated

use chrono::{DateTime, Utc};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

// --- Configuration ---
// TODO: URLs are now constants, but could be moved to config.json for more flexibility.
const AUTH_URL: &str = "https://auth.factorio.com/api-login";
const FILES_URL: &str = "https://mods.factorio.com/api/mods";
const BASE_API_URL: &str = "https://mods.factorio.com";
// The directory where files will be downloaded and checked.
const DOWNLOAD_DIRECTORY: &str = "./";
// The name of the file to store local file versions.
const VERSION_MANIFEST_FILE: &str = "versions.json";
// The name of the configuration file.
const CONFIG_FILE: &str = "config.json";
// The name of the file for adding new mods.
const NEW_MODS_FILE: &str = "New_mods.json";

/// Represents the structure of the config.json file.
#[derive(Serialize, Deserialize, Debug)]
struct Config {
    #[serde(rename = "Username")]
    username: String,
    #[serde(rename = "Password")]
    password: String,
    #[serde(rename = "Last Login")]
    last_login: String,
    #[serde(rename = "Last Session token")]
    last_session_token: String,
}

/// Represents the structure of the JSON response from the authentication API.
#[derive(Deserialize, Debug)]
struct AuthResponse {
    token: String,
}

// Represents the info.json object nested within a release.
#[derive(Deserialize, Debug, Clone)]
struct InfoJson {
    #[serde(default)]
    dependencies: Vec<String>,
}

// Represents a single release within the API response, now with nested dependency info.
#[derive(Deserialize, Debug, Clone)]
struct Release {
    version: String,
    download_url: String,
    info_json: InfoJson,
}

// Represents the overall API response for a file check.
#[derive(Deserialize, Debug, Clone)]
struct ApiFileResponse {
    name: String, // Base name of the file
    releases: Vec<Release>,
}

// Struct to hold info about a local file in the manifest.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct LocalVersionInfo {
    version: String,
    extension: String,
}

// A new struct to hold the precise information for a pending download.
#[derive(Debug, Clone)]
struct FileToDownload {
    base_name: String,
    new_version: String,
    full_new_name: String,
    download_url: String,
}

/// A type alias for our version manifest. Maps a base filename to its version info.
type VersionManifest = HashMap<String, LocalVersionInfo>;


/// Loads the application configuration from a JSON file, assuming it exists.
fn load_config(path: &Path) -> Result<Config, Box<dyn Error>> {
    let file_content = fs::read_to_string(path)?;
    let config: Config = serde_json::from_str(&file_content)?;
    Ok(config)
}


/// Saves the application configuration to a JSON file.
fn save_config(path: &Path, config: &Config) -> Result<(), Box<dyn Error>> {
    let file_content = serde_json::to_string_pretty(config)?;
    fs::write(path, file_content)?;
    println!("Configuration saved to {}.", path.display());
    Ok(())
}

/// Gets a session token, reusing an existing one if it's less than 36 hours old.
async fn get_valid_token(
    client: &reqwest::Client,
    config: &Config,
) -> Result<String, Box<dyn Error>> {
    if !config.last_session_token.is_empty() && !config.last_login.is_empty() {
        if let Ok(last_login_time) = DateTime::parse_from_rfc3339(&config.last_login) {
            if Utc::now().signed_duration_since(last_login_time) < chrono::Duration::hours(36) {
                println!("Using existing session token, as it is less than 36 hours old.");
                return Ok(config.last_session_token.clone());
            }
        }
    }

    println!("Session token is missing or expired. Authenticating for a new one...");
    let params = [("username", &config.username), ("password", &config.password)];
    let response = client.post(AUTH_URL).form(&params).send().await?;

    if response.status().is_success() {
        let auth_response = response.json::<AuthResponse>().await?;
        println!("Authentication successful. New token acquired.");
        Ok(auth_response.token)
    } else {
        let status = response.status();
        let error_body = response.text().await?;
        Err(format!("Authentication failed with status: {}. Response: {}", status, error_body).into())
    }
}

/// Parses a Factorio-style dependency string to get the mod name.
/// e.g., "? base >= 1.1" -> "base"
fn parse_dependency_name(dep_str: &str) -> Option<String> {
    let mut parts = dep_str.trim().split_whitespace();
    let first_part = parts.next()?;
    
    // If the first part is a dependency marker, the name is the second part.
    if first_part == "?" || first_part == "!" || first_part == "~" {
        parts.next().map(|s| s.to_string())
    } else {
        // Otherwise, the first part is the name.
        Some(first_part.to_string())
    }
}

/// Checks for a conflicting mod, panicking if it exists or cleaning the manifest and download queue if it's missing.
fn pre_panic_check(
    local_manifest: &mut VersionManifest,
    files_to_download: &mut HashMap<String, FileToDownload>,
    conflicting_mod_name: &str,
    mod_being_checked: &str,
) {
    if let Some(conflicting_info) = local_manifest.get(conflicting_mod_name) {
        println!(
            "Conflict Detected: Mod '{}' lists '{}' as an incompatible dependency.",
            mod_being_checked, conflicting_mod_name
        );
        
        // A conflict exists in the manifest. Check if the file is actually on disk.
        let conflicting_filename = format!(
            "{}_{}.{}",
            conflicting_mod_name, conflicting_info.version, conflicting_info.extension
        );
        let conflicting_filepath = Path::new(DOWNLOAD_DIRECTORY).join(&conflicting_filename);
        
        println!(
            "Checking for presence of conflicting file: {}",
            conflicting_filepath.display()
        );

        if conflicting_filepath.exists() {
            // The file is real. This is a fatal conflict.
            panic!(
                "FATAL: Mod '{}' is incompatible with the locally present mod '{}'. Please resolve this conflict manually by removing one of them.",
                mod_being_checked, conflicting_mod_name
            );
        } else {
            // The file is not on disk, so the manifest is stale. Remove the entry and continue.
            println!(
                "Warning: Conflicting file '{}' was not found on disk. The manifest is stale. Removing entry for '{}' from manifest.",
                conflicting_filename, conflicting_mod_name
            );
            local_manifest.remove(conflicting_mod_name);

            // Also remove it from the download queue, in case it was added before this conflict was found.
            if files_to_download.remove(conflicting_mod_name).is_some() {
                println!(
                    "Also removed conflicting mod '{}' from the download queue.",
                    conflicting_mod_name
                );
            }
        }
    }
}

/// Checks for a file listing new mods, validates them, adds them to the manifest, and returns a list of failed mods.
async fn process_new_mods_file(
    client: &reqwest::Client,
    local_manifest: &mut VersionManifest,
) -> Result<Option<Vec<String>>, Box<dyn Error>> {
    let new_mods_path = Path::new(NEW_MODS_FILE);
    if !new_mods_path.exists() {
        println!("No '{}' file found, skipping.", NEW_MODS_FILE);
        return Ok(None);
    }

    println!("Found '{}', processing new mods...", NEW_MODS_FILE);
    let file_content = fs::read_to_string(new_mods_path)?;
    if file_content.trim().is_empty() {
        return Ok(Some(vec![]));
    }
    let new_mods: Vec<String> = serde_json::from_str(&file_content)?;
    
    let mut failed_mods = Vec::new();

    for mod_name in new_mods {
        if local_manifest.contains_key(&mod_name) {
            println!("Mod '{}' already exists in the manifest, skipping.", mod_name);
            continue;
        }

        println!("Validating new mod: '{}'...", &mod_name);
        let check_url = format!("{}/{}/full", FILES_URL, mod_name);
        let response = client.get(&check_url).send().await?;

        if response.status().is_success() {
            println!("'{}' is a valid mod. Adding to manifest for update check.", mod_name);
            // Add to manifest with a placeholder version to ensure it gets downloaded.
            let placeholder_info = LocalVersionInfo {
                version: "0.0.0".to_string(),
                extension: "zip".to_string(), // Assume zip, will be corrected on download.
            };
            local_manifest.insert(mod_name, placeholder_info);
        } else {
            let status = response.status();
            let error_body = response.text().await?;
            eprintln!("Error: Could not find mod '{}' on the portal (Status: {}). It will be left in {}. Response: {}", &mod_name, status, NEW_MODS_FILE, error_body);
            failed_mods.push(mod_name);
        }
    }
    
    Ok(Some(failed_mods))
}


/// Iteratively checks for updates and dependencies.
async fn check_for_updates(
    client: &reqwest::Client,
    local_manifest: &mut VersionManifest,
) -> Result<Vec<FileToDownload>, Box<dyn Error>> {
    println!("\nChecking for updates and dependencies...");
    
    let mut files_to_download = HashMap::new();
    let mut files_to_process: Vec<String> = local_manifest.keys().cloned().collect();
    let mut processed_files = HashSet::new();
    
    // Create a set of dependencies to ignore.
    let ignored_dependencies: HashSet<String> = 
        vec!["base".to_string(), "elevated-rails".to_string(), "quality".to_string(), "space-age".to_string()]
        .into_iter()
        .collect();

    while let Some(base_name) = files_to_process.pop() {
        if !processed_files.insert(base_name.clone()) {
            // Already processed this file in this run, skip.
            continue;
        }

        println!("Checking status for '{}'...", base_name);

        // Use the /full endpoint to get dependency information.
        let check_url = format!("{}/{}/full", FILES_URL, base_name);
        let response = match client.get(&check_url).send().await {
            Ok(res) => res,
            Err(e) => {
                eprintln!("Warning: Request to check '{}' failed: {}", base_name, e);
                continue;
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await?;
            eprintln!("Warning: API check for '{}' failed with status: {}. Response: {}", base_name, status, error_body);
            continue;
        }

        let api_response: ApiFileResponse = response.json().await?;

        // Find the latest release for the current file.
        let mut latest_release: Option<&Release> = None;
        let mut latest_semver: Option<Version> = None;
        for release in &api_response.releases {
            if let Ok(release_ver) = Version::parse(&release.version) {
                if latest_semver.as_ref().map_or(true, |v| &release_ver > v) {
                    latest_semver = Some(release_ver);
                    latest_release = Some(release);
                }
            }
        }

        // After finding the latest release, check its specific dependencies.
        if let (Some(latest_rel), Some(latest_v)) = (latest_release, latest_semver) {
            // Queue up any dependencies from the latest release
            for dep_string in &latest_rel.info_json.dependencies {
                // Check for conflicting/incompatible mods, which start with '!'
                if dep_string.trim().starts_with('!') {
                    if let Some(dep_name) = parse_dependency_name(dep_string) {
                        pre_panic_check(local_manifest, &mut files_to_download, &dep_name, &base_name);
                    }
                    continue; // Move to the next dependency string
                }
                
                // Ignore optional dependencies, which start with '?'
                if dep_string.trim().starts_with('?') {
                    if let Some(dep_name) = parse_dependency_name(dep_string) {
                        println!("'{}' has an optional dependency on '{}', ignoring.", base_name, dep_name);
                    }
                    continue;
                }

                if let Some(dep_name) = parse_dependency_name(dep_string) {
                    if !processed_files.contains(&dep_name) && !ignored_dependencies.contains(&dep_name) {
                        println!("'{}' depends on '{}', queuing for check...", base_name, dep_name);
                        files_to_process.push(dep_name);
                    }
                }
            }
            
            // Now, determine if this latest release needs to be downloaded.
            let needs_download = match local_manifest.get(&base_name) {
                Some(local_info) => Version::parse(&local_info.version)? < latest_v,
                None => true, // Not in manifest, so it's a new dependency that needs downloading.
            };

            if needs_download {
                // Use a placeholder extension for new files; this assumes they are zip.
                let extension = local_manifest.get(&base_name).map_or("zip", |li| &li.extension);
                let full_new_name = format!("{}_{}.{}", base_name, latest_v, extension);
                // Correctly construct the base URL without trimming the slash.
                let download_url = format!("{}{}", BASE_API_URL, latest_rel.download_url);

                println!("Queueing '{}' version {} for download.", base_name, latest_v);
                let download_info = FileToDownload {
                    base_name: base_name.clone(),
                    new_version: latest_rel.version.clone(),
                    full_new_name,
                    download_url,
                };
                // Insert into HashMap to prevent duplicate downloads.
                files_to_download.insert(base_name.clone(), download_info);
            }
        }
    }

    Ok(files_to_download.into_values().collect())
}


/// Loads the local version manifest. If not found, it creates one by parsing filenames.
fn load_local_manifest(
    manifest_path: &Path,
    download_dir: &str,
) -> Result<VersionManifest, Box<dyn Error>> {
    if manifest_path.exists() {
        println!("Loading local version manifest from {}...", manifest_path.display());
        let file_content = fs::read_to_string(manifest_path)?;
        return Ok(serde_json::from_str(&file_content)?);
    }

    println!("Version manifest not found. Creating from contents of '{}'...", download_dir);
    let mut new_manifest = VersionManifest::new();
    let download_path = Path::new(download_dir);

    if download_path.exists() && download_path.is_dir() {
        for entry in fs::read_dir(download_path)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                let full_filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if full_filename == VERSION_MANIFEST_FILE || full_filename == CONFIG_FILE { continue; }

                let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_string();

                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if let Some((name, version_str)) = stem.rsplit_once('_') {
                        if Version::parse(version_str).is_ok() {
                            println!("Found: '{}'. Adding to manifest as name: '{}', version: {}.", full_filename, name, version_str);
                            let info = LocalVersionInfo {
                                version: version_str.to_string(),
                                extension,
                            };
                            new_manifest.insert(name.to_string(), info);
                        } else {
                            eprintln!("Warning: Could not parse version '{}' for '{}'. Skipping.", version_str, full_filename);
                        }
                    } else {
                        eprintln!("Warning: Skipping '{}' as it does not match NAME_VERSION.ext format.", full_filename);
                    }
                }
            }
        }
    }

    save_local_manifest(manifest_path, &new_manifest)?;
    Ok(new_manifest)
}

/// Saves the version manifest to a JSON file.
fn save_local_manifest(path: &Path, manifest: &VersionManifest) -> Result<(), Box<dyn Error>> {
    let file_content = serde_json::to_string_pretty(manifest)?;
    fs::write(path, file_content)?;
    println!("Version manifest saved to {}.", path.display());
    Ok(())
}

/// Downloads a file and saves it with its new versioned filename.
async fn download_file(
    client: &reqwest::Client,
    file_to_download: &FileToDownload,
    directory: &str,
    username: &str,
    token: &str,
) -> Result<(), Box<dyn Error>> {
    // Correctly construct the final authenticated download URL.
    let authenticated_download_url = format!(
        "{}?username={}&token={}",
        file_to_download.download_url, username, token
    );

    println!("Downloading from {}...", authenticated_download_url);
    let response = client.get(&authenticated_download_url).send().await?;

    if !response.status().is_success() {
        let status = response.status();
        let error_body = response.text().await?;
        return Err(format!("Failed to download {}: Status {}\nResponse: {}", file_to_download.full_new_name, status, error_body).into());
    }

    let bytes = response.bytes().await?;
    fs::create_dir_all(directory)?;
    let file_path = Path::new(directory).join(&file_to_download.full_new_name);
    fs::write(&file_path, bytes)?;
    println!("Successfully saved to {}", file_path.display());
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let config_path = PathBuf::from(CONFIG_FILE);

    if !config_path.exists() {
        println!("Configuration file '{}' not found. Creating a template...", config_path.display());
        let default_config = Config {
            username: "your-username-here".to_string(),
            password: "your-password-here".to_string(),
            last_login: "".to_string(),
            last_session_token: "".to_string(),
        };
        let file_content = serde_json::to_string_pretty(&default_config)?;
        fs::write(&config_path, file_content)?;
        println!("\nA new configuration file has been created. Please fill it out and run the program again.");
        return Ok(());
    }

    println!("Loading configuration from '{}'...", config_path.display());
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
        println!("\nStarting downloads for {} files...", files_to_download.len());
        for file in &files_to_download {
            if let Err(e) = download_file(&client, file, DOWNLOAD_DIRECTORY, &config.username, &token).await {
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
        println!("Updating '{}' with any remaining invalid mods...", NEW_MODS_FILE);
        let new_content = serde_json::to_string_pretty(&failed_mods)?;
        fs::write(NEW_MODS_FILE, new_content)?;
        if failed_mods.is_empty() {
            println!("'{}' has been cleared as all mods were processed successfully.", NEW_MODS_FILE);
        } else {
            println!("'{}' has been updated. Please correct the invalid entries.", NEW_MODS_FILE);
        }
    }


    Ok(())
}

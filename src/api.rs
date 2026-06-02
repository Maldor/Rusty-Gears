use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::path::Path;

use semver::Version;

use crate::manifest::DOWNLOAD_DIRECTORY;
use crate::models::{
    ApiFileResponse, FileToDownload, LocalVersionInfo, Release, VersionManifest,
};

/// The base URL for the Factorio mods API.
pub const FILES_URL: &str = "https://mods.factorio.com/api/mods";
/// The base URL for constructing download links.
pub const BASE_API_URL: &str = "https://mods.factorio.com";
/// The name of the file for adding new mods.
pub const NEW_MODS_FILE: &str = "New_mods.json";

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

/// Checks for a conflicting mod, panicking if it exists or cleaning the manifest
/// and download queue if it's missing.
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
                "FATAL: Mod '{}' is incompatible with the locally present mod '{}'. \
                 Please resolve this conflict manually by removing one of them.",
                mod_being_checked, conflicting_mod_name
            );
        } else {
            // The file is not on disk, so the manifest is stale. Remove the entry and continue.
            println!(
                "Warning: Conflicting file '{}' was not found on disk. \
                 The manifest is stale. Removing entry for '{}' from manifest.",
                conflicting_filename, conflicting_mod_name
            );
            local_manifest.remove(conflicting_mod_name);

            // Also remove it from the download queue, in case it was added before this conflict
            // was found.
            if files_to_download.remove(conflicting_mod_name).is_some() {
                println!(
                    "Also removed conflicting mod '{}' from the download queue.",
                    conflicting_mod_name
                );
            }
        }
    }
}

/// Checks for a file listing new mods, validates them, adds them to the manifest,
/// and returns a list of failed mods.
pub async fn process_new_mods_file(
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
            println!(
                "Mod '{}' already exists in the manifest, skipping.",
                mod_name
            );
            continue;
        }

        println!("Validating new mod: '{}'...", &mod_name);
        let check_url = format!("{}/{}/full", FILES_URL, mod_name);
        let response = client.get(&check_url).send().await?;

        if response.status().is_success() {
            println!(
                "'{}' is a valid mod. Adding to manifest for update check.",
                mod_name
            );
            // Add to manifest with a placeholder version to ensure it gets downloaded.
            let placeholder_info = LocalVersionInfo {
                version: "0.0.0".to_string(),
                extension: "zip".to_string(), // Assume zip, will be corrected on download.
            };
            local_manifest.insert(mod_name, placeholder_info);
        } else {
            let status = response.status();
            let error_body = response.text().await?;
            eprintln!(
                "Error: Could not find mod '{}' on the portal (Status: {}). \
                 It will be left in {}. Response: {}",
                &mod_name, status, NEW_MODS_FILE, error_body
            );
            failed_mods.push(mod_name);
        }
    }

    Ok(Some(failed_mods))
}

/// Iteratively checks for updates and dependencies.
pub async fn check_for_updates(
    client: &reqwest::Client,
    local_manifest: &mut VersionManifest,
) -> Result<Vec<FileToDownload>, Box<dyn Error>> {
    println!("\nChecking for updates and dependencies...");

    let mut files_to_download = HashMap::new();
    let mut files_to_process: Vec<String> = local_manifest.keys().cloned().collect();
    let mut processed_files = HashSet::new();

    // Create a set of dependencies to ignore.
    let ignored_dependencies: HashSet<String> = vec![
        "base".to_string(),
        "elevated-rails".to_string(),
        "quality".to_string(),
        "space-age".to_string(),
    ]
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
            eprintln!(
                "Warning: API check for '{}' failed with status: {}. Response: {}",
                base_name, status, error_body
            );
            continue;
        }

        let api_response: ApiFileResponse = response.json().await?;

        // Find the latest release for the current file.
        let mut latest_release: Option<&Release> = None;
        let mut latest_semver: Option<Version> = None;
        for release in &api_response.releases {
            if let Ok(release_ver) = Version::parse(&release.version) {
                if latest_semver
                    .as_ref()
                    .map_or(true, |v| &release_ver > v)
                {
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
                        pre_panic_check(
                            local_manifest,
                            &mut files_to_download,
                            &dep_name,
                            &base_name,
                        );
                    }
                    continue; // Move to the next dependency string
                }

                // Ignore optional dependencies, which start with '?'
                if dep_string.trim().starts_with('?') {
                    if let Some(dep_name) = parse_dependency_name(dep_string) {
                        println!(
                            "'{}' has an optional dependency on '{}', ignoring.",
                            base_name, dep_name
                        );
                    }
                    continue;
                }

                if let Some(dep_name) = parse_dependency_name(dep_string) {
                    if !processed_files.contains(&dep_name)
                        && !ignored_dependencies.contains(&dep_name)
                    {
                        println!(
                            "'{}' depends on '{}', queuing for check...",
                            base_name, dep_name
                        );
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
                let extension = local_manifest
                    .get(&base_name)
                    .map_or("zip", |li| &li.extension);
                let full_new_name = format!("{}_{}.{}", base_name, latest_v, extension);
                // Correctly construct the base URL without trimming the slash.
                let download_url = format!("{}{}", BASE_API_URL, latest_rel.download_url);

                println!(
                    "Queueing '{}' version {} for download.",
                    base_name, latest_v
                );
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

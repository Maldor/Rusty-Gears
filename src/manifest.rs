use std::error::Error;
use std::fs;
use std::path::Path;

use semver::Version;

use crate::config::CONFIG_FILE;
use crate::models::{LocalVersionInfo, VersionManifest};

/// The directory where files will be downloaded and checked.
pub const DOWNLOAD_DIRECTORY: &str = "./";
/// The name of the file to store local file versions.
pub const VERSION_MANIFEST_FILE: &str = "versions.json";

/// Loads the local version manifest. If not found, it creates one by parsing filenames.
pub fn load_local_manifest(
    manifest_path: &Path,
    download_dir: &str,
) -> Result<VersionManifest, Box<dyn Error>> {
    if manifest_path.exists() {
        println!(
            "Loading local version manifest from {}...",
            manifest_path.display()
        );
        let file_content = fs::read_to_string(manifest_path)?;
        return Ok(serde_json::from_str(&file_content)?);
    }

    println!(
        "Version manifest not found. Creating from contents of '{}'...",
        download_dir
    );
    let mut new_manifest = VersionManifest::new();
    let download_path = Path::new(download_dir);

    if download_path.exists() && download_path.is_dir() {
        for entry in fs::read_dir(download_path)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                let full_filename = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                if full_filename == VERSION_MANIFEST_FILE || full_filename == CONFIG_FILE {
                    continue;
                }

                let extension = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();

                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if let Some((name, version_str)) = stem.rsplit_once('_') {
                        if Version::parse(version_str).is_ok() {
                            println!(
                                "Found: '{}'. Adding to manifest as name: '{}', version: {}.",
                                full_filename, name, version_str
                            );
                            let info = LocalVersionInfo {
                                version: version_str.to_string(),
                                extension,
                            };
                            new_manifest.insert(name.to_string(), info);
                        } else {
                            eprintln!(
                                "Warning: Could not parse version '{}' for '{}'. Skipping.",
                                version_str, full_filename
                            );
                        }
                    } else {
                        eprintln!(
                            "Warning: Skipping '{}' as it does not match NAME_VERSION.ext format.",
                            full_filename
                        );
                    }
                }
            }
        }
    }

    save_local_manifest(manifest_path, &new_manifest)?;
    Ok(new_manifest)
}

/// Saves the version manifest to a JSON file.
pub fn save_local_manifest(
    path: &Path,
    manifest: &VersionManifest,
) -> Result<(), Box<dyn Error>> {
    let file_content = serde_json::to_string_pretty(manifest)?;
    fs::write(path, file_content)?;
    println!("Version manifest saved to {}.", path.display());
    Ok(())
}

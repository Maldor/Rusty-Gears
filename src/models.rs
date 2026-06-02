use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Represents the structure of the config.json file.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    #[serde(rename = "Username")]
    pub username: String,
    #[serde(rename = "Password")]
    pub password: String,
    #[serde(rename = "Last Login")]
    pub last_login: String,
    #[serde(rename = "Last Session token")]
    pub last_session_token: String,
    #[serde(default)]
    #[serde(rename = "Use TUI")]
    pub use_tui: bool,
}

/// Represents the structure of the JSON response from the authentication API.
#[derive(Deserialize, Debug)]
pub struct AuthResponse {
    pub token: String,
}

/// Represents the info.json object nested within a release.
#[derive(Deserialize, Debug, Clone)]
pub struct InfoJson {
    #[serde(default)]
    pub dependencies: Vec<String>,
}

/// Represents a single release within the API response, now with nested dependency info.
#[derive(Deserialize, Debug, Clone)]
pub struct Release {
    pub version: String,
    pub download_url: String,
    pub info_json: InfoJson,
}

/// Represents the overall API response for a file check.
#[derive(Deserialize, Debug, Clone)]
pub struct ApiFileResponse {
    pub name: String, // Base name of the file
    pub releases: Vec<Release>,
}

/// Struct to hold info about a local file in the manifest.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LocalVersionInfo {
    pub version: String,
    pub extension: String,
}

/// A struct to hold the precise information for a pending download.
#[derive(Debug, Clone)]
pub struct FileToDownload {
    pub base_name: String,
    pub new_version: String,
    pub full_new_name: String,
    pub download_url: String,
}

/// A type alias for our version manifest. Maps a base filename to its version info.
pub type VersionManifest = HashMap<String, LocalVersionInfo>;

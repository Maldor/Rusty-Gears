use std::error::Error;
use std::fs;
use std::path::Path;

use chrono::{DateTime, Utc};

use crate::models::{AuthResponse, Config, PlayerData};

/// The URL for the Factorio authentication API.
pub const AUTH_URL: &str = "https://auth.factorio.com/api-login";

/// The name of Factorio's player-data file (in the CWD).
pub const PLAYER_DATA_FILE: &str = "player-data.json";

/// Reads the `service-token` from Factorio's player-data.json file.
/// Returns `Ok(None)` if the file doesn't exist or the token is empty.
pub fn load_player_data_token(path: &Path) -> Result<Option<String>, Box<dyn Error>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    let player_data: PlayerData = serde_json::from_str(&content)?;
    let token = player_data.service_token;
    if token.is_empty() {
        Ok(None)
    } else {
        Ok(Some(token))
    }
}

/// Gets a session token, reusing an existing one if it's less than 36 hours old.
///
/// Returns `(token, did_internal_auth)` where `did_internal_auth` is `true` when
/// the token was obtained by authenticating with a username and password (meaning
/// the caller should discard the stored password).
pub async fn get_valid_token(
    client: &reqwest::Client,
    config: &Config,
) -> Result<(String, bool), Box<dyn Error>> {
    // ── token_source = "player-data.json" ─────────────────────────────────
    if config.token_source == "player-data.json" {
        match load_player_data_token(Path::new(PLAYER_DATA_FILE)) {
            Ok(Some(player_token)) => {
                if player_token == config.last_session_token {
                    println!("Using existing session token (matches player-data.json).");
                    return Ok((config.last_session_token.clone(), false));
                } else {
                    println!("Using session token from player-data.json.");
                    return Ok((player_token, false));
                }
            }
            Ok(None) => {
                eprintln!(
                    "Warning: player-data.json exists but service-token is empty."
                );
            }
            Err(e) => {
                eprintln!(
                    "Warning: Could not read player-data.json: {}",
                    e
                );
            }
        }
        eprintln!(
            "Factorio's token isn't fresh, please enter a user name and password \
             for our temporary use"
        );
    }

    // ── token_source = "internal" | "" | (fallback from above) ────────────
    if !config.last_session_token.is_empty() && !config.last_login.is_empty() {
        if let Ok(last_login_time) = DateTime::parse_from_rfc3339(&config.last_login) {
            if Utc::now().signed_duration_since(last_login_time)
                < chrono::Duration::hours(36)
            {
                println!("Using existing session token, as it is less than 36 hours old.");
                return Ok((config.last_session_token.clone(), false));
            }
        }
    }

    println!("Session token is missing or expired. Authenticating for a new one...");
    let params = [
        ("username", &config.username),
        ("password", &config.password),
    ];
    let response = client.post(AUTH_URL).form(&params).send().await?;

    if response.status().is_success() {
        let auth_response = response.json::<AuthResponse>().await?;
        println!("Authentication successful. New token acquired.");
        Ok((auth_response.token, true))
    } else {
        let status = response.status();
        let error_body = response.text().await?;
        Err(format!(
            "Authentication failed with status: {}. Response: {}",
            status, error_body
        )
        .into())
    }
}

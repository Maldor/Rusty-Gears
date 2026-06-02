use std::error::Error;

use chrono::{DateTime, Utc};

use crate::models::{AuthResponse, Config};

/// The URL for the Factorio authentication API.
pub const AUTH_URL: &str = "https://auth.factorio.com/api-login";

/// Gets a session token, reusing an existing one if it's less than 36 hours old.
pub async fn get_valid_token(
    client: &reqwest::Client,
    config: &Config,
) -> Result<String, Box<dyn Error>> {
    if !config.last_session_token.is_empty() && !config.last_login.is_empty() {
        if let Ok(last_login_time) = DateTime::parse_from_rfc3339(&config.last_login) {
            if Utc::now().signed_duration_since(last_login_time)
                < chrono::Duration::hours(36)
            {
                println!("Using existing session token, as it is less than 36 hours old.");
                return Ok(config.last_session_token.clone());
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
        Ok(auth_response.token)
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

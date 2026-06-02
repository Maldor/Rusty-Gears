use std::error::Error;
use std::fs;
use std::path::Path;

use crate::models::FileToDownload;

/// Downloads a file and saves it with its new versioned filename.
pub async fn download_file(
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
        return Err(format!(
            "Failed to download {}: Status {}\nResponse: {}",
            file_to_download.full_new_name, status, error_body
        )
        .into());
    }

    let bytes = response.bytes().await?;
    fs::create_dir_all(directory)?;
    let file_path = Path::new(directory).join(&file_to_download.full_new_name);
    fs::write(&file_path, bytes)?;
    println!("Successfully saved to {}", file_path.display());
    Ok(())
}

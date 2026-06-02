// tui.rs
// Interactive TUI mode for Rusty Gears v3.
// Activated via the `-i` flag.
//
// Layout: three panels — Local Mods (left), Download Queue (center), Status Log (right).
// A bottom bar shows available keybindings.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};

use crate::api::check_for_updates;
use crate::auth::get_valid_token;
use crate::config::{load_config, save_config, CONFIG_FILE};
use crate::download::download_file;
use crate::manifest::{
    load_local_manifest, save_local_manifest, DOWNLOAD_DIRECTORY, VERSION_MANIFEST_FILE,
};
use crate::models::{Config, FileToDownload, LocalVersionInfo, VersionManifest};

// ─── Constants ───────────────────────────────────────────────────────────────

/// How often the event loop re-renders and polls for async results.
const TICK_MS: u64 = 50;

// ─── Enums ───────────────────────────────────────────────────────────────────

/// Which panel currently has keyboard focus (for up/down navigation).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    LocalMods,
    DownloadQueue,
    Log,
}

/// What the TUI is waiting for from the user.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    AddingMod,     // typing a mod name in the overlay
    EditingConfig, // editing config fields in the overlay
}

/// Which config field is currently focused in the editing overlay.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfigField {
    Username,
    Password,
}

/// Severity of a status log entry.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LogLevel {
    Info,
    Success,
    Error,
}

/// Messages flowing from async tasks (or the keyboard thread) to the event loop.
enum AppEvent {
    Key(KeyEvent),
    UpdateCheckDone {
        files: Vec<FileToDownload>,
        manifest: VersionManifest,
    },
    UpdateCheckFailed {
        manifest: VersionManifest,
        error: String,
    },
    DownloadDone {
        file: FileToDownload,
        result: Result<(), String>,
    },
    ModValidated {
        mod_name: String,
        result: Result<(), String>,
    },
    AuthDone {
        result: Result<String, String>,
    },
}

// ─── Structs ─────────────────────────────────────────────────────────────────

/// A single entry in the scrollable status log.
struct StatusMessage {
    text: String,
    level: LogLevel,
}

impl StatusMessage {
    fn info(text: impl Into<String>) -> Self {
        Self { text: text.into(), level: LogLevel::Info }
    }
    fn success(text: impl Into<String>) -> Self {
        Self { text: text.into(), level: LogLevel::Success }
    }
    fn error(text: impl Into<String>) -> Self {
        Self { text: text.into(), level: LogLevel::Error }
    }
}

/// Central application state for the TUI.
struct TuiApp {
    // ── Persistent data ──
    config: Config,
    config_path: PathBuf,
    client: reqwest::Client,
    token: Option<String>,
    manifest: VersionManifest,
    manifest_path: PathBuf,
    download_queue: Vec<FileToDownload>,
    status_log: Vec<StatusMessage>,

    // ── UI state ──
    active_panel: Panel,
    mod_list_state: ListState,
    queue_list_state: ListState,
    log_list_state: ListState,
    should_quit: bool,

    // ── Input ──
    input_mode: InputMode,
    mod_input_buffer: String,
    config_edit_username: String,
    config_edit_password: String,
    config_edit_focus: ConfigField,

    // ── Async event channel (sender cloned for spawns) ──
    event_tx: tokio::sync::mpsc::Sender<AppEvent>,
}

// ─── Public entry point ──────────────────────────────────────────────────────

pub async fn run_tui() -> Result<(), Box<dyn Error>> {
    // ── Terminal setup ──
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // ── Load persisted state ──
    let config_path = PathBuf::from(CONFIG_FILE);

    let (config, config_missing) = if config_path.exists() {
        match load_config(&config_path) {
            Ok(c) => (c, false),
            Err(e) => {
                // Print before entering TUI so the user sees it.
                eprintln!("Warning: failed to load config.json ({})", e);
                (
                    Config {
                        username: String::new(),
                        password: String::new(),
                        last_login: String::new(),
                        last_session_token: String::new(),
                        use_tui: false,
                    },
                    true,
                )
            }
        }
    } else {
        // Create a template so the user can fill it in.
        let default_config = Config {
            username: "your-username-here".to_string(),
            password: "your-password-here".to_string(),
            last_login: String::new(),
            last_session_token: String::new(),
            use_tui: false,
        };
        let content = serde_json::to_string_pretty(&default_config)?;
        fs::write(&config_path, content)?;
        (default_config, true)
    };

    let client = reqwest::Client::new();

    let manifest_path = PathBuf::from(DOWNLOAD_DIRECTORY).join(VERSION_MANIFEST_FILE);
    let manifest = load_local_manifest(&manifest_path, DOWNLOAD_DIRECTORY)?;

    // ── Event channels ──
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AppEvent>(100);

    // Spawn a thread that reads terminal keyboard events and forwards them
    // over the channel so the async event loop never blocks on input.
    let kb_tx = event_tx.clone();
    std::thread::spawn(move || loop {
        if event::poll(Duration::from_millis(200)).unwrap_or(false) {
            if let Event::Key(key) = event::read().unwrap_or(Event::Key(
                            KeyEvent::new(KeyCode::Esc, crossterm::event::KeyModifiers::NONE),
                        )) {
                if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat {
                    if kb_tx.blocking_send(AppEvent::Key(key)).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // ── Build app ──
    let mut app = TuiApp {
        config,
        config_path,
        client,
        token: None,
        manifest,
        manifest_path,
        download_queue: Vec::new(),
        status_log: vec![
            StatusMessage::info("Rusty Gears v3 — Interactive Mode"),
            StatusMessage::info("F1 Check  |  F2 Add Mod  |  F3 Auth  |  F4 Download All"),
            StatusMessage::info("c Config  |  r Reload Config  |  Tab Focus  |  q Quit"),
        ],
        active_panel: Panel::LocalMods,
        mod_list_state: ListState::default(),
        queue_list_state: ListState::default(),
        log_list_state: ListState::default(),
        should_quit: false,
        input_mode: InputMode::Normal,
        mod_input_buffer: String::new(),
        config_edit_username: String::new(),
        config_edit_password: String::new(),
        config_edit_focus: ConfigField::Username,
        event_tx,
    };

    // Select first mod in list if any exist.
    if !app.manifest.is_empty() {
        app.mod_list_state.select(Some(0));
    }

    if config_missing {
        app.log_error("config.json not found — a template was created. Press c to configure now.");
    } else if app.config.username == "your-username-here" {
        app.log_info("Default config detected. Press c to enter your credentials.");
    }

    // ── Event loop ──
    let result = run_event_loop(&mut terminal, &mut app, &mut event_rx).await;

    // ── Cleanup ──
    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

// ─── Event loop ──────────────────────────────────────────────────────────────

async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut TuiApp,
    event_rx: &mut tokio::sync::mpsc::Receiver<AppEvent>,
) -> Result<(), Box<dyn Error>> {
    loop {
        // Drain all available events without blocking.
        loop {
            match event_rx.try_recv() {
                Ok(AppEvent::Key(key)) => app.handle_key(key),
                Ok(event) => app.handle_async_event(event).await,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    app.should_quit = true;
                    break;
                }
            }
        }

        // Ensure the log auto-scrolls to the latest message.
        app.update_log_scroll();

        // Render.
        terminal.draw(|f| draw_ui(f, app))?;

        if app.should_quit {
            break;
        }

        tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
    }

    Ok(())
}

// ─── TuiApp implementation ───────────────────────────────────────────────────

impl TuiApp {
    // ── Key handling ──────────────────────────────────────────────────────

    fn handle_key(&mut self, key: KeyEvent) {
        match self.input_mode {
            InputMode::Normal => self.handle_normal_key(key),
            InputMode::AddingMod => self.handle_adding_mod_key(key),
            InputMode::EditingConfig => self.handle_editing_config_key(key),
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.should_quit = true;
            }

            // Panel navigation.
            KeyCode::Tab => {
                self.active_panel = match self.active_panel {
                    Panel::LocalMods => Panel::DownloadQueue,
                    Panel::DownloadQueue => Panel::Log,
                    Panel::Log => Panel::LocalMods,
                };
            }
            KeyCode::Up | KeyCode::Char('k') => self.navigate_up(),
            KeyCode::Down | KeyCode::Char('j') => self.navigate_down(),

            // ── Operations ──

            // F1 – Check for updates.
            KeyCode::F(1) => self.spawn_check_updates(),

            // F2 – Add new mod (opens text input overlay).
            KeyCode::F(2) => {
                self.input_mode = InputMode::AddingMod;
                self.mod_input_buffer.clear();
                self.log_info("Type the mod name and press Enter to validate. Esc to cancel.");
            }

            // F3 – Authenticate.
            KeyCode::F(3) => self.spawn_auth(),

            // F4 – Download all queued items.
            KeyCode::F(4) => self.spawn_download_all(),

            // F5 – Refresh manifest from disk.
            KeyCode::F(5) => self.refresh_manifest(),

            // c – Edit config inline.
            KeyCode::Char('c') | KeyCode::Char('C') => {
                self.config_edit_username = self.config.username.clone();
                self.config_edit_password = self.config.password.clone();
                self.config_edit_focus = ConfigField::Username;
                self.input_mode = InputMode::EditingConfig;
                self.log_info("Edit config — Tab to switch fields, Enter to save, Esc to cancel.");
            }

            // r – Reload config from disk.
            KeyCode::Char('r') | KeyCode::Char('R') => self.reload_config(),

            _ => {}
        }
    }

    fn handle_adding_mod_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.mod_input_buffer.clear();
                self.log_info("Mod add cancelled.");
            }
            KeyCode::Enter => {
                let mod_name = self.mod_input_buffer.trim().to_string();
                if mod_name.is_empty() {
                    self.log_error("Mod name cannot be empty.");
                    return;
                }
                self.input_mode = InputMode::Normal;
                self.spawn_validate_mod(mod_name);
            }
            KeyCode::Char(c) => {
                self.mod_input_buffer.push(c);
            }
            KeyCode::Backspace => {
                self.mod_input_buffer.pop();
            }
            _ => {}
        }
    }

    // ── Navigation ────────────────────────────────────────────────────────

    fn navigate_up(&mut self) {
        let (len, state) = self.active_list();
        let current = state.selected().unwrap_or(0);
        if current > 0 {
            state.select(Some(current - 1));
        }
        let _ = len; // used for bounds checking
    }

    fn navigate_down(&mut self) {
        let (len, state) = self.active_list();
        let current = state.selected().unwrap_or(0);
        if current + 1 < len {
            state.select(Some(current + 1));
        }
    }

    /// Returns `(item_count, &mut ListState)` for the currently active panel.
    fn active_list(&mut self) -> (usize, &mut ListState) {
        match self.active_panel {
            Panel::LocalMods => {
                let len = self.manifest.len();
                (len, &mut self.mod_list_state)
            }
            Panel::DownloadQueue => {
                let len = self.download_queue.len();
                (len, &mut self.queue_list_state)
            }
            Panel::Log => {
                let len = self.status_log.len();
                (len, &mut self.log_list_state)
            }
        }
    }

    /// Keep the log list state pointing at the last entry so it auto-scrolls.
    fn update_log_scroll(&mut self) {
        if !self.status_log.is_empty() {
            self.log_list_state.select(Some(self.status_log.len() - 1));
        }
    }

    // ── Logging helpers ───────────────────────────────────────────────────

    fn log_info(&mut self, msg: impl Into<String>) {
        self.status_log.push(StatusMessage::info(msg));
    }

    fn log_success(&mut self, msg: impl Into<String>) {
        self.status_log.push(StatusMessage::success(msg));
    }

    fn log_error(&mut self, msg: impl Into<String>) {
        self.status_log.push(StatusMessage::error(msg));
    }

    // ── Async task spawning ───────────────────────────────────────────────

    /// F1: spawn update check, sending back modified manifest + found files.
    fn spawn_check_updates(&mut self) {
        self.log_info("Checking for mod updates…");
        let client = self.client.clone();
        let manifest = self.manifest.clone();
        let tx = self.event_tx.clone();

        tokio::spawn(async move {
            let mut mf = manifest;
            let result = check_for_updates(&client, &mut mf).await;
            // Convert non-Send error immediately so the future is Send.
            let result = result.map_err(|e| e.to_string());
            match result {
                Ok(files) => {
                    let _ = tx
                        .send(AppEvent::UpdateCheckDone {
                            files,
                            manifest: mf,
                        })
                        .await;
                }
                Err(error) => {
                    let _ = tx
                        .send(AppEvent::UpdateCheckFailed {
                            manifest: mf,
                            error,
                        })
                        .await;
                }
            }
        });
    }

    /// F2 sub-step: validate a user-typed mod name against the Factorio API.
    fn spawn_validate_mod(&mut self, mod_name: String) {
        let client = self.client.clone();
        let tx = self.event_tx.clone();

        self.log_info(format!("Validating mod '{}'…", &mod_name));

        tokio::spawn(async move {
            let url = format!(
                "https://mods.factorio.com/api/mods/{}/full",
                mod_name
            );
            match client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    let _ = tx
                        .send(AppEvent::ModValidated {
                            mod_name,
                            result: Ok(()),
                        })
                        .await;
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let _ = tx
                        .send(AppEvent::ModValidated {
                            mod_name,
                            result: Err(format!("API returned {}: {}", status, body)),
                        })
                        .await;
                }
                Err(e) => {
                    let _ = tx
                        .send(AppEvent::ModValidated {
                            mod_name,
                            result: Err(e.to_string()),
                        })
                        .await;
                }
            }
        });
    }

    /// F3: authenticate / refresh token.
    fn spawn_auth(&mut self) {
        self.log_info("Authenticating with Factorio…");
        let client = self.client.clone();
        let config = self.config.clone();
        let tx = self.event_tx.clone();

        tokio::spawn(async move {
            let result = get_valid_token(&client, &config).await.map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::AuthDone { result }).await;
        });
    }

    /// F4: download every item in the queue sequentially.
    fn spawn_download_all(&mut self) {
        if self.download_queue.is_empty() {
            self.log_info("Download queue is empty — nothing to download.");
            return;
        }

        let token = match &self.token {
            Some(t) => t.clone(),
            None => {
                self.log_error("Not authenticated. Press F3 to log in first.");
                return;
            }
        };

        self.log_info(format!(
            "Starting download of {} file(s)…",
            self.download_queue.len()
        ));

        let client = self.client.clone();
        let queue = self.download_queue.clone();
        let username = self.config.username.clone();
        let tx = self.event_tx.clone();

        tokio::spawn(async move {
            for file in &queue {
                let file = file.clone();
                let result = download_file(&client, &file, DOWNLOAD_DIRECTORY, &username, &token)
                    .await
                    .map_err(|e| e.to_string());
                let _ = tx
                    .send(AppEvent::DownloadDone {
                        file,
                        result,
                    })
                    .await;
            }
            let _ = tx.send(AppEvent::UpdateCheckDone {
                files: Vec::new(),
                manifest: VersionManifest::new(),
            })
            .await;
        });

        // Clear the queue immediately so it's not double-processed.
        self.download_queue.clear();
    }

    fn handle_editing_config_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.log_info("Config edit cancelled.");
            }
            KeyCode::Enter => {
                self.config.username = self.config_edit_username.clone();
                self.config.password = self.config_edit_password.clone();
                if let Err(e) = save_config(&self.config_path, &self.config) {
                    self.log_error(format!("Failed to save config: {}", e));
                } else {
                    self.log_success("Config saved to disk.");
                }
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Tab => {
                self.config_edit_focus = match self.config_edit_focus {
                    ConfigField::Username => ConfigField::Password,
                    ConfigField::Password => ConfigField::Username,
                };
            }
            KeyCode::Char(c) => {
                let buf = match self.config_edit_focus {
                    ConfigField::Username => &mut self.config_edit_username,
                    ConfigField::Password => &mut self.config_edit_password,
                };
                buf.push(c);
            }
            KeyCode::Backspace => {
                let buf = match self.config_edit_focus {
                    ConfigField::Username => &mut self.config_edit_username,
                    ConfigField::Password => &mut self.config_edit_password,
                };
                buf.pop();
            }
            _ => {}
        }
    }

    // ── Sync operations ───────────────────────────────────────────────────

    fn refresh_manifest(&mut self) {
        match load_local_manifest(&self.manifest_path, DOWNLOAD_DIRECTORY) {
            Ok(mf) => {
                self.manifest = mf;
                self.log_success("Manifest reloaded from disk.");
                if !self.manifest.is_empty() {
                    self.mod_list_state.select(Some(0));
                } else {
                    self.mod_list_state.select(None);
                }
            }
            Err(e) => {
                self.log_error(format!("Failed to reload manifest: {}", e));
            }
        }
    }

    fn reload_config(&mut self) {
        match load_config(&self.config_path) {
            Ok(c) => {
                self.config = c;
                self.log_success("Config reloaded from disk.");
            }
            Err(e) => {
                self.log_error(format!("Failed to reload config: {}", e));
            }
        }
    }

    // ── Async event handling ──────────────────────────────────────────────

    async fn handle_async_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::UpdateCheckDone { files, manifest } => {
                self.manifest = manifest;
                if files.is_empty() {
                    self.log_success("All mods are up-to-date.");
                } else {
                    self.download_queue = files;
                    self.log_success(format!(
                        "Update check complete — {} update(s) queued.",
                        self.download_queue.len()
                    ));
                    if !self.download_queue.is_empty() {
                        self.queue_list_state.select(Some(0));
                    }
                }
            }
            AppEvent::UpdateCheckFailed { manifest, error } => {
                self.manifest = manifest;
                self.log_error(format!("Update check failed: {}", error));
            }
            AppEvent::DownloadDone { file, result } => {
                match result {
                    Ok(()) => {
                        // Update the manifest with the new version.
                        let extension = Path::new(&file.full_new_name)
                            .extension()
                            .and_then(|s| s.to_str())
                            .unwrap_or("zip")
                            .to_string();
                        let info = LocalVersionInfo {
                            version: file.new_version.clone(),
                            extension,
                        };
                        self.manifest.insert(file.base_name.clone(), info);
                        self.log_success(format!("Downloaded {}", file.full_new_name));

                        // Persist manifest after each successful download.
                        if let Err(e) =
                            save_local_manifest(&self.manifest_path, &self.manifest)
                        {
                            self.log_error(format!("Failed to save manifest: {}", e));
                        }
                    }
                    Err(e) => {
                        self.log_error(format!("Failed to download {}: {}", file.full_new_name, e));
                    }
                }
            }
            AppEvent::ModValidated { mod_name, result } => {
                match result {
                    Ok(()) => {
                        let placeholder = LocalVersionInfo {
                            version: "0.0.0".to_string(),
                            extension: "zip".to_string(),
                        };
                        self.manifest.insert(mod_name.clone(), placeholder);
                        self.log_success(format!("Mod '{}' added to manifest.", mod_name));
                        if !self.manifest.is_empty() {
                            self.mod_list_state.select(Some(0));
                        }
                    }
                    Err(e) => {
                        self.log_error(format!("Mod '{}' is invalid: {}", mod_name, e));
                    }
                }
            }
            AppEvent::AuthDone { result } => {
                match result {
                    Ok(token) => {
                        self.token = Some(token.clone());
                        self.config.last_session_token = token;
                        self.config.last_login = Utc::now().to_rfc3339();
                        if let Err(e) = save_config(&self.config_path, &self.config) {
                            self.log_error(format!("Failed to save token to config: {}", e));
                        } else {
                            self.log_success("Authentication successful — token saved.");
                        }
                    }
                    Err(e) => {
                        self.log_error(format!("Authentication failed: {}", e));
                    }
                }
            }

            AppEvent::Key(_) => {} // handled separately
        }
    }
}

// ─── UI rendering ────────────────────────────────────────────────────────────

fn draw_ui(f: &mut Frame, app: &mut TuiApp) {
    let area = f.size();

    // ── Outer layout: title, body, bottom bar ──
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),  // title bar
            Constraint::Min(1),     // panels
            Constraint::Length(1),  // bottom bar
        ])
        .split(area);

    // Title bar.
    let title_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let title = Paragraph::new(Line::from(Span::styled(
        " Rusty Gears v3 — Interactive Mode ",
        title_style,
    )));
    f.render_widget(title, outer[0]);

    // ── Body: three panels ──
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(30),
            Constraint::Percentage(30),
            Constraint::Percentage(40),
        ])
        .split(outer[1]);

    app.mod_list_state = render_mods_panel(f, app.mod_list_state.clone(), app, body[0]);
    app.queue_list_state = render_queue_panel(f, app.queue_list_state.clone(), app, body[1]);
    app.log_list_state = render_log_panel(f, app.log_list_state.clone(), app, body[2]);

    // Bottom bar with keybindings.
    let bar_style = Style::default().fg(Color::Black).bg(Color::Cyan);
    let bar_text = Line::from(Span::styled(
        " F1 Check  |  F2 Add Mod  |  F3 Auth  |  F4 Download All  |  c Config  |  r Reload  |  Tab Focus  |  q Quit ",
        bar_style,
    ));
    let bar = Paragraph::new(bar_text);
    f.render_widget(bar, outer[2]);

    // ── Overlays ──
    match app.input_mode {
        InputMode::AddingMod => draw_mod_input_overlay(f, app, area),
        InputMode::EditingConfig => draw_config_overlay(f, app, area),
        InputMode::Normal => {}
    }
}

/// Left panel: local mods list.
fn render_mods_panel(
    f: &mut Frame,
    mut state: ListState,
    app: &TuiApp,
    area: Rect,
) -> ListState {
    let is_active = app.active_panel == Panel::LocalMods;
    let border_style = active_border_style(is_active);

    let mut mod_names: Vec<(&String, &LocalVersionInfo)> = app.manifest.iter().collect();
    mod_names.sort_by(|a, b| a.0.cmp(b.0));

    let items: Vec<ListItem> = mod_names
        .iter()
        .map(|(name, info)| {
            let line = Line::from(vec![
                Span::raw(*name),
                Span::raw("  "),
                Span::styled(
                    format!("v{}", info.version),
                    Style::default().fg(Color::Yellow),
                ),
            ]);
            ListItem::new(line)
        })
        .collect();

    let count = mod_names.len();
    let title = format!(" Local Mods ({}) ", count);

    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL).border_style(border_style))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(if is_active { Color::Cyan } else { Color::DarkGray }),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(list, area, &mut state);
    state
}

/// Center panel: download queue.
fn render_queue_panel(
    f: &mut Frame,
    mut state: ListState,
    app: &TuiApp,
    area: Rect,
) -> ListState {
    let is_active = app.active_panel == Panel::DownloadQueue;
    let border_style = active_border_style(is_active);

    let items: Vec<ListItem> = if app.download_queue.is_empty() {
        vec![ListItem::new(
            Line::from(Span::styled(
                " None ",
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            )),
        )]
    } else {
        app.download_queue
            .iter()
            .map(|f| {
                let line = Line::from(vec![
                    Span::raw(&f.base_name),
                    Span::raw("  "),
                    Span::styled(
                        format!("v{}", f.new_version),
                        Style::default().fg(Color::Green),
                    ),
                ]);
                ListItem::new(line)
            })
            .collect()
    };

    let count = app.download_queue.len();
    let title = format!(" Download Queue ({}) ", count);

    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL).border_style(border_style))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(if is_active { Color::Cyan } else { Color::DarkGray }),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(list, area, &mut state);
    state
}

/// Right panel: scrollable status log.
fn render_log_panel(
    f: &mut Frame,
    mut state: ListState,
    app: &TuiApp,
    area: Rect,
) -> ListState {
    let is_active = app.active_panel == Panel::Log;
    let border_style = active_border_style(is_active);

    let items: Vec<ListItem> = app
        .status_log
        .iter()
        .map(|msg| {
            let style = match msg.level {
                LogLevel::Info => Style::default().fg(Color::White),
                LogLevel::Success => Style::default().fg(Color::Green),
                LogLevel::Error => Style::default().fg(Color::Red),
            };
            ListItem::new(Line::from(Span::styled(&msg.text, style)))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Status Log ")
                .borders(Borders::ALL)
                .border_style(border_style),
        )
        .highlight_style(Style::default().bg(Color::DarkGray));

    f.render_stateful_widget(list, area, &mut state);
    state
}

/// Overlay shown when the user is typing a mod name to add.
fn draw_mod_input_overlay(f: &mut Frame, app: &TuiApp, area: Rect) {
    // Dimmed backdrop.
    f.render_widget(Clear, area);

    // Pop-up area centred both ways.
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Length(3),
            Constraint::Percentage(40),
        ])
        .split(area)[1];

    let popup = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(50),
            Constraint::Percentage(25),
        ])
        .split(popup)[1];

    // Clear the popup area first, then render the input widget.
    f.render_widget(Clear, popup);

    let input = Paragraph::new(app.mod_input_buffer.as_str())
        .block(
            Block::default()
                .title(" Add New Mod ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .style(Style::default().fg(Color::White));

    f.render_widget(input, popup);

    // Show a cursor at the end of the input.
    let cursor_x = popup.x + 1 + app.mod_input_buffer.len() as u16;
    let cursor_y = popup.y + 1;
    // Clamp to the popup interior.
    let cursor_x = cursor_x.min(popup.x + popup.width.saturating_sub(2));
    f.set_cursor(cursor_x, cursor_y);
}

/// Overlay shown when the user is editing the config (Username / Password).
fn draw_config_overlay(f: &mut Frame, app: &TuiApp, area: Rect) {
    // Dimmed backdrop.
    f.render_widget(Clear, area);

    // Pop-up area centred both ways (5 lines tall).
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(35),
            Constraint::Length(7),
            Constraint::Percentage(35),
        ])
        .split(area)[1];

    let popup = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(50),
            Constraint::Percentage(25),
        ])
        .split(popup)[1];

    f.render_widget(Clear, popup);

    let username_focused = app.config_edit_focus == ConfigField::Username;
    let password_focused = app.config_edit_focus == ConfigField::Password;
    let input_style = Style::default().fg(Color::Cyan);
    let label_style = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);

    let username_line = Line::from(vec![
        Span::styled(" Username: ", label_style),
        Span::styled(
            &app.config_edit_username,
            if username_focused {
                input_style
            } else {
                Style::default().fg(Color::Gray)
            },
        ),
    ]);

    // Mask the password with asterisks while keeping the actual buffer intact.
    let masked_password: String = "*".repeat(app.config_edit_password.len());
    let password_line = Line::from(vec![
        Span::styled(" Password: ", label_style),
        Span::styled(
            &masked_password,
            if password_focused {
                input_style
            } else {
                Style::default().fg(Color::Gray)
            },
        ),
    ]);

    let help_line = Line::from(Span::styled(
        " Tab: switch field  |  Enter: save  |  Esc: cancel ",
        Style::default().fg(Color::DarkGray),
    ));

    let lines = vec![username_line, password_line, Line::from(""), help_line];

    let block = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" Edit Config ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .style(Style::default().fg(Color::White));

    f.render_widget(block, popup);

    // Place the cursor on the focused field.
    let cursor_line = match app.config_edit_focus {
        ConfigField::Username => 1,
        ConfigField::Password => 2,
    };
    let buf = match app.config_edit_focus {
        ConfigField::Username => &app.config_edit_username,
        ConfigField::Password => &app.config_edit_password,
    };
    let cursor_x = popup.x + 1 + 11 + buf.len() as u16; // " Username: " = 11 chars
    let cursor_y = popup.y + cursor_line;
    let cursor_x = cursor_x.min(popup.x + popup.width.saturating_sub(2));
    f.set_cursor(cursor_x, cursor_y);
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn active_border_style(is_active: bool) -> Style {
    if is_active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

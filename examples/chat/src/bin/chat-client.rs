//! TUI Chat Client using ratatui.
//!
//! A modern terminal-based chat client with:
//! - Room list on the left
//! - Chat messages in the center
//! - User list with presence on the right
//!
//! # Usage
//!
//! ```bash
//! cargo run --bin chat-client -- --port 9999
//! ```
//!
//! # Configuration
//!
//! The client looks for config in:
//! 1. `--config <path>` argument
//! 2. `~/.config/starlang-chat/config.toml`
//! 3. Built-in defaults
//!
//! # Keybindings
//!
//! - `Tab` - Cycle focus between panels
//! - `Enter` - Send message / Join selected room
//! - `Up/Down` - Navigate lists
//! - `Ctrl+C` or `Esc` - Quit
//! - `/nick <name>` - Set nickname
//! - `/join <room>` - Join a room
//! - `/leave` - Leave current room

#![deny(warnings)]
#![deny(missing_docs)]

use chrono::{Local, TimeZone};
use clap::Parser;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use serde::Deserialize;
use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
};

// Include the protocol module from the main crate
#[path = "../protocol.rs"]
mod protocol;

use protocol::{ClientCommand, HistoryMessage, RoomInfo, ServerEvent, frame_message, parse_frame};

/// Color configuration from TOML
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ColorValue {
    Named(String),
    Rgb { r: u8, g: u8, b: u8 },
}

impl ColorValue {
    fn to_color(&self) -> Color {
        match self {
            ColorValue::Named(name) => parse_color_name(name),
            ColorValue::Rgb { r, g, b } => Color::Rgb(*r, *g, *b),
        }
    }
}

fn parse_color_name(s: &str) -> Color {
    let s = s.trim().to_lowercase();
    // Handle hex colors
    if s.starts_with('#') || s.chars().all(|c| c.is_ascii_hexdigit()) {
        let hex = s.trim_start_matches('#');
        if hex.len() == 6 {
            if let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&hex[0..2], 16),
                u8::from_str_radix(&hex[2..4], 16),
                u8::from_str_radix(&hex[4..6], 16),
            ) {
                return Color::Rgb(r, g, b);
            }
        }
    }
    // Named colors
    match s.as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::White,
        "gray" | "grey" => Color::Gray,
        "dark_gray" | "darkgray" | "dark_grey" | "darkgrey" => Color::DarkGray,
        "light_red" | "lightred" => Color::LightRed,
        "light_green" | "lightgreen" => Color::LightGreen,
        "light_yellow" | "lightyellow" => Color::LightYellow,
        "light_blue" | "lightblue" => Color::LightBlue,
        "light_magenta" | "lightmagenta" => Color::LightMagenta,
        "light_cyan" | "lightcyan" => Color::LightCyan,
        _ => Color::White, // fallback
    }
}

/// Theme configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
struct ThemeConfig {
    border_focused: ColorValue,
    border_unfocused: ColorValue,
    title_focused: ColorValue,
    title_unfocused: ColorValue,
    highlight_bg: ColorValue,
    highlight_fg: ColorValue,
    user_online: ColorValue,
    user_self: ColorValue,
    room_active: ColorValue,
    room_unread: ColorValue,
    room_normal: ColorValue,
    message_nick: ColorValue,
    message_system: ColorValue,
    message_text: ColorValue,
    message_timestamp: ColorValue,
    status_connected: ColorValue,
    status_disconnected: ColorValue,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            border_focused: ColorValue::Named("yellow".to_string()),
            border_unfocused: ColorValue::Named("dark_gray".to_string()),
            title_focused: ColorValue::Named("yellow".to_string()),
            title_unfocused: ColorValue::Named("white".to_string()),
            highlight_bg: ColorValue::Named("#3a3a5a".to_string()),
            highlight_fg: ColorValue::Named("white".to_string()),
            user_online: ColorValue::Named("green".to_string()),
            user_self: ColorValue::Named("green".to_string()),
            room_active: ColorValue::Named("green".to_string()),
            room_unread: ColorValue::Named("yellow".to_string()),
            room_normal: ColorValue::Named("white".to_string()),
            message_nick: ColorValue::Named("cyan".to_string()),
            message_system: ColorValue::Named("dark_gray".to_string()),
            message_text: ColorValue::Named("white".to_string()),
            message_timestamp: ColorValue::Named("dark_gray".to_string()),
            status_connected: ColorValue::Named("cyan".to_string()),
            status_disconnected: ColorValue::Named("red".to_string()),
        }
    }
}

/// Resolved theme colors for rendering
#[derive(Debug, Clone)]
struct Theme {
    border_focused: Color,
    border_unfocused: Color,
    title_focused: Color,
    title_unfocused: Color,
    highlight_bg: Color,
    highlight_fg: Color,
    user_online: Color,
    user_self: Color,
    room_active: Color,
    room_unread: Color,
    room_normal: Color,
    message_nick: Color,
    message_system: Color,
    message_text: Color,
    message_timestamp: Color,
    status_connected: Color,
    status_disconnected: Color,
}

impl From<ThemeConfig> for Theme {
    fn from(cfg: ThemeConfig) -> Self {
        Self {
            border_focused: cfg.border_focused.to_color(),
            border_unfocused: cfg.border_unfocused.to_color(),
            title_focused: cfg.title_focused.to_color(),
            title_unfocused: cfg.title_unfocused.to_color(),
            highlight_bg: cfg.highlight_bg.to_color(),
            highlight_fg: cfg.highlight_fg.to_color(),
            user_online: cfg.user_online.to_color(),
            user_self: cfg.user_self.to_color(),
            room_active: cfg.room_active.to_color(),
            room_unread: cfg.room_unread.to_color(),
            room_normal: cfg.room_normal.to_color(),
            message_nick: cfg.message_nick.to_color(),
            message_system: cfg.message_system.to_color(),
            message_text: cfg.message_text.to_color(),
            message_timestamp: cfg.message_timestamp.to_color(),
            status_connected: cfg.status_connected.to_color(),
            status_disconnected: cfg.status_disconnected.to_color(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        ThemeConfig::default().into()
    }
}

/// Display configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
struct DisplayConfig {
    /// Timestamp format (strftime-style). Set to empty string to disable.
    /// Examples: "%H:%M", "%H:%M:%S", "%Y-%m-%d %H:%M"
    timestamp_format: String,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            timestamp_format: "%H:%M".to_string(),
        }
    }
}

/// Full config file structure
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct Config {
    theme: ThemeConfig,
    display: DisplayConfig,
}

impl Config {
    fn load(path: Option<&PathBuf>) -> Self {
        // Try explicit path first
        if let Some(p) = path {
            if let Ok(contents) = std::fs::read_to_string(p) {
                if let Ok(cfg) = toml::from_str(&contents) {
                    return cfg;
                }
            }
        }

        // Try default config location
        if let Some(config_dir) = dirs::config_dir() {
            let default_path = config_dir.join("starlang-chat").join("config.toml");
            if let Ok(contents) = std::fs::read_to_string(&default_path) {
                if let Ok(cfg) = toml::from_str(&contents) {
                    return cfg;
                }
            }
        }

        // Fall back to defaults
        Config::default()
    }
}

/// Starlang Chat Client
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Server port to connect to
    #[arg(short, long, default_value = "9999")]
    port: u16,

    /// Server host to connect to
    #[arg(short = 'H', long, default_value = "127.0.0.1")]
    host: String,

    /// Path to config file
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Nickname to set on connect
    #[arg(short, long)]
    nick: Option<String>,

    /// Room to join on connect
    #[arg(short, long)]
    room: Option<String>,
}

/// Which panel is currently focused
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Rooms,
    Chat,
    Users,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Focus::Rooms => Focus::Chat,
            Focus::Chat => Focus::Users,
            Focus::Users => Focus::Rooms,
        }
    }
}

/// A chat message
#[derive(Debug, Clone)]
struct ChatMessage {
    from: String,
    text: String,
    /// Unix timestamp in seconds (if available from history)
    timestamp: Option<u64>,
    is_system: bool,
}

/// Room state
#[derive(Debug, Default)]
struct RoomState {
    messages: Vec<ChatMessage>,
    users: Vec<String>,
    unread: usize,
}

/// Application state
struct App {
    /// Server address we're connected to
    server_addr: String,
    /// Theme colors
    theme: Theme,
    /// Timestamp format (strftime-style, empty to disable)
    timestamp_format: String,
    /// Current nickname
    nick: Option<String>,
    /// Available rooms
    rooms: Vec<RoomInfo>,
    /// Room list selection state
    room_list_state: ListState,
    /// Currently selected/active room
    current_room: Option<String>,
    /// State per room
    room_states: HashMap<String, RoomState>,
    /// User list selection state
    user_list_state: ListState,
    /// Current input buffer
    input: String,
    /// Cursor position in input
    cursor_pos: usize,
    /// Which panel is focused
    focus: Focus,
    /// Status message to display
    status: Option<(String, Instant)>,
    /// Message scroll offset
    scroll_offset: usize,
    /// Whether we're connected
    connected: bool,
    /// Show help popup
    show_help: bool,
}

impl App {
    fn new(server_addr: String, theme: Theme, timestamp_format: String) -> Self {
        Self {
            server_addr,
            theme,
            timestamp_format,
            nick: None,
            rooms: vec![],
            room_list_state: ListState::default(),
            current_room: None,
            room_states: HashMap::new(),
            user_list_state: ListState::default(),
            input: String::new(),
            cursor_pos: 0,
            focus: Focus::Chat,
            status: None,
            scroll_offset: 0,
            connected: false,
            show_help: false,
        }
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    fn add_system_message(&mut self, room: &str, text: impl Into<String>) {
        let state = self.room_states.entry(room.to_string()).or_default();
        state.messages.push(ChatMessage {
            from: "***".to_string(),
            text: text.into(),
            timestamp: None,
            is_system: true,
        });
        if self.current_room.as_deref() != Some(room) {
            state.unread += 1;
        }
    }

    fn add_chat_message(&mut self, room: &str, from: &str, text: &str) {
        let state = self.room_states.entry(room.to_string()).or_default();
        // Use current time for live messages
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        state.messages.push(ChatMessage {
            from: from.to_string(),
            text: text.to_string(),
            timestamp: Some(now),
            is_system: false,
        });
        if self.current_room.as_deref() != Some(room) {
            state.unread += 1;
        }
        // Auto-scroll to bottom
        self.scroll_offset = 0;
    }

    fn add_history_message(&mut self, room: &str, msg: HistoryMessage) {
        let state = self.room_states.entry(room.to_string()).or_default();
        // History arrives in chronological order, so just append
        state.messages.push(ChatMessage {
            from: msg.from,
            text: msg.text,
            timestamp: Some(msg.timestamp),
            is_system: false,
        });
    }

    fn select_room(&mut self, room: &str) {
        self.current_room = Some(room.to_string());
        // Clear unread
        if let Some(state) = self.room_states.get_mut(room) {
            state.unread = 0;
        }
        // Update selection state
        if let Some(idx) = self.rooms.iter().position(|r| r.name == room) {
            self.room_list_state.select(Some(idx));
        }
        self.scroll_offset = 0;
    }

    fn current_room_state(&self) -> Option<&RoomState> {
        self.current_room
            .as_ref()
            .and_then(|r| self.room_states.get(r))
    }

    #[allow(dead_code)]
    fn current_room_state_mut(&mut self) -> Option<&mut RoomState> {
        let room = self.current_room.clone()?;
        self.room_states.get_mut(&room)
    }

    fn selected_room_name(&self) -> Option<&str> {
        self.room_list_state
            .selected()
            .and_then(|i| self.rooms.get(i))
            .map(|r| r.name.as_str())
    }
}

/// Events from the network or UI
enum AppEvent {
    /// Server event received
    Server(ServerEvent),
    /// Terminal input event
    Input(Event),
    /// Tick for UI updates
    Tick,
    /// Disconnected
    Disconnected,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let addr = format!("{}:{}", args.host, args.port);

    // Load configuration
    let config = Config::load(args.config.as_ref());
    let theme: Theme = config.theme.into();
    let timestamp_format = config.display.timestamp_format;

    // Connect to server
    let stream = TcpStream::connect(&addr).await?;
    let (mut reader, mut writer) = stream.into_split();

    // Set up terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app state
    let mut app = App::new(addr.clone(), theme, timestamp_format);
    app.connected = true;
    app.set_status(format!("Connected to {}", addr));

    // Channel for app events
    let (tx, mut rx) = mpsc::channel::<AppEvent>(100);

    // Spawn network reader task
    let tx_net = tx.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        let mut pending = Vec::new();

        loop {
            match reader.read(&mut buf).await {
                Ok(0) => {
                    let _ = tx_net.send(AppEvent::Disconnected).await;
                    break;
                }
                Ok(n) => {
                    pending.extend_from_slice(&buf[..n]);
                    while let Some((event, consumed)) = parse_frame::<ServerEvent>(&pending) {
                        pending.drain(..consumed);
                        if tx_net.send(AppEvent::Server(event)).await.is_err() {
                            return;
                        }
                    }
                }
                Err(_) => {
                    let _ = tx_net.send(AppEvent::Disconnected).await;
                    break;
                }
            }
        }
    });

    // Spawn input reader task
    let tx_input = tx.clone();
    tokio::spawn(async move {
        let mut reader = crossterm::event::EventStream::new();
        while let Some(Ok(event)) = reader.next().await {
            if tx_input.send(AppEvent::Input(event)).await.is_err() {
                break;
            }
        }
    });

    // Spawn tick task
    let tx_tick = tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            interval.tick().await;
            if tx_tick.send(AppEvent::Tick).await.is_err() {
                break;
            }
        }
    });

    // Auto-set nickname if provided
    if let Some(ref nick) = args.nick {
        let frame = frame_message(&ClientCommand::Nick(nick.clone()));
        writer.write_all(&frame).await?;
    }

    // Auto-join room if provided (only if nick is also set)
    if let Some(ref room) = args.room {
        if args.nick.is_some() {
            let frame = frame_message(&ClientCommand::Join(room.clone()));
            writer.write_all(&frame).await?;
        }
    }

    // Main event loop
    let result = run_app(&mut terminal, &mut app, &mut rx, &mut writer).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("Error: {}", e);
    }

    Ok(())
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    rx: &mut mpsc::Receiver<AppEvent>,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui(f, app))?;

        if let Some(event) = rx.recv().await {
            match event {
                AppEvent::Server(server_event) => {
                    handle_server_event(app, server_event, writer).await?;
                }
                AppEvent::Input(input_event) => {
                    if handle_input(app, input_event, writer).await? {
                        return Ok(());
                    }
                }
                AppEvent::Tick => {
                    // Clear old status messages
                    if let Some((_, time)) = &app.status {
                        if time.elapsed() > Duration::from_secs(5) {
                            app.status = None;
                        }
                    }
                }
                AppEvent::Disconnected => {
                    app.connected = false;
                    app.set_status("Disconnected from server");
                }
            }
        }
    }
}

async fn handle_server_event(
    app: &mut App,
    event: ServerEvent,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
) -> io::Result<()> {
    match event {
        ServerEvent::Welcome { message } => {
            app.set_status(message);
            // Request room list
            let frame = frame_message(&ClientCommand::ListRooms);
            writer.write_all(&frame).await?;
        }
        ServerEvent::NickOk { nick } => {
            app.nick = Some(nick.clone());
            app.set_status(format!("Nickname set to '{}'", nick));
        }
        ServerEvent::NickError { reason } => {
            app.set_status(format!("Nick error: {}", reason));
        }
        ServerEvent::Joined { room } => {
            app.room_states.entry(room.clone()).or_default();
            app.select_room(&room);
            // Request user list
            let frame = frame_message(&ClientCommand::ListUsers(room));
            writer.write_all(&frame).await?;
            // Refresh room list
            let frame = frame_message(&ClientCommand::ListRooms);
            writer.write_all(&frame).await?;
        }
        ServerEvent::JoinError { room, reason } => {
            app.set_status(format!("Failed to join #{}: {}", room, reason));
        }
        ServerEvent::Left { room } => {
            app.add_system_message(&room, "You left the room");
            if app.current_room.as_deref() == Some(&room) {
                app.current_room = None;
            }
            // Refresh room list
            let frame = frame_message(&ClientCommand::ListRooms);
            writer.write_all(&frame).await?;
        }
        ServerEvent::Message { room, from, text } => {
            app.add_chat_message(&room, &from, &text);
        }
        ServerEvent::UserJoined { room, nick } => {
            // Update user list (no system message - user list shows who's here)
            if let Some(state) = app.room_states.get_mut(&room) {
                if !state.users.contains(&nick) {
                    state.users.push(nick);
                    state.users.sort();
                }
            }
        }
        ServerEvent::UserLeft { room, nick } => {
            // Update user list (no system message - user list shows who's here)
            if let Some(state) = app.room_states.get_mut(&room) {
                state.users.retain(|u| u != &nick);
            }
        }
        ServerEvent::RoomList { rooms } => {
            app.rooms = rooms;
            // Preserve selection if possible
            if let Some(current) = &app.current_room {
                if let Some(idx) = app.rooms.iter().position(|r| &r.name == current) {
                    app.room_list_state.select(Some(idx));
                }
            }
        }
        ServerEvent::UserList { room, users } => {
            if let Some(state) = app.room_states.get_mut(&room) {
                state.users = users;
                state.users.sort();
            }
        }
        ServerEvent::History { room, messages } => {
            // Add history messages to the room
            for msg in messages {
                app.add_history_message(&room, msg);
            }
        }
        ServerEvent::Error { message } => {
            app.set_status(format!("Error: {}", message));
        }
        ServerEvent::Shutdown => {
            app.set_status("Server shutting down");
            app.connected = false;
        }
    }
    Ok(())
}

async fn handle_input(
    app: &mut App,
    event: Event,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
) -> io::Result<bool> {
    if let Event::Key(key) = event {
        // Global keys
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(true); // Quit
            }
            KeyCode::Esc => {
                if app.show_help {
                    app.show_help = false;
                } else {
                    return Ok(true); // Quit
                }
            }
            KeyCode::F(1) => {
                app.show_help = !app.show_help;
            }
            KeyCode::Tab => {
                app.focus = app.focus.next();
            }
            _ => {}
        }

        // Focus-specific keys
        match app.focus {
            Focus::Chat => {
                match key.code {
                    KeyCode::Char(c) => {
                        app.input.insert(app.cursor_pos, c);
                        app.cursor_pos += 1;
                    }
                    KeyCode::Backspace => {
                        if app.cursor_pos > 0 {
                            app.cursor_pos -= 1;
                            app.input.remove(app.cursor_pos);
                        }
                    }
                    KeyCode::Delete => {
                        if app.cursor_pos < app.input.len() {
                            app.input.remove(app.cursor_pos);
                        }
                    }
                    KeyCode::Left => {
                        app.cursor_pos = app.cursor_pos.saturating_sub(1);
                    }
                    KeyCode::Right => {
                        app.cursor_pos = (app.cursor_pos + 1).min(app.input.len());
                    }
                    KeyCode::Home => {
                        app.cursor_pos = 0;
                    }
                    KeyCode::End => {
                        app.cursor_pos = app.input.len();
                    }
                    KeyCode::Up => {
                        // Scroll up
                        if let Some(state) = app.current_room_state() {
                            app.scroll_offset =
                                (app.scroll_offset + 1).min(state.messages.len().saturating_sub(1));
                        }
                    }
                    KeyCode::Down => {
                        // Scroll down
                        app.scroll_offset = app.scroll_offset.saturating_sub(1);
                    }
                    KeyCode::PageUp => {
                        if let Some(state) = app.current_room_state() {
                            app.scroll_offset = (app.scroll_offset + 10)
                                .min(state.messages.len().saturating_sub(1));
                        }
                    }
                    KeyCode::PageDown => {
                        app.scroll_offset = app.scroll_offset.saturating_sub(10);
                    }
                    KeyCode::Enter => {
                        if !app.input.is_empty() {
                            let input = std::mem::take(&mut app.input);
                            app.cursor_pos = 0;
                            process_input(app, &input, writer).await?;
                        }
                    }
                    _ => {}
                }
            }
            Focus::Rooms => match key.code {
                KeyCode::Up => {
                    let len = app.rooms.len();
                    if len > 0 {
                        let i = app.room_list_state.selected().unwrap_or(0);
                        app.room_list_state
                            .select(Some(if i == 0 { len - 1 } else { i - 1 }));
                    }
                }
                KeyCode::Down => {
                    let len = app.rooms.len();
                    if len > 0 {
                        let i = app.room_list_state.selected().unwrap_or(0);
                        app.room_list_state.select(Some((i + 1) % len));
                    }
                }
                KeyCode::Enter => {
                    if let Some(room_name) = app.selected_room_name().map(|s| s.to_string()) {
                        // Join this room if not already in it
                        if !app.room_states.contains_key(&room_name) {
                            let frame = frame_message(&ClientCommand::Join(room_name));
                            writer.write_all(&frame).await?;
                        } else {
                            // Switch to this room
                            app.select_room(&room_name);
                        }
                    }
                }
                _ => {}
            },
            Focus::Users => match key.code {
                KeyCode::Up => {
                    if let Some(state) = app.current_room_state() {
                        let len = state.users.len();
                        if len > 0 {
                            let i = app.user_list_state.selected().unwrap_or(0);
                            app.user_list_state
                                .select(Some(if i == 0 { len - 1 } else { i - 1 }));
                        }
                    }
                }
                KeyCode::Down => {
                    if let Some(state) = app.current_room_state() {
                        let len = state.users.len();
                        if len > 0 {
                            let i = app.user_list_state.selected().unwrap_or(0);
                            app.user_list_state.select(Some((i + 1) % len));
                        }
                    }
                }
                _ => {}
            },
        }
    }
    Ok(false)
}

async fn process_input(
    app: &mut App,
    input: &str,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
) -> io::Result<()> {
    let input = input.trim();

    if let Some(stripped) = input.strip_prefix('/') {
        // Command
        let parts: Vec<&str> = stripped.splitn(2, ' ').collect();
        let cmd = parts.first().unwrap_or(&"");

        match *cmd {
            "nick" | "n" => {
                if let Some(nick) = parts.get(1) {
                    let frame = frame_message(&ClientCommand::Nick(nick.to_string()));
                    writer.write_all(&frame).await?;
                } else {
                    app.set_status("Usage: /nick <name>");
                }
            }
            "join" | "j" => {
                if let Some(room) = parts.get(1) {
                    let frame = frame_message(&ClientCommand::Join(room.to_string()));
                    writer.write_all(&frame).await?;
                } else {
                    app.set_status("Usage: /join <room>");
                }
            }
            "leave" | "l" => {
                if let Some(room) = app.current_room.clone() {
                    let frame = frame_message(&ClientCommand::Leave(room));
                    writer.write_all(&frame).await?;
                } else {
                    app.set_status("Not in a room");
                }
            }
            "rooms" | "r" => {
                let frame = frame_message(&ClientCommand::ListRooms);
                writer.write_all(&frame).await?;
            }
            "users" | "u" => {
                if let Some(room) = app.current_room.clone() {
                    let frame = frame_message(&ClientCommand::ListUsers(room));
                    writer.write_all(&frame).await?;
                } else {
                    app.set_status("Not in a room");
                }
            }
            "quit" | "q" => {
                let frame = frame_message(&ClientCommand::Quit);
                writer.write_all(&frame).await?;
            }
            "help" | "h" | "?" => {
                app.show_help = true;
            }
            _ => {
                app.set_status(format!("Unknown command: /{}", cmd));
            }
        }
    } else {
        // Regular message to current room
        if let Some(room) = app.current_room.clone() {
            if app.nick.is_none() {
                app.set_status("Set a nickname first with /nick <name>");
            } else {
                let frame = frame_message(&ClientCommand::Msg {
                    room,
                    text: input.to_string(),
                });
                writer.write_all(&frame).await?;
            }
        } else {
            app.set_status("Join a room first with /join <room>");
        }
    }

    Ok(())
}

fn ui(f: &mut Frame, app: &App) {
    let size = f.area();

    // Main layout: status bar at top, content in middle, input at bottom
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Status bar
            Constraint::Min(5),    // Content
            Constraint::Length(3), // Input
        ])
        .split(size);

    // Status bar
    let status_text = if let Some((msg, _)) = &app.status {
        msg.clone()
    } else if !app.connected {
        "Disconnected".to_string()
    } else if let Some(nick) = &app.nick {
        format!("{} @ {}", nick, app.server_addr)
    } else {
        format!("Not logged in @ {} - use /nick <name>", app.server_addr)
    };

    let status_style = if !app.connected {
        Style::default().fg(app.theme.status_disconnected)
    } else {
        Style::default().fg(app.theme.status_connected)
    };

    let status_bar = Paragraph::new(format!(" {} | F1: Help | Tab: Switch panels", status_text))
        .style(status_style);
    f.render_widget(status_bar, main_chunks[0]);

    // Content area: rooms | chat | users
    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(20), // Rooms
            Constraint::Percentage(60), // Chat
            Constraint::Percentage(20), // Users
        ])
        .split(main_chunks[1]);

    // Rooms panel
    render_rooms(f, app, content_chunks[0]);

    // Chat panel
    render_chat(f, app, content_chunks[1]);

    // Users panel
    render_users(f, app, content_chunks[2]);

    // Input panel
    render_input(f, app, main_chunks[2]);

    // Help popup
    if app.show_help {
        render_help_popup(f, size);
    }
}

fn render_rooms(f: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.focus == Focus::Rooms;
    let border_style = if is_focused {
        Style::default().fg(app.theme.border_focused)
    } else {
        Style::default().fg(app.theme.border_unfocused)
    };

    let items: Vec<ListItem> = app
        .rooms
        .iter()
        .map(|room| {
            let unread = app
                .room_states
                .get(&room.name)
                .map(|s| s.unread)
                .unwrap_or(0);

            let style = if app.current_room.as_deref() == Some(&room.name) {
                Style::default().fg(app.theme.room_active).bold()
            } else if unread > 0 {
                Style::default().fg(app.theme.room_unread)
            } else {
                Style::default().fg(app.theme.room_normal)
            };

            let text = if unread > 0 {
                format!("#{} ({}) [{}]", room.name, room.user_count, unread)
            } else {
                format!("#{} ({})", room.name, room.user_count)
            };

            ListItem::new(text).style(style)
        })
        .collect();

    let title_style = if is_focused {
        Style::default().fg(app.theme.title_focused).bold()
    } else {
        Style::default().fg(app.theme.title_unfocused)
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Rooms ")
                .title_style(title_style)
                .borders(Borders::ALL)
                .border_style(border_style),
        )
        .highlight_style(
            Style::default()
                .bg(app.theme.highlight_bg)
                .fg(app.theme.highlight_fg)
                .bold(),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(list, area, &mut app.room_list_state.clone());
}

fn render_chat(f: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.focus == Focus::Chat;
    let border_style = if is_focused {
        Style::default().fg(app.theme.border_focused)
    } else {
        Style::default().fg(app.theme.border_unfocused)
    };

    let title = if let Some(room) = &app.current_room {
        format!(" #{} ", room)
    } else {
        " Chat ".to_string()
    };

    let title_style = if is_focused {
        Style::default().fg(app.theme.title_focused).bold()
    } else {
        Style::default().fg(app.theme.title_unfocused)
    };

    let block = Block::default()
        .title(title)
        .title_style(title_style)
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    f.render_widget(block, area);

    if let Some(state) = app.current_room_state() {
        let available_height = inner.height as usize;
        let total_messages = state.messages.len();

        // Calculate which messages to show
        let end = total_messages.saturating_sub(app.scroll_offset);
        let start = end.saturating_sub(available_height);

        let lines: Vec<Line> = state.messages[start..end]
            .iter()
            .map(|msg| {
                if msg.is_system {
                    Line::from(vec![
                        Span::styled("*** ", Style::default().fg(app.theme.message_system)),
                        Span::styled(
                            &msg.text,
                            Style::default().fg(app.theme.message_system).italic(),
                        ),
                    ])
                } else {
                    let mut spans = Vec::new();

                    // Add timestamp if format is configured and timestamp exists
                    if !app.timestamp_format.is_empty() {
                        if let Some(ts) = msg.timestamp {
                            if let Some(dt) = Local.timestamp_opt(ts as i64, 0).single() {
                                let formatted = dt.format(&app.timestamp_format).to_string();
                                spans.push(Span::styled(
                                    format!("[{}] ", formatted),
                                    Style::default().fg(app.theme.message_timestamp),
                                ));
                            }
                        }
                    }

                    spans.push(Span::styled(
                        "<",
                        Style::default().fg(app.theme.border_unfocused),
                    ));
                    spans.push(Span::styled(
                        &msg.from,
                        Style::default().fg(app.theme.message_nick).bold(),
                    ));
                    spans.push(Span::styled(
                        "> ",
                        Style::default().fg(app.theme.border_unfocused),
                    ));
                    spans.push(Span::styled(
                        &msg.text,
                        Style::default().fg(app.theme.message_text),
                    ));

                    Line::from(spans)
                }
            })
            .collect();

        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        f.render_widget(paragraph, inner);

        // Scroll indicator
        if app.scroll_offset > 0 {
            let indicator = format!("↑ {} more", app.scroll_offset);
            let indicator_area = Rect {
                x: area.x + area.width - indicator.len() as u16 - 2,
                y: area.y,
                width: indicator.len() as u16 + 1,
                height: 1,
            };
            let indicator_widget =
                Paragraph::new(indicator).style(Style::default().fg(app.theme.title_focused));
            f.render_widget(indicator_widget, indicator_area);
        }
    } else {
        let help =
            Paragraph::new("Join a room to start chatting\n\n/join <room> or select from list")
                .style(Style::default().fg(app.theme.message_system))
                .wrap(Wrap { trim: false });
        f.render_widget(help, inner);
    }
}

fn render_users(f: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.focus == Focus::Users;
    let border_style = if is_focused {
        Style::default().fg(app.theme.border_focused)
    } else {
        Style::default().fg(app.theme.border_unfocused)
    };

    let users = app
        .current_room_state()
        .map(|s| &s.users[..])
        .unwrap_or(&[]);

    let items: Vec<ListItem> = users
        .iter()
        .map(|user| {
            // All users in the list are online, so show them in green
            // Bold the current user's name to distinguish self
            let style = if Some(user.as_str()) == app.nick.as_deref() {
                Style::default().fg(app.theme.user_self).bold()
            } else {
                Style::default().fg(app.theme.user_online)
            };
            // Show online indicator
            ListItem::new(format!("● {}", user)).style(style)
        })
        .collect();

    let title_style = if is_focused {
        Style::default().fg(app.theme.title_focused).bold()
    } else {
        Style::default().fg(app.theme.title_unfocused)
    };

    let title = format!(" Users ({}) ", users.len());
    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .title_style(title_style)
                .borders(Borders::ALL)
                .border_style(border_style),
        )
        .highlight_style(
            Style::default()
                .bg(app.theme.highlight_bg)
                .fg(app.theme.highlight_fg)
                .bold(),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(list, area, &mut app.user_list_state.clone());
}

fn render_input(f: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.focus == Focus::Chat;
    let border_style = if is_focused {
        Style::default().fg(app.theme.border_focused)
    } else {
        Style::default().fg(app.theme.border_unfocused)
    };

    let title_style = if is_focused {
        Style::default().fg(app.theme.title_focused).bold()
    } else {
        Style::default().fg(app.theme.title_unfocused)
    };

    let input_block = Block::default()
        .title(" Message ")
        .title_style(title_style)
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = input_block.inner(area);
    f.render_widget(input_block, area);

    let input_text = Paragraph::new(app.input.as_str());
    f.render_widget(input_text, inner);

    // Show cursor
    if is_focused {
        f.set_cursor_position((inner.x + app.cursor_pos as u16, inner.y));
    }
}

fn render_help_popup(f: &mut Frame, area: Rect) {
    let popup_width = 50;
    let popup_height = 16;
    let popup_area = Rect {
        x: (area.width.saturating_sub(popup_width)) / 2,
        y: (area.height.saturating_sub(popup_height)) / 2,
        width: popup_width.min(area.width),
        height: popup_height.min(area.height),
    };

    f.render_widget(Clear, popup_area);

    let help_text = vec![
        Line::from("Keybindings".bold()),
        Line::from(""),
        Line::from(vec![
            Span::styled("Tab       ", Style::default().fg(Color::Cyan)),
            Span::raw("Switch between panels"),
        ]),
        Line::from(vec![
            Span::styled("Enter     ", Style::default().fg(Color::Cyan)),
            Span::raw("Send message / Join room"),
        ]),
        Line::from(vec![
            Span::styled("↑/↓       ", Style::default().fg(Color::Cyan)),
            Span::raw("Navigate / Scroll"),
        ]),
        Line::from(vec![
            Span::styled("Esc/Ctrl+C", Style::default().fg(Color::Cyan)),
            Span::raw("Quit"),
        ]),
        Line::from(""),
        Line::from("Commands".bold()),
        Line::from(""),
        Line::from(vec![
            Span::styled("/nick ", Style::default().fg(Color::Green)),
            Span::raw("<name>  Set nickname"),
        ]),
        Line::from(vec![
            Span::styled("/join ", Style::default().fg(Color::Green)),
            Span::raw("<room>  Join a room"),
        ]),
        Line::from(vec![
            Span::styled("/leave", Style::default().fg(Color::Green)),
            Span::raw("        Leave current room"),
        ]),
        Line::from(vec![
            Span::styled("/rooms", Style::default().fg(Color::Green)),
            Span::raw("        List all rooms"),
        ]),
        Line::from(""),
        Line::from("Press any key to close".italic().fg(Color::DarkGray)),
    ];

    let help = Paragraph::new(Text::from(help_text))
        .block(
            Block::default()
                .title(" Help (F1) ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });

    f.render_widget(help, popup_area);
}

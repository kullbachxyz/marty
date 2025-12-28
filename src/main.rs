mod config;
mod matrix;
mod storage;

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Result;
use arboard::Clipboard;
use chrono::{Local, TimeZone};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;
use rpassword::read_password;
use tokio::sync::mpsc;

use crate::config::{
    config_path, crypto_dir, decrypt_sessions, encrypt_account_session, encrypt_missing_sessions,
    load_config, messages_dir, save_config,
};
use crate::matrix::{
    build_client, login_with_client, start_sync, MatrixCommand, MatrixEvent, RoomInfo, RoomListState,
};
use crate::storage::load_all_messages;

const TICK_RATE: Duration = Duration::from_millis(100);
const HELP_LINES: [&str; 21] = [
    "App navigation",
    "  F1 Toggle help panel showing shortcuts.",
    "  Up One Channel Up",
    "  Down One Channel Down",
    "  Alt+A Add chat (room or user).",
    "  Alt+J Join/add chat (room or user).",
    "  Alt+D Delete chat (type DELETE to confirm).",
    "  Ctrl+A Accept invite.",
    "  Ctrl+D Decline invite.",
    "  Alt+V Start verification (SAS).",
    "Message input",
    "  Enter when input box empty in single-line mode Open URL or attachment from selected message.",
    "  Enter otherwise Send message.",
    "Message/channel selection",
    "  Esc Reset message selection or close help panel.",
    "  Alt+Up Select previous message.",
    "  Alt+Down Select next message.",
    "Clipboard",
    "  Alt+Y Copy selected message to clipboard.",
    "Help menu",
    "  Esc Close help panel. Up/Down/PageDown Scroll.",
];

#[derive(Clone)]
enum MessageItem {
    Separator(String),
    Message {
        time: String,
        name: String,
        text: String,
    },
    Attachment {
        time: String,
        name: String,
        label: String,
        filename: String,
        path: String,
    },
}

enum PromptMode {
    Add,
    Delete { room_id: String, room_name: String },
}

struct PromptState {
    mode: PromptMode,
    input: String,
}

struct App {
    rooms: Vec<RoomInfo>,
    selected: usize,
    messages_by_room: HashMap<String, Vec<MessageItem>>,
    last_date_by_room: HashMap<String, String>,
    seen_event_ids: HashMap<String, HashSet<String>>,
    last_message_ts: HashMap<String, i64>,
    last_seen_ts: HashMap<String, i64>,
    unread_counts: HashMap<String, usize>,
    message_selected: Option<usize>,
    input: String,
    prompt: Option<PromptState>,
    verification_emojis: Option<Vec<(String, String)>>,
    verification_status: Option<String>,
    verification_until: Option<Instant>,
    help_open: bool,
    help_scroll: u16,
    is_syncing: bool,
    notifications_ready: bool,
    own_user_id: Option<String>,
    should_quit: bool,
}

impl App {
    fn new() -> Self {
        Self {
            rooms: Vec::new(),
            selected: 0,
            messages_by_room: HashMap::new(),
            last_date_by_room: HashMap::new(),
            seen_event_ids: HashMap::new(),
            last_message_ts: HashMap::new(),
            last_seen_ts: HashMap::new(),
            unread_counts: HashMap::new(),
            message_selected: None,
            input: String::new(),
            prompt: None,
            verification_emojis: None,
            verification_status: None,
            verification_until: None,
            help_open: false,
            help_scroll: 0,
            is_syncing: true,
            notifications_ready: false,
            own_user_id: None,
            should_quit: false,
        }
    }

    fn on_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.message_selected = None;
            if let Some(room_id) = self.rooms.get(self.selected).map(|room| room.room_id.clone()) {
                self.mark_room_read(&room_id);
            }
        }
    }

    fn on_down(&mut self) {
        if self.selected + 1 < self.rooms.len() {
            self.selected += 1;
            self.message_selected = None;
            if let Some(room_id) = self.rooms.get(self.selected).map(|room| room.room_id.clone()) {
                self.mark_room_read(&room_id);
            }
        }
    }

    fn on_enter(&mut self) -> Option<String> {
        if !self.input.trim().is_empty() {
            let text = self.input.trim_end().to_string();
            self.input.clear();
            return Some(text);
        }
        None
    }

    fn toggle_help(&mut self) {
        self.help_open = !self.help_open;
        if self.help_open {
            self.help_scroll = 0;
        }
    }

    fn start_add_prompt(&mut self) {
        self.prompt = Some(PromptState {
            mode: PromptMode::Add,
            input: String::new(),
        });
    }

    fn start_delete_prompt(&mut self) {
        if let Some(room) = self.rooms.get(self.selected) {
            self.prompt = Some(PromptState {
                mode: PromptMode::Delete {
                    room_id: room.room_id.clone(),
                    room_name: room.name.clone(),
                },
                input: String::new(),
            });
        }
    }

    fn cancel_prompt(&mut self) {
        self.prompt = None;
    }

    fn prompt_backspace(&mut self) {
        if let Some(state) = self.prompt.as_mut() {
            state.input.pop();
        }
    }

    fn prompt_push(&mut self, c: char) {
        if let Some(state) = self.prompt.as_mut() {
            state.input.push(c);
        }
    }

    fn submit_prompt(&mut self) -> Option<MatrixCommand> {
        let mut state = self.prompt.take()?;
        let trimmed = state.input.trim();
        if trimmed.is_empty() {
            self.prompt = Some(state);
            return None;
        }
        match &state.mode {
            PromptMode::Add => {
                if trimmed.starts_with('@') {
                    return Some(MatrixCommand::CreateDirect {
                        user_id: trimmed.to_string(),
                    });
                }
                Some(MatrixCommand::JoinRoom {
                    room: trimmed.to_string(),
                })
            }
            PromptMode::Delete { room_id, .. } => {
                if trimmed.eq_ignore_ascii_case("delete") {
                    let room_id = room_id.clone();
                    Some(MatrixCommand::LeaveRoom { room_id })
                } else {
                    state.input.clear();
                    self.prompt = Some(state);
                    None
                }
            }
        }
    }

    fn show_verification_emojis(&mut self, emojis: Vec<(String, String)>) {
        self.verification_emojis = Some(emojis);
        self.verification_status =
            Some("Match the emojis on your other device. Y=confirm, N=cancel".to_string());
        self.verification_until = None;
    }

    fn show_verification_status(&mut self, status: &str) {
        self.verification_emojis = None;
        self.verification_status = Some(status.to_string());
        self.verification_until = Some(Instant::now() + Duration::from_secs(3));
    }

    fn clear_verification(&mut self) {
        self.verification_emojis = None;
        self.verification_status = None;
        self.verification_until = None;
    }

    fn on_escape(&mut self) {
        if self.help_open {
            self.help_open = false;
        } else {
            self.message_selected = None;
        }
    }

    fn on_message_up(&mut self) {
        let Some(messages) = self.current_messages() else {
            return;
        };
        if messages.is_empty() {
            return;
        }
        self.message_selected = match self.message_selected {
            Some(idx) => Some(idx.saturating_sub(1)),
            None => Some(messages.len() - 1),
        };
    }

    fn on_message_down(&mut self) {
        let Some(messages) = self.current_messages() else {
            return;
        };
        if messages.is_empty() {
            return;
        }
        self.message_selected = match self.message_selected {
            Some(idx) => {
                if idx + 1 < messages.len() {
                    Some(idx + 1)
                } else {
                    Some(idx)
                }
            }
            None => Some(0),
        };
    }

    fn on_copy_message(&mut self) {
        if let Some(idx) = self.message_selected {
            if let Some(messages) = self.current_messages_mut() {
                if let Some(msg) = messages.get(idx) {
                    let text = msg_string(msg);
                    let _ = copy_to_clipboard(&text);
                }
            }
        }
    }

    fn on_open_url(&mut self) {
        if let Some(idx) = self.message_selected {
            if let Some(messages) = self.current_messages_mut() {
                if let Some(msg) = messages.get(idx) {
                    let msg_text = msg_string(msg);
                    if let Some(url) = extract_url(&msg_text) {
                        let _ = open_url(&url);
                    }
                }
            }
        }
    }

    fn on_help_up(&mut self) {
        self.help_scroll = self.help_scroll.saturating_sub(1);
    }

    fn on_help_down(&mut self) {
        let max = HELP_LINES.len().saturating_sub(1) as u16;
        self.help_scroll = (self.help_scroll + 1).min(max);
    }

    fn selected_room_id(&self) -> Option<String> {
        self.rooms.get(self.selected).map(|room| room.room_id.clone())
    }

    fn selected_room(&self) -> Option<&RoomInfo> {
        self.rooms.get(self.selected)
    }

    fn selected_room_is_invited(&self) -> bool {
        matches!(
            self.selected_room().map(|room| room.state),
            Some(RoomListState::Invited)
        )
    }

    fn selected_attachment_path(&self) -> Option<String> {
        let idx = self.message_selected?;
        let messages = self.current_messages()?;
        match messages.get(idx) {
            Some(MessageItem::Attachment { path, .. }) => Some(path.clone()),
            _ => None,
        }
    }

    fn current_messages(&self) -> Option<&Vec<MessageItem>> {
        let room_id = self.selected_room_id()?;
        self.messages_by_room.get(&room_id)
    }

    fn current_messages_mut(&mut self) -> Option<&mut Vec<MessageItem>> {
        let room_id = self.selected_room_id()?;
        self.messages_by_room.get_mut(&room_id)
    }

    fn update_rooms(&mut self, rooms: Vec<RoomInfo>) {
        for room in &rooms {
            self.messages_by_room
                .entry(room.room_id.clone())
                .or_default();
            self.seen_event_ids
                .entry(room.room_id.clone())
                .or_default();
            self.unread_counts.entry(room.room_id.clone()).or_default();
            self.last_seen_ts.entry(room.room_id.clone()).or_default();
            self.last_message_ts.entry(room.room_id.clone()).or_default();
        }
        self.rooms = rooms;
        self.selected = 0;
        self.message_selected = None;
        self.is_syncing = false;
        if let Some(room_id) = self.rooms.get(self.selected).map(|room| room.room_id.clone()) {
            self.mark_room_read(&room_id);
        }
    }

    fn handle_incoming_message(
        &mut self,
        room_id: &str,
        event_id: Option<&str>,
        ts: i64,
        sender: &str,
        body: &str,
    ) {
        let is_selected = self
            .selected_room_id()
            .as_deref()
            .map(|id| id == room_id)
            .unwrap_or(false);
        let last_seen = *self.last_seen_ts.get(room_id).unwrap_or(&0);
        if !is_selected && ts > last_seen {
            let entry = self.unread_counts.entry(room_id.to_string()).or_default();
            *entry = entry.saturating_add(1);
        }
        self.push_message_with_time(room_id, event_id, ts, sender, body);
        if is_selected {
            self.mark_room_read(room_id);
        }
    }

    fn handle_incoming_attachment(
        &mut self,
        room_id: &str,
        event_id: Option<&str>,
        ts: i64,
        sender: &str,
        label: &str,
        filename: &str,
        path: &str,
    ) {
        let is_selected = self
            .selected_room_id()
            .as_deref()
            .map(|id| id == room_id)
            .unwrap_or(false);
        let last_seen = *self.last_seen_ts.get(room_id).unwrap_or(&0);
        if !is_selected && ts > last_seen {
            let entry = self.unread_counts.entry(room_id.to_string()).or_default();
            *entry = entry.saturating_add(1);
        }
        self.push_attachment_with_time(
            room_id,
            event_id,
            ts,
            sender,
            label,
            filename,
            path,
        );
        if is_selected {
            self.mark_room_read(room_id);
        }
    }

    fn room_name(&self, room_id: &str) -> String {
        self.rooms
            .iter()
            .find(|room| room.room_id == room_id)
            .map(|room| room.name.clone())
            .unwrap_or_else(|| room_id.to_string())
    }

    fn should_notify(&self, room_id: &str, sender: &str) -> bool {
        if !self.notifications_ready {
            return false;
        }
        if self
            .selected_room_id()
            .as_deref()
            .map(|id| id == room_id)
            .unwrap_or(false)
        {
            return false;
        }
        if let Some(own) = self.own_user_id.as_deref() {
            if sender == own {
                return false;
            }
        }
        true
    }

    fn mark_room_read(&mut self, room_id: &str) {
        if let Some(ts) = self.last_message_ts.get(room_id).copied() {
            self.last_seen_ts.insert(room_id.to_string(), ts);
        }
        self.unread_counts.insert(room_id.to_string(), 0);
    }

    fn push_message_with_time(
        &mut self,
        room_id: &str,
        event_id: Option<&str>,
        ts: i64,
        sender: &str,
        body: &str,
    ) {
        if let Some(event_id) = event_id {
            let seen = self.seen_event_ids.entry(room_id.to_string()).or_default();
            if !seen.insert(event_id.to_string()) {
                return;
            }
        }
        let date = format_date(ts);
        let entry = self.messages_by_room.entry(room_id.to_string()).or_default();
        let last_date = self.last_date_by_room.entry(room_id.to_string()).or_default();
        if last_date != &date {
            entry.push(MessageItem::Separator(date.clone()));
            *last_date = date;
        }
        entry.push(MessageItem::Message {
            time: format_timestamp(ts),
            name: format_sender(sender),
            text: body.to_string(),
        });
        self.last_message_ts
            .insert(room_id.to_string(), ts);
    }

    fn push_attachment_with_time(
        &mut self,
        room_id: &str,
        event_id: Option<&str>,
        ts: i64,
        sender: &str,
        label: &str,
        filename: &str,
        path: &str,
    ) {
        if let Some(event_id) = event_id {
            let seen = self.seen_event_ids.entry(room_id.to_string()).or_default();
            if !seen.insert(event_id.to_string()) {
                return;
            }
        }
        let date = format_date(ts);
        let entry = self.messages_by_room.entry(room_id.to_string()).or_default();
        let last_date = self.last_date_by_room.entry(room_id.to_string()).or_default();
        if last_date != &date {
            entry.push(MessageItem::Separator(date.clone()));
            *last_date = date;
        }
        entry.push(MessageItem::Attachment {
            time: format_timestamp(ts),
            name: format_sender(sender),
            label: label.to_string(),
            filename: filename.to_string(),
            path: path.to_string(),
        });
        self.last_message_ts
            .insert(room_id.to_string(), ts);
    }
}

fn format_timestamp(ts: i64) -> String {
    Local
        .timestamp_millis_opt(ts)
        .single()
        .unwrap_or_else(Local::now)
        .format("%H:%M")
        .to_string()
}

fn format_date(ts: i64) -> String {
    Local
        .timestamp_millis_opt(ts)
        .single()
        .unwrap_or_else(Local::now)
        .format("%A, %m/%d/%y")
        .to_string()
}

fn format_sender(sender: &str) -> String {
    let trimmed = sender.trim_start_matches('@');
    trimmed.split(':').next().unwrap_or(trimmed).to_string()
}

fn parse_command(_text: &str) -> Option<MatrixCommand> {
    None
}

fn prompt(label: &str) -> io::Result<String> {
    print!("{}", label);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

fn prompt_password(label: &str) -> io::Result<String> {
    print!("{}", label);
    io::stdout().flush()?;
    let password = read_password().unwrap_or_default();
    Ok(password)
}

fn msg_string(item: &MessageItem) -> String {
    match item {
        MessageItem::Separator(label) => format!("==== {} ====", label),
        MessageItem::Message { time, name, text } => {
            format!("{} {}: {}", time, name, text)
        }
        MessageItem::Attachment {
            time,
            name,
            label,
            filename,
            path,
        } => {
            format!("{} {}: [{}] {} ({})", time, name, label, filename, path)
        }
    }
}

fn render_messages_area(
    f: &mut ratatui::Frame,
    area: Rect,
    app: &mut App,
) {
    let block = Block::default().borders(Borders::ALL).title("Messages");
    f.render_widget(&block, area);
    let inner = block.inner(area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if let Some(room) = app.selected_room() {
        if room.state == RoomListState::Invited {
            let inviter = room.inviter.as_deref().unwrap_or("Unknown user");
            let lines = vec![
                Line::from(format!("Invitation from {}", inviter)),
                Line::from("Ctrl+A to accept, Ctrl+D to decline."),
            ];
            let text = Paragraph::new(lines).wrap(Wrap { trim: false });
            f.render_widget(text, inner);
            return;
        }
    }
    let messages = app
        .current_messages()
        .map(|items| items.as_slice())
        .unwrap_or(&[]);
    let buf = f.buffer_mut();
    let mut y = inner.y;
    let max_y = inner.y + inner.height;
    for (idx, item) in messages.iter().enumerate() {
        if y >= max_y {
            break;
        }
        let selected = app.message_selected == Some(idx);
        match item {
            MessageItem::Separator(label) => {
                let line = format_separator(label, inner.width);
                draw_plain_line(buf, inner, y, &line, selected);
                y = y.saturating_add(1);
            }
            MessageItem::Message { time, name, text } => {
                let spans = message_spans(time, name, text);
                draw_spans_line(buf, inner, y, &spans, selected);
                y = y.saturating_add(1);
            }
            MessageItem::Attachment {
                time,
                name,
                label,
                filename,
                ..
            } => {
                let text = format!("[{}] {}", label, filename);
                let spans = message_spans(time, name, &text);
                draw_spans_line(buf, inner, y, &spans, selected);
                y = y.saturating_add(1);
            }
        }
    }
}

fn format_separator(label: &str, width: u16) -> String {
    let content_width = width as usize;
    let label_width = label.len();
    if content_width == 0 {
        return String::new();
    }
    if label_width + 2 >= content_width {
        return label.to_string();
    }
    let fill = content_width - label_width - 2;
    let left = fill / 2;
    let right = fill - left;
    format!("{} {} {}", "=".repeat(left), label, "=".repeat(right))
}

fn message_spans(time: &str, name: &str, text: &str) -> Vec<Span<'static>> {
    let time_span = Span::styled(
        format!("{} ", time),
        Style::default().fg(Color::Rgb(238, 193, 99)),
    );
    let name_color = if name == "You" {
        Color::Rgb(180, 140, 210)
    } else {
        Color::Rgb(109, 188, 226)
    };
    let name_span = Span::styled(
        format!("{}: ", name),
        Style::default()
            .fg(name_color)
            .add_modifier(Modifier::BOLD),
    );
    let text_span = Span::raw(text.to_string());
    vec![time_span, name_span, text_span]
}

fn draw_plain_line(buf: &mut Buffer, area: Rect, y: u16, text: &str, selected: bool) {
    if y >= area.y + area.height {
        return;
    }
    if selected {
        fill_line(buf, area, y);
        let style = Style::default().bg(Color::Indexed(15)).fg(Color::Black);
        let _ = buf.set_stringn(area.x, y, text, area.width as usize, style);
    } else {
        let _ = buf.set_stringn(area.x, y, text, area.width as usize, Style::default());
    }
}

fn draw_spans_line(buf: &mut Buffer, area: Rect, y: u16, spans: &[Span], selected: bool) {
    if y >= area.y + area.height {
        return;
    }
    let mut x = area.x;
    let max_width = area.width as usize;
    if selected {
        fill_line(buf, area, y);
    }
    for span in spans {
        if (x - area.x) as usize >= max_width {
            break;
        }
        let remaining = max_width.saturating_sub((x - area.x) as usize);
        let style = if selected {
            Style::default()
                .bg(Color::Indexed(15))
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD)
        } else {
            span.style
        };
        let (next_x, _) = buf.set_stringn(x, y, span.content.as_ref(), remaining, style);
        x = next_x;
    }
}

fn fill_line(buf: &mut Buffer, area: Rect, y: u16) {
    for x in 0..area.width {
        buf.get_mut(area.x + x, y)
            .set_symbol(" ")
            .set_bg(Color::Indexed(15))
            .set_fg(Color::Black);
    }
}

fn copy_to_clipboard(text: &str) -> bool {
    if env::var_os("WAYLAND_DISPLAY").is_some() {
        return copy_with_wl_copy(text);
    }
    if Clipboard::new()
        .and_then(|mut cb| cb.set_text(text.to_string()))
        .is_ok()
    {
        return true;
    }
    copy_with_wl_copy(text)
}

fn copy_with_wl_copy(text: &str) -> bool {
    if let Ok(mut child) = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        return child.wait().is_ok();
    }
    false
}

fn extract_url(text: &str) -> Option<String> {
    for part in text.split_whitespace() {
        if part.starts_with("http://") || part.starts_with("https://") {
            return Some(part.trim_end_matches(|c: char| c == ')' || c == ',' || c == '.').to_string());
        }
    }
    None
}

fn open_url(url: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        return Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .is_ok();
    }
    #[cfg(target_os = "macos")]
    {
        return Command::new("open").arg(url).spawn().is_ok();
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        return Command::new("xdg-open").arg(url).spawn().is_ok();
    }
}

fn open_path(path: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        return Command::new("cmd")
            .args(["/C", "start", "", &path.display().to_string()])
            .spawn()
            .is_ok();
    }
    #[cfg(target_os = "macos")]
    {
        return Command::new("open").arg(path).spawn().is_ok();
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        return Command::new("xdg-open").arg(path).spawn().is_ok();
    }
}

fn notify_send(title: &str, body: &str) {
    let _ = Command::new("notify-send")
        .arg(title)
        .arg(body)
        .spawn();
}

#[tokio::main]
async fn main() -> Result<()> {
    let config_file = config_path()?;
    let mut cfg = load_config(&config_file)?;
    let passphrase_prompt = if cfg.accounts.is_empty() {
        "Create passphrase: "
    } else {
        "Enter passphrase: "
    };
    let passphrase = prompt_password(passphrase_prompt)?;
    decrypt_sessions(&mut cfg, &passphrase)?;
    if encrypt_missing_sessions(&mut cfg, &passphrase)? {
        save_config(&config_file, &cfg)?;
    }

    let account = if cfg.accounts.is_empty() {
        let homeserver = prompt("Homeserver URL: ")?;
        let username = prompt("Username: ")?;
        let password = prompt_password("Password: ")?;
        let (client, account) =
            login_with_recovery(&homeserver, &username, &password, &passphrase).await?;
        let mut account = account.clone();
        encrypt_account_session(&mut account, &passphrase)?;
        let own_user_id = account.user_id.clone();
        cfg.accounts.push(account);
        cfg.active = Some(0);
        save_config(&config_file, &cfg)?;
        return start_matrix(client, passphrase, own_user_id).await;
    } else {
        let idx = cfg.active.unwrap_or(0).min(cfg.accounts.len().saturating_sub(1));
        cfg.accounts[idx].clone()
    };

    let client = if let Some(session) = account.session.clone() {
        let client = build_client_with_recovery(&account.homeserver, &passphrase).await?;
        if client.restore_session(session).await.is_ok() {
            client
        } else {
            let password = prompt_password("Password: ")?;
            let (client, updated) =
                login_with_recovery(&account.homeserver, &account.username, &password, &passphrase)
                    .await?;
            update_account_session(&mut cfg, &updated, &passphrase)?;
            save_config(&config_file, &cfg)?;
            client
        }
    } else {
        let password = prompt_password("Password: ")?;
        let (client, updated) =
            login_with_recovery(&account.homeserver, &account.username, &password, &passphrase)
                .await?;
        update_account_session(&mut cfg, &updated, &passphrase)?;
        save_config(&config_file, &cfg)?;
        client
    };

    start_matrix(client, passphrase, account.user_id.clone()).await
}

async fn start_matrix(
    client: matrix_sdk::Client,
    passphrase: String,
    own_user_id: Option<String>,
) -> Result<()> {
    let (evt_tx, evt_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

    tokio::spawn(start_sync(client, passphrase.clone(), cmd_rx, evt_tx));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_app(&mut terminal, evt_rx, cmd_tx, passphrase, own_user_id);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res?;
    Ok(())
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    mut evt_rx: mpsc::UnboundedReceiver<MatrixEvent>,
    cmd_tx: mpsc::UnboundedSender<MatrixCommand>,
    passphrase: String,
    own_user_id: Option<String>,
) -> io::Result<()> {
    let mut app = App::new();
    app.own_user_id = own_user_id;
    let mut last_tick = Instant::now();
    if let Ok(base) = messages_dir() {
        if let Ok(persisted) = load_all_messages(&base, &passphrase) {
            for (room_key, mut records) in persisted {
                records.sort_by_key(|m| m.timestamp);
                for record in records {
                    let room_id = room_key.replace('_', ":");
                    if let Some(path) = record.attachment_path.as_deref() {
                        let label = record
                            .attachment_kind
                            .as_deref()
                            .unwrap_or("file");
                        let name = record
                            .attachment_name
                            .as_deref()
                            .unwrap_or(&record.body);
                        app.push_attachment_with_time(
                            &room_id,
                            record.event_id.as_deref(),
                            record.timestamp,
                            &record.sender,
                            label,
                            name,
                            path,
                        );
                    } else {
                        app.push_message_with_time(
                            &room_id,
                            record.event_id.as_deref(),
                            record.timestamp,
                            &record.sender,
                            &record.body,
                        );
                    }
                }
            }
            for (room_id, ts) in app.last_message_ts.clone() {
                app.last_seen_ts.entry(room_id).or_insert(ts);
            }
        }
    }

    loop {
        while let Ok(evt) = evt_rx.try_recv() {
            match evt {
                MatrixEvent::Rooms(rooms) => app.update_rooms(rooms),
                MatrixEvent::Message {
                    room_id,
                    event_id,
                    sender,
                    body,
                    timestamp,
                } => {
                    app.handle_incoming_message(
                        &room_id,
                        Some(&event_id),
                        timestamp,
                        &sender,
                        &body,
                    );
                    if app.should_notify(&room_id, &sender) {
                        let title = format!("{} — {}", app.room_name(&room_id), format_sender(&sender));
                        notify_send(&title, &body);
                    }
                }
                MatrixEvent::Attachment {
                    room_id,
                    event_id,
                    sender,
                    name,
                    path,
                    kind,
                    timestamp,
                } => {
                    app.handle_incoming_attachment(
                        &room_id,
                        Some(&event_id),
                        timestamp,
                        &sender,
                        &kind,
                        &name,
                        &path,
                    );
                    if app.should_notify(&room_id, &sender) {
                        let title = format!("{} — {}", app.room_name(&room_id), format_sender(&sender));
                        let body = format!("[{}] {}", kind, name);
                        notify_send(&title, &body);
                    }
                }
                MatrixEvent::BackfillDone => {
                    app.notifications_ready = true;
                }
                MatrixEvent::VerificationEmojis { emojis } => {
                    app.show_verification_emojis(emojis);
                }
                MatrixEvent::VerificationStatus { message } => {
                    app.show_verification_status(&message);
                }
                MatrixEvent::VerificationDone => {
                    app.show_verification_status("Verification complete.");
                }
                MatrixEvent::VerificationCancelled { reason } => {
                    app.show_verification_status(&format!("Verification cancelled: {}", reason));
                }
            }
        }
        if app.verification_emojis.is_none() {
            if let Some(until) = app.verification_until {
                if Instant::now() >= until {
                    app.clear_verification();
                }
            }
        }

        terminal.draw(|f| {
            let size = f.size();

            if app.help_open {
                let help_lines: Vec<Line> = HELP_LINES
                    .iter()
                    .map(|line| Line::from(Span::raw(*line)))
                    .collect();
                let help = Paragraph::new(help_lines)
                    .block(Block::default().borders(Borders::ALL).title("Help"))
                    .wrap(Wrap { trim: false })
                    .scroll((app.help_scroll, 0));
                f.render_widget(help, size);
            } else {
                let main_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Length(28), Constraint::Min(1)])
                    .split(size);

                let right_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(3), Constraint::Length(3)])
                    .split(main_chunks[1]);

                let channels: Vec<ListItem> = app
                    .rooms
                    .iter()
                    .map(|room| {
                        let label = if room.state == RoomListState::Invited {
                            format!("[invite] {}", room.name)
                        } else {
                            room.name.clone()
                        };
                        let unread = *app.unread_counts.get(&room.room_id).unwrap_or(&0);
                        let display = if unread > 0 {
                            format!("{} [{}]", label, unread)
                        } else {
                            label
                        };
                        let style = if unread > 0 {
                            Style::default().add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        };
                        ListItem::new(Line::from(Span::styled(display, style)))
                    })
                    .collect();

                let mut list_state = ListState::default();
                if !app.rooms.is_empty() {
                    list_state.select(Some(app.selected));
                }

                let channels_list = List::new(channels)
                    .block(Block::default().borders(Borders::ALL).title("Channels"))
                    .highlight_style(
                        Style::default()
                            .bg(Color::Rgb(160, 170, 210))
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    );

                f.render_stateful_widget(channels_list, main_chunks[0], &mut list_state);

                render_messages_area(f, right_chunks[0], &mut app);
                let input = Paragraph::new(app.input.as_str())
                    .block(Block::default().borders(Borders::ALL).title("Input"));

                f.render_widget(input, right_chunks[1]);
                let input_area = right_chunks[1];
                let x = input_area.x + 1;
                let y = input_area.y + 1;
                let max_width = input_area.width.saturating_sub(2) as usize;
                let cursor_x = x + (app.input.len().min(max_width) as u16);
                f.set_cursor(cursor_x, y);
            }

            if let Some(ref prompt) = app.prompt {
                render_prompt(f, size, prompt);
            }
            if app.verification_emojis.is_some() || app.verification_status.is_some() {
                render_verification_overlay(f, size, &app);
            }
            if app.is_syncing && !app.help_open {
                render_sync_indicator(f, size);
            }
        })?;

        let timeout = TICK_RATE
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    if app.prompt.is_some() {
                        match key.code {
                            KeyCode::Esc => app.cancel_prompt(),
                            KeyCode::Enter => {
                                if let Some(cmd) = app.submit_prompt() {
                                    let _ = cmd_tx.send(cmd);
                                }
                            }
                            KeyCode::Backspace => app.prompt_backspace(),
                            KeyCode::Char(c) => app.prompt_push(c),
                            _ => {}
                        }
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('q') => app.should_quit = true,
                        KeyCode::F(1) => app.toggle_help(),
                        KeyCode::Esc => {
                            if app.verification_status.is_some()
                                && app.verification_emojis.is_none()
                            {
                                app.clear_verification();
                            } else {
                                app.on_escape();
                            }
                        }
                        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::ALT) => {
                            app.start_add_prompt();
                        }
                        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::ALT) => {
                            app.start_add_prompt();
                        }
                        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::ALT) => {
                            app.start_delete_prompt();
                        }
                        KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::ALT) => {
                            let _ = cmd_tx.send(MatrixCommand::StartVerification);
                            app.show_verification_status("Waiting for verification...");
                        }
                        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if app.selected_room_is_invited() {
                                if let Some(room_id) = app.selected_room_id() {
                                    let _ = cmd_tx.send(MatrixCommand::AcceptInvite { room_id });
                                }
                            }
                        }
                        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if app.selected_room_is_invited() {
                                if let Some(room_id) = app.selected_room_id() {
                                    let _ = cmd_tx.send(MatrixCommand::RejectInvite { room_id });
                                }
                            }
                        }
                        KeyCode::Char('y') if app.verification_emojis.is_some() => {
                            let _ = cmd_tx.send(MatrixCommand::ConfirmVerification);
                            app.show_verification_status("Verification confirmed.");
                        }
                        KeyCode::Char('n') if app.verification_emojis.is_some() => {
                            let _ = cmd_tx.send(MatrixCommand::CancelVerification);
                            app.show_verification_status("Verification cancelled.");
                        }
                        KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
                            app.on_message_up()
                        }
                        KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                            app.on_message_down()
                        }
                        KeyCode::Up => {
                            if app.help_open {
                                app.on_help_up();
                            } else {
                                app.on_up();
                            }
                        }
                        KeyCode::Down => {
                            if app.help_open {
                                app.on_help_down();
                            } else {
                                app.on_down();
                            }
                        }
                        KeyCode::PageDown => {
                            if app.help_open {
                                app.on_help_down();
                            }
                        }
                        KeyCode::PageUp => {
                            if app.help_open {
                                app.on_help_up();
                            }
                        }
                        KeyCode::Enter => {
                            if app.input.trim().is_empty() {
                                if let Some(path) = app.selected_attachment_path() {
                                    let _ = open_path(Path::new(&path));
                                } else {
                                    app.on_open_url();
                                }
                            } else if let Some(text) = app.on_enter() {
                                if let Some(cmd) = parse_command(&text) {
                                    let _ = cmd_tx.send(cmd);
                                } else if let Some(room_id) = app.selected_room_id() {
                                    if app.selected_room_is_invited() {
                                        continue;
                                    }
                                    let _ = cmd_tx.send(MatrixCommand::SendMessage { room_id, body: text });
                                }
                            }
                        }
                        KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::ALT) => {
                            app.on_copy_message();
                        }
                        KeyCode::Backspace => {
                            app.input.pop();
                        }
                        KeyCode::Char(c) => {
                            app.input.push(c);
                        }
                        _ => {}
                    }
                }
            }
        }

        if last_tick.elapsed() >= TICK_RATE {
            last_tick = Instant::now();
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

fn update_account_session(
    cfg: &mut config::AppConfig,
    updated: &config::AccountConfig,
    passphrase: &str,
) -> io::Result<()> {
    if let Some(idx) = cfg.active {
        if let Some(existing) = cfg.accounts.get_mut(idx) {
            existing.session = updated.session.clone();
            existing.user_id = updated.user_id.clone();
            encrypt_account_session(existing, passphrase)?;
            return Ok(());
        }
    }
    let mut account = updated.clone();
    encrypt_account_session(&mut account, passphrase)?;
    cfg.accounts.push(account);
    cfg.active = Some(0);
    Ok(())
}

async fn build_client_with_recovery(
    homeserver: &str,
    passphrase: &str,
) -> Result<matrix_sdk::Client> {
    match build_client(homeserver, passphrase).await {
        Ok(client) => Ok(client),
        Err(err) => {
            let err_str = format!("{:#}", err);
            if err_str.contains("EncryptedValue") || err_str.contains("decrypt") {
                eprintln!("Crypto store appears unencrypted or passphrase mismatch.");
                let answer = prompt("Type 'reset' to delete the crypto store and continue: ")?;
                if answer.trim().eq_ignore_ascii_case("reset") {
                    let dir = crypto_dir()?;
                    if dir.exists() {
                        fs::remove_dir_all(&dir)?;
                    }
                    return build_client(homeserver, passphrase).await;
                }
            }
            Err(err)
        }
    }
}

async fn login_with_recovery(
    homeserver: &str,
    username: &str,
    password: &str,
    passphrase: &str,
) -> Result<(matrix_sdk::Client, config::AccountConfig)> {
    let mut client = build_client_with_recovery(homeserver, passphrase).await?;
    match login_with_client(&client, homeserver, username, password).await {
        Ok(account) => Ok((client, account)),
        Err(err) => {
            let err_str = format!("{:#}", err);
            if err_str.contains("EncryptedValue") || err_str.contains("decrypt") {
                eprintln!("Crypto store appears unencrypted or passphrase mismatch.");
                let answer = prompt("Type 'reset' to delete the crypto store and continue: ")?;
                if answer.trim().eq_ignore_ascii_case("reset") {
                    let dir = crypto_dir()?;
                    if dir.exists() {
                        fs::remove_dir_all(&dir)?;
                    }
                    client = build_client(homeserver, passphrase).await?;
                    let account = login_with_client(&client, homeserver, username, password).await?;
                    return Ok((client, account));
                }
            }
            Err(err)
        }
    }
}

fn render_prompt(f: &mut ratatui::Frame, area: Rect, prompt: &PromptState) {
    let popup = centered_rect(60, 3, area);
    let title = match &prompt.mode {
        PromptMode::Add => "Add chat (@user or #room)".to_string(),
        PromptMode::Delete { room_name, .. } => {
            format!("Delete chat \"{}\"? Type DELETE", room_name)
        }
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    f.render_widget(&block, popup);
    let inner = block.inner(popup);
    let text = Paragraph::new(prompt.input.as_str());
    f.render_widget(text, inner);
    let x = inner.x + (prompt.input.len().min(inner.width as usize) as u16);
    f.set_cursor(x, inner.y);
}

fn render_verification_overlay(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(70, 7, area);
    let block = Block::default().borders(Borders::ALL).title("Verification");
    f.render_widget(&block, popup);
    let inner = block.inner(popup);
    let mut lines = Vec::new();
    if let Some(ref emojis) = app.verification_emojis {
        let symbols = emojis
            .iter()
            .map(|(symbol, _)| format!("{:^6}", symbol))
            .collect::<Vec<_>>()
            .join("");
        let labels = emojis
            .iter()
            .map(|(_, desc)| format!("{:^6}", desc))
            .collect::<Vec<_>>()
            .join("");
        lines.push(Line::from(symbols));
        lines.push(Line::from(labels));
    }
    if let Some(ref status) = app.verification_status {
        lines.push(Line::from(status.as_str()));
    }
    let content = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(content, inner);
}

fn render_sync_indicator(f: &mut ratatui::Frame, area: Rect) {
    let width = 18;
    let height = 3;
    let x = area.x + area.width.saturating_sub(width) - 1;
    let y = area.y + 1;
    let rect = Rect { x, y, width, height };
    let block = Block::default().borders(Borders::ALL).title("Sync");
    f.render_widget(&block, rect);
    let inner = block.inner(rect);
    let text = Paragraph::new("Syncing...");
    f.render_widget(text, inner);
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let width = area.width.saturating_mul(percent_x) / 100;
    let x = area.x + (area.width.saturating_sub(width) / 2);
    let y = area.y + (area.height.saturating_sub(height) / 2);
    Rect { x, y, width, height }
}

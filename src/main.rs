use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;

const TICK_RATE: Duration = Duration::from_millis(100);
const HELP_LINES: [&str; 15] = [
    "App navigation",
    "  F1 Toggle help panel showing shortcuts.",
    "  Up One Channel Up",
    "  Down One Channel Down",
    "Message input",
    "  Enter when input box empty in single-line mode Open URL from selected message.",
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
}

struct App {
    channels: Vec<String>,
    selected: usize,
    messages_by_channel: Vec<Vec<MessageItem>>,
    message_selected: Option<usize>,
    input: String,
    help_open: bool,
    help_scroll: u16,
    reply_idx: usize,
    should_quit: bool,
}

impl App {
    fn new() -> Self {
        Self {
            channels: vec![
                "Melanie".to_string(),
                "Nullinger0".to_string(),
                "1994".to_string(),
                "Lido Melphi Tours".to_string(),
                "Philipp".to_string(),
                "Arlene".to_string(),
                "Dominik".to_string(),
                "Lisa".to_string(),
            ],
            selected: 0,
            messages_by_channel: vec![
                vec![
                    MessageItem::Separator("Monday, 12/15/25".to_string()),
                    MessageItem::Message {
                        time: "14:26".to_string(),
                        name: "Philipp".to_string(),
                        text: "Hallo".to_string(),
                    },
                    MessageItem::Message {
                        time: "14:26".to_string(),
                        name: "Philipp".to_string(),
                        text: "wann kommst du nochmal heim?".to_string(),
                    },
                    MessageItem::Message {
                        time: "14:26".to_string(),
                        name: "Philipp".to_string(),
                        text: "ich habs vergessen sorry".to_string(),
                    },
                    MessageItem::Message {
                        time: "14:28".to_string(),
                        name: "Melanie".to_string(),
                        text: "Spat. Ich schatze 18 Uhr etwa".to_string(),
                    },
                    MessageItem::Message {
                        time: "14:30".to_string(),
                        name: "Philipp".to_string(),
                        text: "okay".to_string(),
                    },
                    MessageItem::Separator("Tuesday, 12/16/25".to_string()),
                    MessageItem::Message {
                        time: "09:49".to_string(),
                        name: "Philipp".to_string(),
                        text: "Danke!".to_string(),
                    },
                    MessageItem::Separator("Wednesday, 12/17/25".to_string()),
                    MessageItem::Message {
                        time: "18:50".to_string(),
                        name: "Philipp".to_string(),
                        text: "Hallo! :-)".to_string(),
                    },
                ],
                vec![
                    MessageItem::Separator("Friday, 12/12/25".to_string()),
                    MessageItem::Message {
                        time: "10:02".to_string(),
                        name: "Nullinger0".to_string(),
                        text: "build logs look clean.".to_string(),
                    },
                    MessageItem::Message {
                        time: "10:14".to_string(),
                        name: "Nullinger0".to_string(),
                        text: "F1 opens the shortcuts panel.".to_string(),
                    },
                ],
                vec![
                    MessageItem::Separator("Thursday, 12/11/25".to_string()),
                    MessageItem::Message {
                        time: "21:05".to_string(),
                        name: "1994".to_string(),
                        text: "archival notes live here.".to_string(),
                    },
                    MessageItem::Message {
                        time: "21:06".to_string(),
                        name: "1994".to_string(),
                        text: "https://matrix.org has docs and specs.".to_string(),
                    },
                ],
                vec![
                    MessageItem::Separator("Saturday, 12/20/25".to_string()),
                    MessageItem::Message {
                        time: "08:30".to_string(),
                        name: "Lido Melphi Tours".to_string(),
                        text: "itinerary finalized.".to_string(),
                    },
                    MessageItem::Message {
                        time: "09:10".to_string(),
                        name: "Lido Melphi Tours".to_string(),
                        text: "confirm ticket status.".to_string(),
                    },
                ],
                vec![
                    MessageItem::Separator("Wednesday, 12/17/25".to_string()),
                    MessageItem::Message {
                        time: "16:02".to_string(),
                        name: "Philipp".to_string(),
                        text: "This mirrors the gurk-rs layout.".to_string(),
                    },
                    MessageItem::Message {
                        time: "16:03".to_string(),
                        name: "Philipp".to_string(),
                        text: "keys are in keybinds.md.".to_string(),
                    },
                ],
                vec![
                    MessageItem::Separator("Tuesday, 12/16/25".to_string()),
                    MessageItem::Message {
                        time: "12:01".to_string(),
                        name: "Arlene".to_string(),
                        text: "Press Enter on empty input to open a URL (stub).".to_string(),
                    },
                    MessageItem::Message {
                        time: "12:02".to_string(),
                        name: "Arlene".to_string(),
                        text: "Alt+Y copies the selected line (stub).".to_string(),
                    },
                ],
                vec![
                    MessageItem::Separator("Monday, 12/15/25".to_string()),
                    MessageItem::Message {
                        time: "18:12".to_string(),
                        name: "Dominik".to_string(),
                        text: "Selection highlights in the messages list.".to_string(),
                    },
                    MessageItem::Message {
                        time: "18:13".to_string(),
                        name: "Dominik".to_string(),
                        text: "Esc resets selection.".to_string(),
                    },
                ],
                vec![
                    MessageItem::Separator("Friday, 12/12/25".to_string()),
                    MessageItem::Message {
                        time: "17:44".to_string(),
                        name: "Lisa".to_string(),
                        text: "UI polish looks great.".to_string(),
                    },
                    MessageItem::Message {
                        time: "17:45".to_string(),
                        name: "Lisa".to_string(),
                        text: "Try sending a test line.".to_string(),
                    },
                ],
            ],
            message_selected: None,
            input: String::new(),
            help_open: false,
            help_scroll: 0,
            reply_idx: 0,
            should_quit: false,
        }
    }

    fn on_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.message_selected = None;
        }
    }

    fn on_down(&mut self) {
        if self.selected + 1 < self.channels.len() {
            self.selected += 1;
            self.message_selected = None;
        }
    }

    fn on_enter(&mut self) {
        if !self.input.trim().is_empty() {
            let msg = MessageItem::Message {
                time: "19:02".to_string(),
                name: "You".to_string(),
                text: self.input.trim_end().to_string(),
            };
            if let Some(messages) = self.messages_by_channel.get_mut(self.selected) {
                messages.push(msg);
                let replies = [
                    ("19:03", "Bot", "Got it."),
                    ("19:03", "Bot", "Sounds good."),
                    ("19:04", "Bot", "Acknowledged."),
                    ("19:04", "Bot", "Let me check on that."),
                    ("19:05", "Bot", "Thanks for the update."),
                ];
                let reply = replies[self.reply_idx % replies.len()];
                self.reply_idx = self.reply_idx.wrapping_add(1);
                messages.push(MessageItem::Message {
                    time: reply.0.to_string(),
                    name: reply.1.to_string(),
                    text: reply.2.to_string(),
                });
            }
            self.input.clear();
        }
    }

    fn toggle_help(&mut self) {
        self.help_open = !self.help_open;
        if self.help_open {
            self.help_scroll = 0;
        }
    }

    fn on_escape(&mut self) {
        if self.help_open {
            self.help_open = false;
        } else {
            self.message_selected = None;
        }
    }

    fn on_message_up(&mut self) {
        let Some(messages) = self.messages_by_channel.get(self.selected) else {
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
        let Some(messages) = self.messages_by_channel.get(self.selected) else {
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
            if let Some(messages) = self.messages_by_channel.get_mut(self.selected) {
                if let Some(msg) = messages.get(idx) {
                    messages.push(MessageItem::Message {
                        time: "19:06".to_string(),
                        name: "System".to_string(),
                        text: format!("Copied: {}", msg_string(msg)),
                    });
                }
            }
        }
    }

    fn on_open_url(&mut self) {
        if let Some(idx) = self.message_selected {
            if let Some(messages) = self.messages_by_channel.get_mut(self.selected) {
                if let Some(msg) = messages.get(idx) {
                    let msg_text = msg_string(msg);
                    if let Some(start) = msg_text.find("http") {
                        let url = &msg_text[start..];
                        messages.push(MessageItem::Message {
                            time: "19:06".to_string(),
                            name: "System".to_string(),
                            text: format!("Open URL (stub): {}", url),
                        });
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
}

fn message_item_to_list_item(item: &MessageItem, area_width: u16) -> ListItem<'static> {
    match item {
        MessageItem::Separator(label) => {
            let content_width = area_width.saturating_sub(2) as usize;
            let label_width = label.len();
            if content_width == 0 {
                return ListItem::new(Line::from(Span::raw("")));
            }
            if label_width + 2 >= content_width {
                return ListItem::new(Line::from(Span::raw(label.clone())));
            }
            let fill = content_width - label_width - 2;
            let left = fill / 2;
            let right = fill - left;
            let line = format!("{} {} {}", "=".repeat(left), label, "=".repeat(right));
            ListItem::new(Line::from(Span::raw(line)))
        }
        MessageItem::Message { time, name, text } => {
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
            let text_span = Span::raw(text.clone());
            ListItem::new(Line::from(vec![time_span, name_span, text_span]))
        }
    }
}

fn msg_string(item: &MessageItem) -> String {
    match item {
        MessageItem::Separator(label) => format!("==== {} ====", label),
        MessageItem::Message { time, name, text } => {
            format!("{} {}: {}", time, name, text)
        }
    }
}

fn main() -> Result<(), io::Error> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_app(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let mut app = App::new();
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| {
            let size = f.size();

            let main_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(28), Constraint::Min(1)])
                .split(size);

            let right_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(3)])
                .split(main_chunks[1]);

            let channels: Vec<ListItem> = app
                .channels
                .iter()
                .map(|c| ListItem::new(Line::from(Span::raw(c))))
                .collect();

            let mut list_state = ListState::default();
            list_state.select(Some(app.selected));

            let channels_list = List::new(channels)
                .block(Block::default().borders(Borders::ALL).title("Channels"))
                .highlight_style(
                    Style::default()
                        .bg(Color::Rgb(160, 170, 210))
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                );

            f.render_stateful_widget(channels_list, main_chunks[0], &mut list_state);

            if app.help_open {
                let help_lines: Vec<Line> = HELP_LINES
                    .iter()
                    .map(|line| Line::from(Span::raw(*line)))
                    .collect();
                let help = Paragraph::new(help_lines)
                    .block(Block::default().borders(Borders::ALL).title("Help"))
                    .wrap(Wrap { trim: false })
                    .scroll((app.help_scroll, 0));
                f.render_widget(help, right_chunks[0]);
            } else {
                let messages = app
                    .messages_by_channel
                    .get(app.selected)
                    .map(|items| items.as_slice())
                    .unwrap_or(&[]);
                let messages_items: Vec<ListItem> = messages
                    .iter()
                    .map(|m| message_item_to_list_item(m, right_chunks[0].width))
                    .collect();

                let mut messages_state = ListState::default();
                messages_state.select(app.message_selected);

                let messages = List::new(messages_items)
                    .block(Block::default().borders(Borders::ALL).title("Messages"))
                    .highlight_style(
                        Style::default()
                            .bg(Color::Indexed(15))
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    );

                f.render_stateful_widget(messages, right_chunks[0], &mut messages_state);
            }

            let input = Paragraph::new(app.input.as_str())
                .block(Block::default().borders(Borders::ALL).title("Input"));

            f.render_widget(input, right_chunks[1]);
            if !app.help_open {
                let input_area = right_chunks[1];
                let x = input_area.x + 1;
                let y = input_area.y + 1;
                let max_width = input_area.width.saturating_sub(2) as usize;
                let cursor_x = x + (app.input.len().min(max_width) as u16);
                f.set_cursor(cursor_x, y);
            }
        })?;

        let timeout = TICK_RATE
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') => app.should_quit = true,
                        KeyCode::F(1) => app.toggle_help(),
                        KeyCode::Esc => app.on_escape(),
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
                                app.on_open_url();
                            } else {
                                app.on_enter();
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

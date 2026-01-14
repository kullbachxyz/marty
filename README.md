# marty

A Matrix TUI client inspired by gurk-rs.

## Features
- Slim channel list, message view, and input box layout
- Matrix login with persistent, encrypted sessions
- E2EE with SAS emoji verification
- Encrypted local message archive (passphrase protected)
- Join rooms or start DMs from the TUI
- Invite support with accept/decline from the messages pane
- Backfill messages since last run
- Unread counts per channel
- Read receipts for sent messages (○ delivered / ● read)
- Desktop notifications via `notify-send`
- Attachment downloads with `xdg-open`
- Send attachments by typing `file://<path>`
- Input editing with multi-line mode, cursor movement, and word jumps
- Clipboard copy grabs message content only (no timestamp/username)

## Installation
- Install Rust (stable) and Cargo
- Build and run:
  - `cargo run`

## First Run
- Enter a passphrase to encrypt the local store.
- Provide homeserver URL, username, and password.

## Keyboard Shortcuts
| Key | Action |
| --- | --- |
| `Alt+Q` | Quit. |
| `F1` | Toggle help panel showing shortcuts. |
| `Up` | One channel up. |
| `Down` | One channel down. |
| `Alt+A` | Add chat (room or user). |
| `Alt+J` | Join/add chat (room or user). |
| `Alt+D` | Delete chat (y/n confirm). |
| `Ctrl+A` | Accept invite. |
| `Ctrl+D` | Decline invite. |
| `Alt+V` | Start verification (SAS). |
| `Enter` | When input empty (single-line): open URL under cursor, or open the selected attachment message. |
| `Enter` | Send message (single-line) or insert newline (multi-line). |
| `file://<path>` | Send attachment from disk. |
| `Alt+Enter` | Toggle multi-line input. |
| `Left`/`Right` | Move cursor in input. |
| `Alt+Left`/`Alt+Right` | Jump word in input. |
| `Esc` | Reset message selection or close channel selection popup. |
| `Alt+Up` | Select previous message. |
| `Alt+Down` | Select next message. |
| `Alt+Y` | Copy message content to clipboard. |
| `Esc` | Close help panel. |
| `Up` | Previous line. |
| `Down`/`PgDown` | Next line. |

### Building from Source
```text
git clone https://github.com/kullbachxyz/marty
cd marty
cargo build --release
sudo cp target/release/marty /usr/local/bin/
```

## ToDo
- [ ] AUR release
- [ ] Project Page
- [x] improve help page
- [x] Desktop notification support
- [x] Reply to message
- [x] Read receipts
- [x] Attachment send
- [x] Attachment support (xdg-open)
- [x] User verification support
- [x] Message Input Editing
- [x] Backfill messages since last run
- [x] Invite support
- [x] Data Encryption at rest
- [x] Session Verification
- [x] Adding/Deleting chats
- [x] matrix-sdk implementation
- [x] Basic TUI layout


## Project Structure
```text
marty/
├── src/
│   ├── main.rs         # TUI, input handling, and app state
│   ├── matrix.rs       # Matrix client, sync, and commands
│   ├── config.rs       # Config + data directories
│   └── storage.rs      # Encrypted message storage
├── Cargo.toml
└── README.md
```

## Data Locations
- `~/.config/marty/config` Config file (accounts, active profile, encrypted session blob).
- `~/.local/share/marty/crypto/` Matrix SDK encrypted crypto store (keys, device state).
- `~/.local/share/marty/messages/` Encrypted local message archive per room.
- `~/.local/share/marty/attachments/<date>/` Downloaded attachments by date.

# marty

A Matrix TUI client inspired by gurk-rs.

## Features
- Slim channel list, message view, and input box layout
- Matrix login with persistent sessions
- E2EE with SAS emoji verification
- Encrypted local message archive (passphrase protected)
- Join rooms or start DMs from the TUI

## Installation
- Install Rust (stable) and Cargo
- Build and run:
  - `cargo run`

### Building from Source
```text
git clone https://github.com/kullbachxyz/marty
cd marty
cargo build --release
sudo cp target/release/marty /usr/local/bin/
```

## ToDo
- [ ] multi account support
- [ ] Project Page
- [ ] improve help page
- [ ] Desktop notification support
- [ ] Persist [accounts.session_encrypted] in the DB
- [ ] Attachment support (xdg-open)
- [ ] User verification support
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
├── keybinds.md         # Keybinding reference
├── Cargo.toml
└── README.md
```

## Data Locations
- `~/.config/marty/config` Config file (accounts, active profile, encrypted session blob).
- `~/.local/share/marty/crypto/` Matrix SDK encrypted crypto store (keys, device state).
- `~/.local/share/marty/messages/` Encrypted local message archive per room.

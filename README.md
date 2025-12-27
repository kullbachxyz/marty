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

## Data Locations
- `~/.config/marty/config` Config file (accounts, active profile, encrypted session blob).
- `~/.local/share/marty/crypto/` Matrix SDK encrypted crypto store (keys, device state).
- `~/.local/share/marty/messages/` Encrypted local message archive per room.

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

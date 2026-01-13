# Repository Guidelines

## Project Structure & Module Organization
- `src/main.rs`: TUI entrypoint, input handling, and app state.
- `src/matrix.rs`: Matrix client, sync loop, and command handling.
- `src/storage.rs`: Encrypted message storage and persistence helpers.
- `src/config.rs`: Config loading, paths, and profile data.
- `keybinds.md`: User-facing keybinding reference.

## Build, Test, and Development Commands
- `cargo run`: Build and run the TUI client in debug mode.
- `cargo build --release`: Build an optimized binary (`target/release/marty`).
- `cargo fmt`: Format Rust sources with rustfmt.
- `cargo clippy`: Run lints to catch common issues.

## Coding Style & Naming Conventions
- Indentation: 4 spaces (Rust default); keep line widths reasonable.
- Naming: `snake_case` for functions/variables, `PascalCase` for types, `SCREAMING_SNAKE_CASE` for constants.
- Prefer clear, short helpers in `src/matrix.rs` and `src/storage.rs` over deep nesting.

## Testing Guidelines
- There are currently no dedicated test modules.
- If you add tests, use `#[cfg(test)]` in the relevant module and run `cargo test`.
- Name tests descriptively, e.g., `encrypts_and_decrypts_roundtrip`.

## Commit & Pull Request Guidelines
- Commit messages are short, lowercase, present-tense statements (e.g., “implemented reply to message”).
- PRs should include a concise description, affected areas (files/modules), and manual test notes.
- Update `README.md` or `keybinds.md` when behavior or user-facing flows change.

## Configuration & Data Locations
- Config: `~/.config/marty/config` (accounts, active profile, encrypted session blob).
- Data: `~/.local/share/marty/` for crypto store, messages, and attachments.
- Avoid logging secrets; prefer redaction when troubleshooting.

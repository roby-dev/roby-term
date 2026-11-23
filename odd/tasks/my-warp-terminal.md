# Feature: my-warp-terminal
Objective: Build a cross-platform (Windows & macOS), minimal-RAM Rust terminal emulator with custom 2D UI, session sidebar with shell selector, user/session color differentiation, and a Warp-style syntax-colored input bar.
Branch: `feat/my-warp-terminal` | Delivery: `ask-on-risk` | Forecast: ~1,400 authored lines | Test runner: `cargo test`

## Specs

- **S1 — Cross-platform Rust terminal (Windows & macOS)**: `"queireo crear una terminal con rust, compatible con windows y macos"`.
  - Acceptance: Compiles and runs natively on Windows and macOS using cross-platform crates (`winit`, `softbuffer`, `fontdue`, `portable-pty`, `vte`), with platform-specific shell discovery and font lookup behind `#[cfg(target_os = ...)]`.
  - Checks: `cargo test`, `cargo build --release`.
- **S2 — Session sidebar with shell selector**: `"labartra lateral con als sesioens (que puedo seleccionar que temrinal usar en caso de windws temrinal pwoewrershel. etc)"`.
  - Acceptance: Left sidebar displays active terminal sessions, allows switching between sessions, closing sessions, and opening a new session by selecting an available shell (Windows: PowerShell 7 / Windows PowerShell / CMD / Git Bash / WSL; macOS: zsh / bash / fish).
  - Checks: Unit tests for shell discovery and session lifecycle; manual/UI structural verification.
- **S3 — User-differentiated colors & syntax/parameter coloring**: `"que la tarminal se difenrete por el usuario con colore,s y ls parametros tamibne cosas asi"`.
  - Acceptance: Detects current OS username and assigns a distinct deterministic/configurable user color theme & badge (and distinct accent colors per session/shell), plus colorizes commands, flags/parameters, strings, paths, variables, and operators.
  - Checks: Unit tests for user color derivation, theme palettes, and syntax token classification.
- **S4 — Minimal RAM & custom lightweight UI**: `"OJO; quiero que consuma lo minimo de RAM (no como warp); osea no uses librerias pesadsa, y si es UI tu mismo hazla, es sencillo la verdad, pore so queremos optimizar los recursos"`.
  - Acceptance: No WebView/Tauri/Electron, no `wgpu` GPU driver overhead, no `tokio` runtime, no heavy GUI framework. Custom 2D pixel renderer (`softbuffer` + `fontdue`), compact cell grid with bounded scrollback ring buffer, and std threads for PTY I/O.
  - Checks: `Cargo.toml` dependency audit, bounded scrollback unit tests, release binary build.
- **S5 — Warp-style custom input bar**: `"barra input propia etstilo warp"`.
  - Acceptance: Dedicated bottom input bar with cursor editing, command history (Up/Down), live syntax highlighting (command, `--flags`/`-f`, strings, paths, env vars, pipes/operators), Enter to submit to active PTY, and automatic/manual passthrough mode when a full-screen TUI (alternate screen) is active.
  - Checks: Unit tests for input editing, history navigation, tokenization, and passthrough toggling.
- **S6 — Rust toolchain installation**: `"instalas rust sin problema"`.
  - Acceptance: `rustc` and `cargo` installed and functional on this machine.
  - Checks: `rustc --version && cargo --version`.

## Tasks

- [x] **T1** (`S1, S4, S6`, route: inline, commit: `9b816d8`): Install Rust toolchain, init git branch `feat/my-warp-terminal`, and scaffold lean `Cargo.toml` + module layout.
- [x] **T2** (`S1, S2, S3`, route: inline, commit: `9b816d8`): Implement shell discovery (Windows/macOS), user identity & color theme engine, and multi-session PTY manager (`portable-pty`) with unit tests.
- [x] **T3** (`S1, S4`, route: inline, commit: `f9409bd`): Implement compact VT/ANSI terminal emulator (`vte`) with bounded ring-buffer scrollback and alternate-screen detection, with unit tests.
- [x] **T4** (`S3, S5`, route: inline, commit: `f9409bd`): Implement Warp-style input bar state, command history, and live command/parameter syntax tokenizer with unit tests.
- [x] **T5** (`S1, S2, S3, S4, S5`, route: inline, commit: `3726a1c`): Implement custom 2D software renderer (`winit` + `softbuffer` + `fontdue`) with session sidebar, shell picker, user/session colors, terminal viewport, Warp input bar, and `README.md`.

## Log

- **L1 (2026-11-23)**: `"conoces warp?, queireo crear una terminal con rust, compatible con windows y macos.  que tenga lo que me gusta que es labartra lateral con als sesioens (que puedo seleccionar que temrinal usar en caso de windws temrinal pwoewrershel. etc) que la tarminal se difenrete por el usuario con colore,s y ls parametros tamibne cosas asi."`
- **L2 (2026-11-23)**: `"si no sabesm esta es su repo https://github.com/warpdotdev/warp"` / `"OJO; quiero que consuma lo minimo de RAM (no como warp); osea no uses librerias pesadsa, y si es UI tu mismo hazla, es sencillo la verdad, pore so queremos optimizar los recursos"` / `"barra input propia etstilo warp. instalas rust sin problema"`
- **L3 (2026-11-23)**: Environment check confirmed empty workspace `D:\FUENTES\my-warp`, VS 2022 Professional VC Tools installed, `git` and `winget` available, Rust not yet installed. Proceeding with T1.
- **L4 (2026-11-23)**: Completed T1–T5. `cargo test` passed (13/13 unit tests) and `cargo build --release` produced `target/release/my-warp.exe` (994.5 KB) with zero warnings.

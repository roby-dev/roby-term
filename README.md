# my-warp

Ultra-low-RAM, cross-platform (Windows & macOS) Rust terminal emulator with a custom 2D software UI, session sidebar with shell picker, user-differentiated colors, and a Warp-style syntax-colored input bar.

## Why It Stays Light on RAM

- **No WebView, Electron, or Tauri**: renders directly into a native 2D framebuffer (`winit` + `softbuffer`).
- **No GPU driver bloat (`wgpu`) or async runtime (`tokio`)**: uses `std::thread` for PTY readers and event-driven redraws (`ControlFlow::Wait`), keeping idle CPU near 0%.
- **On-demand glyph rasterization**: `fontdue` rasterizes only the characters actually displayed.
- **Bounded scrollback**: each session uses a fixed-cap ring buffer (`DEFAULT_MAX_SCROLLBACK = 1500` lines) with compact terminal cells.

## Features

- **Session Sidebar & Shell Selector**: switch, close, and spawn new sessions with auto-detected shells:
  - **Windows**: PowerShell 7 (`pwsh`), Windows PowerShell, Command Prompt (`cmd`), Git Bash, WSL.
  - **macOS**: Zsh, Bash, Fish, PowerShell.
- **User & Session Color Differentiation**: detects the OS username (`USERNAME` / `USER`) to derive a distinct user accent color & badge, plus unique accent colors per shell type.
- **Warp-Style Input Bar**: dedicated command bar with cursor editing, Up/Down history, and live syntax highlighting for commands, `--flags`/`-f`, `"strings"`, file paths, `$ENV`/`%ENV%` variables, operators (`|`, `&&`, `;`, `>`), and comments.
- **Automatic TUI Passthrough**: detects alternate-screen apps (`vim`, `htop`, etc.) and forwards keystrokes directly to the PTY (also toggleable manually via `Ctrl+Shift+P` or clicking the header pill).

## Shortcuts

| Shortcut | Action |
| --- | --- |
| `Enter` | Run command in active session |
| `Up` / `Down` | Navigate command history (Warp input mode) |
| `Ctrl+T` (`Cmd+T` on macOS) | Toggle New Session shell picker |
| `Ctrl+W` (`Cmd+W` on macOS) | Close active session |
| `Ctrl+Tab` / `Ctrl+1..9` | Switch between sessions |
| `Ctrl+Shift+P` | Toggle Passthrough vs Warp Input mode |
| `Ctrl+C` | Send SIGINT to active PTY |
| `Ctrl+U` | Clear input bar |

## Build & Run

```bash
# Run unit tests
cargo test

# Build optimized release binary
cargo build --release

# Launch
cargo run --release
```

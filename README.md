# Roby-Term

Ultra-low-RAM, cross-platform (Windows & macOS) Rust terminal emulator with a custom 2D software UI, session sidebar with shell picker, user-differentiated colors, and a syntax-colored command input bar.

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
- **100% Local History, Tab Completion & Frecency Predictions**: persists executed commands locally (`%APPDATA%\my-warp\history.log` on Windows, `~/Library/Application Support/my-warp/history.log` on macOS; skips commands starting with a space), ranking suggestions by frequency + recency + shell match alongside real `Tab` completion for files, folders, and `PATH` executables.
- **User Preferences (Settings Panel)**: click **Settings** in the top-right header or press `Ctrl+,` (`Cmd+,` on macOS) to pick accent presets or type any custom `#HEX` color (e.g., `#A88BD6`) for accent, background, commands, and flags, plus switch system monospace fonts and font size live. Auto-saved locally to `config.ini`.
- **Live Context Bar (CWD + Git Branch)**: displays your active folder and current Git branch in the header by reading `.git/HEAD` directly (0ms, zero subprocesses).
- **Command Palette & Local Snippets (`Ctrl+K`)**: filter, save (`name = command`), delete, insert (`Enter`), or run (`Shift+Enter`) reusable command workflows stored locally in `snippets.tsv`.
- **Split Panes (`Ctrl+Shift+D`)**: run two terminal sessions side-by-side in the same window; click either pane to focus.
- **Session Rename & Activity Badges**: double-click any session in the sidebar (or press `F2`) to rename it inline, with a green dot for unread background output and a red dot when a process exits.
- **Local Listening Ports Panel (`Ctrl+Shift+O`)**: click **Ports** in the header to view local TCP ports in `LISTENING` state (process + PID), open `http://localhost:<port>` in your browser, or kill a stuck process in 1 click.
- **1-Click Theme Presets in Settings**: switch between *Roby Violet*, *Tokyo Night*, *Catppuccin*, *Nord*, and *Gruvbox* (or customize any `#HEX` color).
- **Drag & Drop Files & Folders**: drop any file or folder onto the window to insert its path (or pre-fill `cd "..."` for folders).
- **Duplicate Session (`Ctrl+Shift+T`) & Per-Session Colors**: duplicate the active session in the exact same `cwd` via **Dup**, and click any session's shell badge in the sidebar to cycle its custom accent color.
- **Optional CLI Agent Token Plugins (Settings)**: opt-in toggles in Settings (`Ctrl+,`) for **Antigravity (`agy`)**, **Claude Code (`claude`)**, and **Codex (`codex`)** that scan local 24h `.jsonl` logs in the background and show live token totals in the sidebar.
- **Full Local Workspace & Session Persistence**: closing Roby-Term automatically saves all open sessions (custom names, shells, `cwd`, split layout, and full scrollback transcripts) locally in `workspace/` and restores them on the next launch.
- **Built-In Colorized `ls` / `ll` / `la`**: renders folders first (in your accent color), files color-coded by type (`COD`, `CFG`, `EXE`, `DOC`, `BIN`), human-readable sizes, and clickable entries identically on Windows (PowerShell/CMD/Git Bash) and macOS (Zsh/Bash/Fish). Pass `--raw` or pipe (`ls | ...`) to invoke the shell's native command.
- **Quick Zoom & Custom App Icon**: zoom font size on the fly with `Ctrl++` / `Ctrl+-` / `Ctrl+0`, with a custom embedded app icon (and GUI-only Windows subsystem in release builds).
- **Native Copy/Paste & Mouse Selection**: drag-select text in the terminal and press `Ctrl+C` (`Cmd+C`) to copy, or `Ctrl+V` (`Cmd+V`) to paste into the input bar, search bar, or settings fields (zero external clipboard crates).
- **Warp-Style Command Blocks**: visually separates each executed command with an accent gutter and inline **`Copy`** / **`Re-run`** buttons.
- **Scrollback Search (`Ctrl+F`)**: case-insensitive search across scrollback and viewport with match counter and jump navigation (`Enter` / `Shift+Enter`).
- **Clickable Paths & URLs**: hover and click any existing file/directory path (e.g., `D:\wauth`, `src/main.rs:42`) to open it in Explorer/Finder, or any `http://` / `https://` URL to open it in your browser.
- **Automatic TUI Passthrough**: detects alternate-screen apps (`vim`, `htop`, etc.) and forwards keystrokes directly to the PTY (also toggleable manually via `Ctrl+Shift+P` or clicking the header pill).

## Shortcuts

| Shortcut | Action |
| --- | --- |
| `Enter` | Run command in active session |
| `Tab` / `→` (at end) | Complete files/folders/commands or accept ghost prediction |
| `Ctrl+N` / `Ctrl+P` | Cycle through top-5 completions & suggestions |
| `Ctrl+K` (`Cmd+K` on macOS) | Open / close Snippets & Workflows palette |
| `Ctrl+Shift+D` | Toggle side-by-side Split Panes |
| `F2` / Double-click session | Rename session inline |
| `Ctrl++` / `Ctrl+-` / `Ctrl+0` | Zoom font size in / out / reset |
| `Ctrl+F` (`Cmd+F` on macOS) | Search output & scrollback (`Enter` / `Shift+Enter` next/prev) |
| `Ctrl+C` (`Cmd+C`) | Copy selected text (or send SIGINT when nothing is selected) |
| `Ctrl+Shift+C` | Copy last command block output |
| `Ctrl+V` (`Cmd+V`) | Paste from OS clipboard |
| `Ctrl+,` (`Cmd+,` on macOS) | Open / close Preferences (Settings) panel |
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

## Credits

Built with [Gentle-AI](https://github.com/Gentleman-Programming/gentle-ai) by [Gentleman Programming](https://github.com/Gentleman-Programming).

<a href="https://github.com/Gentleman-Programming/gentle-ai"><img src="https://raw.githubusercontent.com/Gentleman-Programming/gentle-ai/main/docs/assets/brand/built-with-gentle-ai.png" alt="Built with Gentle-AI" width="220" /></a>

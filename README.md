# Velt

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
- **Velt Input Bar**: dedicated command bar with cursor editing, Up/Down history, and live syntax highlighting for commands, `--flags`/`-f`, `"strings"`, file paths, `$ENV`/`%ENV%` variables, operators (`|`, `&&`, `;`, `>`), and comments.
- **100% Local History, Tab Completion & Frecency Predictions**: persists executed commands locally (`%APPDATA%\my-warp\history.log` on Windows, `~/Library/Application Support/my-warp/history.log` on macOS; skips commands starting with a space), ranking suggestions by frequency + recency + shell match alongside real `Tab` completion for files, folders, and `PATH` executables.
- **User Preferences (Settings Panel)**: click **Settings** in the top-right header or press `Ctrl+,` (`Cmd+,` on macOS) to pick accent presets or type any custom `#HEX` color (e.g., `#A88BD6`) for accent, background, commands, and flags, plus switch system monospace fonts and font size live. Auto-saved locally to `config.ini`.
- **Live Context Bar (CWD + Git Branch)**: displays your active folder and current Git branch in the header by reading `.git/HEAD` directly (0ms, zero subprocesses).
- **Command Palette & Local Snippets (`Ctrl+K`)**: filter, save (`name = command`), delete, insert (`Enter`), or run (`Shift+Enter`) reusable command workflows stored locally in `snippets.tsv`.
- **Split Panes (`Ctrl+Shift+D`)**: run two terminal sessions side-by-side in the same window; click either pane to focus.
- **Session Rename & Activity Badges**: double-click any session in the sidebar (or press `F2`) to rename it inline, with a green dot for unread background output and a red dot when a process exits.
- **Collapsible Sidebar (`Ctrl+B`)**: hide or show the sidebar anytime to reclaim full terminal width for Split Panes and CLI agents.
- **Fuzzy History Search (`Ctrl+R`)**: interactive multi-token search over your persisted local command history (`Enter` to run immediately, `Tab` to edit).
- **Local Listening Ports Panel (`Ctrl+Shift+O`)**: click **Ports** in the header to view local TCP ports in `LISTENING` state (process + PID), open `http://localhost:<port>` in your browser, or kill a stuck process in 1 click.
- **1-Click Theme Presets in Settings**: switch between *Velt Violet*, *Tokyo Night*, *Catppuccin*, *Nord*, and *Gruvbox* (or customize any `#HEX` color).
- **Drag & Drop Files & Folders**: drop any file or folder onto the window to insert its path (or pre-fill `cd "..."` for folders).
- **Duplicate Session (`Ctrl+Shift+T`) & Per-Session Colors**: duplicate the active session in the exact same `cwd` via **Dup**, and click any session's shell badge in the sidebar to cycle its custom accent color.
- **Optional CLI Agent Token Plugins (Settings)**: opt-in toggles in Settings (`Ctrl+,`) for **Antigravity (`agy`)**, **Claude Code (`claude`)**, and **Codex (`codex`)** that scan local 24h `.jsonl` logs in the background and show live token totals in the sidebar.
- **Full Local Workspace & Session Persistence**: closing Velt automatically saves all open sessions (custom names, shells, `cwd`, split layout, and full scrollback transcripts) locally in `workspace/` and restores them on the next launch.
- **Built-In Colorized `ls` / `ll` / `la`**: renders folders first (in your accent color), files color-coded by type (`COD`, `CFG`, `EXE`, `DOC`, `BIN`), human-readable sizes, and clickable entries identically on Windows (PowerShell/CMD/Git Bash) and macOS (Zsh/Bash/Fish). Pass `--raw` or pipe (`ls | ...`) to invoke the shell's native command.
- **Quick Zoom & Custom App Icon**: zoom font size on the fly with `Ctrl++` / `Ctrl+-` / `Ctrl+0`, with a custom embedded app icon (and GUI-only Windows subsystem in release builds).
- **Native Copy/Paste & Mouse Selection**: drag-select text in the terminal and press `Ctrl+C` (`Cmd+C`) to copy. Use the mouse wheel while dragging to extend the selection through scrollback; after release, scrolling keeps the same text selected. Use `Ctrl+V` (`Cmd+V`) to paste into the input bar, search bar, or settings fields (zero external clipboard crates).
- **Warp-Style Command Blocks & Execution Timer**: visually separates each executed command with an accent gutter, elapsed duration badge (`42ms`, `11.4s`, `2m 05s`), and inline **`Copy`** / **`Re-run`** buttons.
- **Scrollback Search (`Ctrl+F`)**: case-insensitive search across scrollback and viewport with match counter and jump navigation (`Enter` / `Shift+Enter`).
- **Clickable Paths & URLs**: click an existing file path (e.g., `src/main.rs:42`) to open its containing folder in Explorer/Finder, a directory path (e.g., `D:\wauth`) to open that folder, or an HTTP(S) URL to open it in your browser. Use quotes around paths with spaces.
- **Automatic Update Detection & 1-Click Self-Updater**: checks GitHub Releases asynchronously on startup (zero extra crates), shows a header `Update vX.Y.Z` pill when a newer release exists, downloads & verifies the platform binary (`MZ` / Mach-O header check), swaps it safely in place, and relaunches with your workspace intact (also manually triggerable from **Settings**).
- **Automatic TUI Passthrough**: detects alternate-screen apps and CLI agents (`claude`, `agy`, `codex`, `opencode`, `vim`, `htop`, etc.), forwarding keystrokes (`Alt+<key>`, `Shift+Enter`, `Shift+Tab`, and atomic bracketed paste) directly to the PTY (also toggleable manually via `Ctrl+Shift+P` or clicking the header pill).
- **ACP Agents (header `ACP` toggle)**: chat with **Antigravity** (`agy acp`), **Codex** (`codex-acp`) and **Claude Code** (`claude-agent-acp`, from `npm install -g @agentclientprotocol/claude-agent-acp`) in one conversation. Start a prompt with `@agy`, `@codex` or `@claude` to choose who answers; without a mention the selected agent answers. The addressed agent receives the turns it has not seen, so earlier replies are shared across providers. Each new agent session is also asked to search and update Engram memory when its tools are available. One turn runs at a time.

## Shortcuts

| Shortcut | Action |
| --- | --- |
| `Enter` | Run command in active session |
| `Tab` / `→` (at end) | Complete files/folders/commands or accept ghost prediction |
| `Ctrl+N` / `Ctrl+P` | Cycle through top-5 completions & suggestions |
| `PageUp` / `PageDown` | Scroll output by page (normal mode); use `Shift+PageUp` / `Shift+PageDown` in passthrough |
| `Ctrl+Home` / `Ctrl+End` | Jump to top / bottom of session scrollback (also `Shift+Home` / `Shift+End` in passthrough) |
| `Ctrl+B` (`Cmd+B` on macOS) | Toggle collapsible sidebar (`Ctrl+Shift+B` in passthrough) |
| `Ctrl+R` (`Cmd+R` on macOS) | Open / close Fuzzy History search (`Ctrl+Shift+R` in passthrough) |
| `Ctrl+K` (`Cmd+K` on macOS) | Open / close Snippets & Workflows palette (`Ctrl+Shift+K` in passthrough) |
| `Ctrl+Shift+O` | Open / close Local Listening Ports panel (IPv4 & IPv6) |
| `Ctrl+Shift+D` | Toggle side-by-side Split Panes |
| `Ctrl+Shift+T` | Duplicate active session in same folder |
| `F2` / Double-click session | Rename session inline |
| `Ctrl++` / `Ctrl+-` / `Ctrl+0` | Zoom font size in / out / reset |
| `Ctrl+F` (`Cmd+F` on macOS) | Search output & scrollback (`Enter` / `Shift+Enter` next/prev) |
| `Ctrl+C` (`Cmd+C`) | Copy selected text (or send SIGINT when nothing is selected) |
| `Ctrl+Shift+C` | Copy last command block output |
| `Ctrl+V` (`Cmd+V`) | Paste from OS clipboard (atomic bracketed paste in passthrough) |
| `Ctrl+,` (`Cmd+,` on macOS) | Open / close Preferences (Settings) panel |
| `Up` / `Down` | Navigate command history (Velt Input mode) |
| `Ctrl+T` (`Cmd+T` on macOS) | Toggle New Session shell picker |
| `Ctrl+W` (`Cmd+W` on macOS) | Close active session (`Ctrl+Shift+W` in passthrough) |
| `Ctrl+Tab` / `Ctrl+1..9` | Switch between sessions |
| `Ctrl+Shift+P` | Toggle Passthrough vs Velt Input mode |
| `Ctrl+U` | Clear input bar |

## Download & Build

Pre-built binaries for Windows (`velt-windows-x64.exe`) and macOS Apple Silicon (`Velt-macos-arm64.dmg` / `velt-macos-arm64`) are published on [GitHub Releases](https://github.com/roby-dev/velt/releases/latest).

```bash
# Run unit tests
cargo test

# Build optimized release binary (target/release/velt)
cargo build --release

# Launch
cargo run --release
```

## Credits

Built with [Gentle-AI](https://github.com/Gentleman-Programming/gentle-ai) by [Gentleman Programming](https://github.com/Gentleman-Programming).

<a href="https://github.com/Gentleman-Programming/gentle-ai"><img src="https://raw.githubusercontent.com/Gentleman-Programming/gentle-ai/main/docs/assets/brand/built-with-gentle-ai.png" alt="Built with Gentle-AI" width="220" /></a>

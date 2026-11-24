#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use my_warp::clipboard::{copy_text, paste_text};
use my_warp::complete::{
    complete_input, is_completing_path, longest_common_prefix, strip_verbatim_prefix,
    CompletionItem, CompletionKind, PathCommandIndex,
};
use my_warp::config::{
    parse_hex_color, UserConfig, DEFAULT_FONT_SIZE, MAX_FONT_SIZE, MIN_FONT_SIZE, PRESET_ACCENTS,
    THEME_PRESETS,
};
use my_warp::history::HistoryStore;
use my_warp::input::InputBar;
use my_warp::plugins::{collect_today_usage, AgentUsage};
use my_warp::ports::{kill_process, list_listening_ports, ListeningPort};
use my_warp::session::SessionManager;
use my_warp::shell::{default_shell, discover_shells, ShellProfile};
use my_warp::snippets::SnippetStore;
use my_warp::terminal::{detect_link_at_col, open_link, open_url_in_browser, LinkTarget};
use my_warp::theme::{Theme, UserIdentity};
use my_warp::ui::{
    build_app_icon_rgba, discover_system_mono_fonts, render_app, FontRenderer, HitTarget, Layout,
    Rect, SettingsField, SystemFont, UiState,
};
use my_warp::updater::{
    check_latest_release, cleanup_old_binary, download_and_install_update, relaunch_exe,
    ReleaseInfo, UpdateStatus, CURRENT_VERSION, RELEASES_PAGE_URL,
};
use my_warp::workspace::{default_workspace_dir, load_workspace, save_workspace};
use softbuffer::{Context, Surface};
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Icon, Window, WindowId};

const MAX_VISIBLE_SUGGESTIONS: usize = 5;

struct App {
    proxy: EventLoopProxy<()>,
    window: Option<Rc<Window>>,
    _context: Option<Context<Rc<Window>>>,
    surface: Option<Surface<Rc<Window>, Rc<Window>>>,
    available_fonts: Vec<SystemFont>,
    font: FontRenderer,
    base_user: UserIdentity,
    user: UserIdentity,
    config: UserConfig,
    theme: Theme,
    shells: Vec<ShellProfile>,
    sessions: SessionManager,
    history: HistoryStore,
    snippets: SnippetStore,
    cmd_index: PathCommandIndex,
    agent_usage: AgentUsage,
    usage_rx: Option<Receiver<AgentUsage>>,
    ports_rx: Option<Receiver<Vec<ListeningPort>>>,
    update_check_rx: Option<Receiver<Result<Option<ReleaseInfo>, String>>>,
    update_check_manual: bool,
    update_install_rx: Option<Receiver<Result<(String, std::path::PathBuf), String>>>,
    last_usage_refresh: Instant,
    ui: UiState,
    layout: Layout,
    scale_factor: f64,
    phys_width: u32,
    phys_height: u32,
    modifiers: ModifiersState,
    cursor_pos: (u32, u32),
    hovered_link_target: Option<LinkTarget>,
    last_session_click: Option<(usize, Instant)>,
}

impl App {
    fn new(proxy: EventLoopProxy<()>) -> Result<Self, String> {
        cleanup_old_binary();
        let config = UserConfig::load_default();
        let available_fonts = discover_system_mono_fonts();
        let font = FontRenderer::load_with_config(
            config.font_name.as_deref(),
            config.font_size,
            &available_fonts,
        )?;
        let base_user = UserIdentity::detect();
        let user = base_user.with_accent_override(config.accent);
        let theme = Theme::for_user_with_config(&user, &config);
        let shells = discover_shells();
        let history = HistoryStore::load_default();
        let snippets = SnippetStore::load_default();
        let cmd_index = PathCommandIndex::scan_system_path();
        let layout = Layout::compute(1120, 720);
        let (cols, rows) = layout.terminal_grid_size(font.cell_w, font.cell_h);

        let wake_proxy = proxy.clone();
        let wakeup = Arc::new(move || {
            let _ = wake_proxy.send_event(());
        });

        let mut sessions =
            SessionManager::new(cols, rows, theme.fg_primary, theme.bg_terminal, Some(wakeup));

        let recent_history = history.recent_commands(200);
        let restored = default_workspace_dir()
            .and_then(|dir| load_workspace(&dir))
            .map(|snap| sessions.restore_snapshot(&snap, &shells, &recent_history))
            .unwrap_or(0);

        if restored == 0 {
            let initial_shell = default_shell(&shells);
            if sessions.spawn_session(initial_shell).is_ok() {
                if let Some(sess) = sessions.active_mut() {
                    sess.input.seed_history(recent_history);
                }
            }
        }

        let ui = UiState::new(&config, user.accent);

        let mut app = Self {
            proxy,
            window: None,
            _context: None,
            surface: None,
            available_fonts,
            font,
            base_user,
            user,
            config,
            theme,
            shells,
            sessions,
            history,
            snippets,
            cmd_index,
            agent_usage: AgentUsage::default(),
            usage_rx: None,
            ports_rx: None,
            update_check_rx: None,
            update_check_manual: false,
            update_install_rx: None,
            last_usage_refresh: Instant::now(),
            ui,
            layout,
            scale_factor: 1.0,
            phys_width: 1120,
            phys_height: 720,
            modifiers: ModifiersState::empty(),
            cursor_pos: (0, 0),
            hovered_link_target: None,
            last_session_click: None,
        };
        app.refresh_plugins_async();
        app.check_updates_async(false);
        Ok(app)
    }

    fn enabled_plugin_count(&self) -> usize {
        (self.config.plugin_agy as usize)
            + (self.config.plugin_claude as usize)
            + (self.config.plugin_codex as usize)
    }

    fn refresh_plugins_async(&mut self) {
        let (a, c, x) = (
            self.config.plugin_agy,
            self.config.plugin_claude,
            self.config.plugin_codex,
        );
        self.last_usage_refresh = Instant::now();
        if !a && !c && !x {
            self.agent_usage = AgentUsage::default();
            self.usage_rx = None;
            return;
        }
        let (tx, rx) = mpsc::channel();
        let wake = self.proxy.clone();
        self.usage_rx = Some(rx);
        let _ = thread::Builder::new()
            .name("plugin-usage".into())
            .spawn(move || {
                let usage = collect_today_usage(a, c, x);
                let _ = tx.send(usage);
                let _ = wake.send_event(());
            });
    }

    fn poll_plugin_usage(&mut self) -> bool {
        if let Some(ref rx) = self.usage_rx {
            if let Ok(usage) = rx.try_recv() {
                self.agent_usage = usage;
                self.usage_rx = None;
                return true;
            }
        }
        if self.enabled_plugin_count() > 0
            && self.usage_rx.is_none()
            && self.last_usage_refresh.elapsed() >= Duration::from_secs(30)
        {
            self.refresh_plugins_async();
        }
        false
    }

    fn refresh_ports_async(&mut self) {
        self.ui.ports.loading = true;
        let (tx, rx) = mpsc::channel();
        let wake = self.proxy.clone();
        self.ports_rx = Some(rx);
        let _ = thread::Builder::new()
            .name("ports-scan".into())
            .spawn(move || {
                let list = list_listening_ports();
                let _ = tx.send(list);
                let _ = wake.send_event(());
            });
    }

    fn poll_ports(&mut self) -> bool {
        if let Some(ref rx) = self.ports_rx {
            if let Ok(list) = rx.try_recv() {
                self.ui.ports.ports = list;
                self.ui.ports.loading = false;
                self.ports_rx = None;
                return true;
            }
        }
        false
    }

    fn check_updates_async(&mut self, manual: bool) {
        if self.update_install_rx.is_some() {
            return;
        }
        if self.update_check_rx.is_some() {
            if manual {
                self.update_check_manual = true;
                self.ui.update_status = UpdateStatus::Checking;
            }
            return;
        }
        self.update_check_manual = manual;
        if manual {
            self.ui.update_status = UpdateStatus::Checking;
        }
        let (tx, rx) = mpsc::channel();
        let wake = self.proxy.clone();
        match thread::Builder::new()
            .name("update-check".into())
            .spawn(move || {
                let res = check_latest_release();
                let _ = tx.send(res);
                let _ = wake.send_event(());
            }) {
            Ok(_) => self.update_check_rx = Some(rx),
            Err(e) => {
                if manual {
                    self.ui.update_status = UpdateStatus::Error {
                        message: format!("failed to start update check: {e}"),
                        from_install: false,
                    };
                    self.ui.toast = Some(format!("Update check failed: {e}"));
                } else {
                    self.ui.update_status = UpdateStatus::Idle;
                }
            }
        }
    }

    fn install_update_async(&mut self, info: ReleaseInfo) {
        if self.update_install_rx.is_some() {
            return;
        }
        self.ui.update_status = UpdateStatus::Downloading(info.version.clone());
        self.ui.toast = Some(format!("Downloading Roby-Term v{}...", info.version));
        let (tx, rx) = mpsc::channel();
        let wake = self.proxy.clone();
        match thread::Builder::new()
            .name("update-install".into())
            .spawn(move || {
                let res = download_and_install_update(&info);
                let _ = tx.send(res);
                let _ = wake.send_event(());
            }) {
            Ok(_) => self.update_install_rx = Some(rx),
            Err(e) => {
                self.ui.update_status = UpdateStatus::Error {
                    message: format!("failed to start updater: {e}"),
                    from_install: true,
                };
                self.ui.toast = Some(format!(
                    "Auto-update failed ({e}). Click to open Releases."
                ));
            }
        }
    }

    fn handle_update_click(&mut self) {
        match self.ui.update_status.clone() {
            UpdateStatus::Available(info) => {
                self.install_update_async(info);
            }
            UpdateStatus::ReadyToRestart { version, exe_path } => {
                if let Some(dir) = default_workspace_dir() {
                    let _ = save_workspace(&dir, &self.sessions.to_snapshot());
                }
                if relaunch_exe(&exe_path).is_ok() {
                    std::process::exit(0);
                } else {
                    self.ui.toast = Some(format!(
                        "Updated to v{version} — please close and reopen Roby-Term."
                    ));
                }
            }
            UpdateStatus::Checking | UpdateStatus::Downloading(_) => {}
            UpdateStatus::Error { from_install, .. } => {
                if from_install {
                    let _ = open_url_in_browser(RELEASES_PAGE_URL);
                    self.ui.update_status = UpdateStatus::Idle;
                } else {
                    self.check_updates_async(true);
                }
            }
            UpdateStatus::Idle | UpdateStatus::UpToDate => {
                self.check_updates_async(true);
            }
        }
    }

    fn poll_updates(&mut self) -> bool {
        let mut changed = false;
        if let Some(ref rx) = self.update_check_rx {
            match rx.try_recv() {
                Ok(res) => {
                    let manual = self.update_check_manual;
                    self.update_check_rx = None;
                    self.update_check_manual = false;
                    changed = true;
                    match res {
                        Ok(Some(info)) => {
                            let ver = info.version.clone();
                            self.ui.update_status = UpdateStatus::Available(info);
                            if manual {
                                self.ui.toast = Some(format!("New version v{ver} available!"));
                            }
                        }
                        Ok(None) => {
                            self.ui.update_status = UpdateStatus::UpToDate;
                            if manual {
                                self.ui.toast =
                                    Some(format!("You are up to date (v{CURRENT_VERSION})!"));
                            }
                        }
                        Err(e) => {
                            if manual {
                                self.ui.update_status = UpdateStatus::Error {
                                    message: e.clone(),
                                    from_install: false,
                                };
                                self.ui.toast = Some(format!("Update check failed: {e}"));
                            } else {
                                self.ui.update_status = UpdateStatus::Idle;
                            }
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    let manual = self.update_check_manual;
                    self.update_check_rx = None;
                    self.update_check_manual = false;
                    changed = true;
                    if manual {
                        self.ui.update_status = UpdateStatus::Error {
                            message: "update check interrupted".to_string(),
                            from_install: false,
                        };
                        self.ui.toast = Some("Update check failed: interrupted".to_string());
                    } else {
                        self.ui.update_status = UpdateStatus::Idle;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(ref rx) = self.update_install_rx {
            match rx.try_recv() {
                Ok(res) => {
                    self.update_install_rx = None;
                    changed = true;
                    match res {
                        Ok((ver, exe_path)) => {
                            self.ui.update_status = UpdateStatus::ReadyToRestart {
                                version: ver.clone(),
                                exe_path,
                            };
                            self.ui.toast = Some(format!(
                                "Updated to v{ver}! Click 'Restart v{ver}' to apply."
                            ));
                        }
                        Err(e) => {
                            self.ui.update_status = UpdateStatus::Error {
                                message: e.clone(),
                                from_install: true,
                            };
                            self.ui.toast = Some(format!(
                                "Auto-update failed ({e}). Click to open Releases."
                            ));
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.update_install_rx = None;
                    changed = true;
                    self.ui.update_status = UpdateStatus::Error {
                        message: "update worker exited unexpectedly".to_string(),
                        from_install: true,
                    };
                    self.ui.toast =
                        Some("Auto-update failed. Click to open Releases.".to_string());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        changed
    }

    fn handle_dropped_file(&mut self, raw_path: std::path::PathBuf) {
        let path = strip_verbatim_prefix(raw_path);
        let raw_str = path.to_string_lossy().to_string();
        let formatted = if raw_str.contains(' ') {
            format!("\"{raw_str}\"")
        } else {
            raw_str
        };
        if let Some(sess) = self.sessions.active_mut() {
            if sess.effective_passthrough() {
                sess.write_bytes(formatted.as_bytes());
            } else if sess.input.text().trim().is_empty() && path.is_dir() {
                sess.input.set_text(format!("cd {formatted}"));
                self.ui.toast = Some("Dropped folder — press Enter to cd!".to_string());
            } else {
                sess.input.insert_str(&format!("{formatted} "));
                self.ui.toast = Some("Inserted dropped path!".to_string());
            }
            self.ui.suggestion_selected = 0;
            self.ui.suggestion_navigated = false;
        }
        self.request_redraw();
    }

    fn active_pane_rect(&self) -> Rect {
        if let Some((left_idx, right_idx)) = self.sessions.split_panes() {
            let (left_vp, right_vp) = self.layout.split_viewports();
            if self.sessions.active_index() == right_idx {
                return right_vp;
            }
            if self.sessions.active_index() == left_idx {
                return left_vp;
            }
        }
        self.layout.viewport
    }

    fn resize_sessions_for_current_layout(&mut self) {
        let vp = if self.sessions.is_split() {
            self.layout.split_viewports().0
        } else {
            self.layout.viewport
        };
        let (cols, rows) = Layout::viewport_grid_size(vp, self.font.cell_w, self.font.cell_h);
        self.sessions.resize_all(cols, rows);
    }

    fn save_workspace_state(&self) {
        if let Some(dir) = default_workspace_dir() {
            let snap = self.sessions.to_snapshot();
            let _ = save_workspace(&dir, &snap);
        }
    }

    fn toggle_split(&mut self) {
        let shell = self
            .sessions
            .active()
            .map(|s| s.shell.clone())
            .unwrap_or_else(|| default_shell(&self.shells));
        if let Ok(opened) = self.sessions.toggle_split(shell) {
            if opened {
                if let Some(sess) = self.sessions.active_mut() {
                    sess.input.seed_history(self.history.recent_commands(200));
                }
            }
            self.ui.clear_selection();
            self.resize_sessions_for_current_layout();
            self.save_workspace_state();
        }
    }

    fn hit_test_all(&self, px: u32, py: u32, suggestion_count: usize) -> HitTarget {
        if self.ui.history_modal.open && !self.ui.settings.open {
            let p = self.layout.palette_panel();
            if p.contains(px, py) {
                if self.layout.palette_close_rect().contains(px, py) {
                    return HitTarget::HistoryClose;
                }
                let count = self.history.search_fuzzy(&self.ui.history_modal.query, 7).len();
                for i in 0..count {
                    if self.layout.palette_row_rect(i).contains(px, py) {
                        return HitTarget::HistoryRow(i);
                    }
                }
                return HitTarget::None;
            }
        }

        if self.ui.ports.open && !self.ui.settings.open && !self.ui.palette.open {
            let p = self.layout.ports_panel();
            if p.contains(px, py) {
                if self.layout.ports_close_rect().contains(px, py) {
                    return HitTarget::PortsClose;
                }
                if self.layout.ports_refresh_rect().contains(px, py) {
                    return HitTarget::PortsRefresh;
                }
                let count = self.ui.ports.ports.len().min(8);
                for i in 0..count {
                    let (open_r, kill_r) = self.layout.ports_action_rects(i);
                    if open_r.contains(px, py) {
                        return HitTarget::PortOpen(i);
                    }
                    if kill_r.contains(px, py) {
                        return HitTarget::PortKill(i);
                    }
                }
                return HitTarget::None;
            }
        }

        if self.ui.palette.open && !self.ui.settings.open {
            let p = self.layout.palette_panel();
            if p.contains(px, py) {
                if self.layout.palette_close_rect().contains(px, py) {
                    return HitTarget::PaletteClose;
                }
                if self.layout.palette_add_rect().contains(px, py) {
                    return HitTarget::PaletteAddToggle;
                }
                if !self.ui.palette.adding {
                    let count = self.snippets.filter(&self.ui.palette.query).len().min(7);
                    for i in 0..count {
                        if self.layout.palette_delete_rect(i).contains(px, py) {
                            return HitTarget::PaletteDelete(i);
                        }
                        if self.layout.palette_row_rect(i).contains(px, py) {
                            return HitTarget::PaletteRow(i);
                        }
                    }
                }
                return HitTarget::None;
            }
        }

        if self.ui.search.open && !self.ui.settings.open && !self.ui.palette.open {
            if self.layout.search_close_rect().contains(px, py) {
                return HitTarget::SearchClose;
            }
            if self.layout.search_bar_rect().contains(px, py) {
                return HitTarget::None;
            }
        }

        // Check Warp command block [Copy] / [Re-run] buttons on the focused pane
        let active_vp = self.active_pane_rect();
        if !self.ui.settings.open
            && !self.ui.search.open
            && !self.ui.palette.open
            && active_vp.contains(px, py)
        {
            if let Some(sess) = self.sessions.active() {
                if !sess.grid.is_alt_screen() {
                    let ch = self.font.cell_h;
                    for row_idx in 0..sess.grid.rows() {
                        let abs_line = sess
                            .grid
                            .screen_row_to_abs_line(row_idx, sess.scroll_offset);
                        if let Some((b_idx, _)) = sess
                            .blocks
                            .iter()
                            .enumerate()
                            .find(|(_, b)| b.start_abs_line == abs_line)
                        {
                            let (copy_r, rerun_r) =
                                self.layout.block_action_rects_in(active_vp, row_idx, ch);
                            if copy_r.contains(px, py) {
                                return HitTarget::BlockCopy(b_idx);
                            }
                            if rerun_r.contains(px, py) {
                                return HitTarget::BlockRerun(b_idx);
                            }
                        }
                    }
                }
            }
        }

        if !self.ui.settings.open
            && !self.ui.picker_open
            && !self.ui.palette.open
            && !self.ui.history_modal.open
            && !self.ui.ports.open
            && self.ui.update_status.shows_header_pill()
            && self.layout.update_pill_rect().contains(px, py)
        {
            return HitTarget::UpdateAction;
        }

        if !self.ui.settings.open && !self.ui.picker_open && self.layout.sidebar.w > 0 {
            if let Some(card) = self.layout.usage_card_rect(self.enabled_plugin_count()) {
                if card.contains(px, py) {
                    return HitTarget::UsageCardRefresh;
                }
            }
        }

        self.layout.hit_test(
            px,
            py,
            self.sessions.sessions().len(),
            self.shells.len(),
            self.ui.picker_open,
            suggestion_count,
            self.ui.settings.open,
        )
    }

    fn update_hovered_link(&mut self) -> bool {
        let mut next_span = None;
        let mut next_label = None;
        let mut next_target = None;

        let active_vp = self.active_pane_rect();
        if !self.ui.settings.open && !self.ui.palette.open && !self.ui.selecting {
            if let Some((row, col)) = Layout::pixel_to_cell_in(
                active_vp,
                self.cursor_pos.0,
                self.cursor_pos.1,
                self.font.cell_w,
                self.font.cell_h,
            ) {
                if let Some(sess) = self.sessions.active() {
                    if let Some(cells) = sess.grid.visible_row(row, sess.scroll_offset) {
                        if let Some(det) = detect_link_at_col(cells, col, &sess.cwd) {
                            next_span = Some((row, det.col_start, det.col_end));
                            next_label = Some(match &det.target {
                                LinkTarget::Url(u) => format!("Click to open URL: {u}"),
                                LinkTarget::Path { resolved, .. } => {
                                    format!("Click to open in Explorer: {}", resolved.display())
                                }
                            });
                            next_target = Some(det.target);
                        }
                    }
                }
            }
        }

        let changed = self.ui.hovered_path != next_span;
        self.ui.hovered_path = next_span;
        self.ui.hovered_path_label = next_label;
        self.hovered_link_target = next_target;
        changed
    }

    fn refresh_search(&mut self) {
        if let Some(sess) = self.sessions.active_mut() {
            self.ui.search.matches = sess.grid.search_matches(&self.ui.search.query);
            if self.ui.search.matches.is_empty() {
                self.ui.search.active_idx = 0;
            } else {
                self.ui.search.active_idx = self
                    .ui
                    .search
                    .active_idx
                    .min(self.ui.search.matches.len() - 1);
                let abs = self.ui.search.matches[self.ui.search.active_idx].abs_line;
                sess.scroll_offset = sess.grid.scroll_offset_for_abs_line(abs);
            }
        }
    }

    fn step_search(&mut self, delta: isize) {
        let len = self.ui.search.matches.len();
        if len == 0 {
            return;
        }
        let next = (self.ui.search.active_idx as isize + delta).rem_euclid(len as isize) as usize;
        self.ui.search.active_idx = next;
        if let Some(sess) = self.sessions.active_mut() {
            let abs = self.ui.search.matches[next].abs_line;
            sess.scroll_offset = sess.grid.scroll_offset_for_abs_line(abs);
        }
    }

    fn apply_config_live(&mut self, reload_font: bool) {
        self.user = self.base_user.with_accent_override(self.config.accent);
        self.theme = Theme::for_user_with_config(&self.user, &self.config);
        self.sessions
            .update_colors(self.theme.fg_primary, self.theme.bg_terminal);

        if let Some(ref w) = self.window {
            if let Ok(icon) = Icon::from_rgba(build_app_icon_rgba(self.user.accent), 32, 32) {
                w.set_window_icon(Some(icon));
            }
        }

        if reload_font {
            if let Ok(mut new_font) = FontRenderer::load_with_config(
                self.config.font_name.as_deref(),
                self.config.font_size,
                &self.available_fonts,
            ) {
                new_font.set_scale_factor(self.scale_factor as f32);
                self.font = new_font;
                self.resize_sessions_for_current_layout();
            }
        }
        let _ = self.config.save();
    }

    fn cycle_font(&mut self, delta: isize) {
        if self.available_fonts.is_empty() {
            return;
        }
        let cur_idx = self
            .available_fonts
            .iter()
            .position(|f| f.name.eq_ignore_ascii_case(&self.font.font_name))
            .unwrap_or(0) as isize;
        let len = self.available_fonts.len() as isize;
        let next_idx = (cur_idx + delta).rem_euclid(len) as usize;
        self.config.font_name = Some(self.available_fonts[next_idx].name.clone());
        self.apply_config_live(true);
    }

    fn adjust_font_size(&mut self, delta: f32) {
        let next = (self.config.font_size + delta).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        if (next - self.config.font_size).abs() > f32::EPSILON {
            self.config.font_size = next;
            self.font.set_term_size(next);
            self.resize_sessions_for_current_layout();
            let _ = self.config.save();
            self.ui.toast = Some(format!("Font size: {:.0} px", next));
        }
    }

    fn reset_font_size(&mut self) {
        if (self.config.font_size - DEFAULT_FONT_SIZE).abs() > f32::EPSILON {
            self.config.font_size = DEFAULT_FONT_SIZE;
            self.font.set_term_size(DEFAULT_FONT_SIZE);
            self.resize_sessions_for_current_layout();
            let _ = self.config.save();
        }
        self.ui.toast = Some(format!("Font size: {:.0} px (default)", DEFAULT_FONT_SIZE));
    }

    fn apply_hex_field_if_valid(&mut self, field: SettingsField) {
        let text = self.ui.settings.field_buf(field).to_string();
        let Some(color) = parse_hex_color(&text) else {
            return;
        };
        match field {
            SettingsField::AccentHex => self.config.accent = Some(color),
            SettingsField::BgHex => self.config.bg_terminal = color,
            SettingsField::CmdHex => self.config.syntax_command = color,
            SettingsField::FlagHex => self.config.syntax_flag = color,
        }
        self.apply_config_live(false);
    }

    fn current_suggestions(&self) -> (Vec<CompletionItem>, Option<String>) {
        let Some(sess) = self.sessions.active() else {
            return (Vec::new(), None);
        };
        if sess.effective_passthrough()
            || self.ui.settings.open
            || self.ui.search.open
            || self.ui.palette.open
            || self.ui.history_modal.open
            || self.ui.ports.open
        {
            return (Vec::new(), None);
        }
        let text = sess.input.text();
        if text.is_empty() {
            return (Vec::new(), None);
        }

        let path_mode = is_completing_path(text);
        let fs_or_cmd = complete_input(text, &sess.cwd, &self.cmd_index, MAX_VISIBLE_SUGGESTIONS);
        let hist_items: Vec<CompletionItem> = self
            .history
            .suggest(text, &sess.shell.badge, MAX_VISIBLE_SUGGESTIONS)
            .into_iter()
            .map(|s| CompletionItem {
                display: s.command.clone(),
                replacement: s.command,
                meta: format!("{} {}x", s.shell_badge, s.count),
                kind: CompletionKind::History,
            })
            .collect();

        let mut merged = Vec::with_capacity(MAX_VISIBLE_SUGGESTIONS);
        if path_mode {
            for item in fs_or_cmd {
                if merged.len() < MAX_VISIBLE_SUGGESTIONS {
                    merged.push(item);
                }
            }
            for item in hist_items {
                if merged.len() < MAX_VISIBLE_SUGGESTIONS
                    && !merged.iter().any(|m| m.replacement == item.replacement)
                {
                    merged.push(item);
                }
            }
        } else {
            for item in hist_items {
                if merged.len() < MAX_VISIBLE_SUGGESTIONS {
                    merged.push(item);
                }
            }
            for item in fs_or_cmd {
                if merged.len() < MAX_VISIBLE_SUGGESTIONS
                    && !merged.iter().any(|m| m.replacement == item.replacement)
                {
                    merged.push(item);
                }
            }
        }

        let ghost = self.history.ghost_suffix(text, &sess.shell.badge).or_else(|| {
            let top = merged.first()?;
            if top
                .replacement
                .to_lowercase()
                .starts_with(&text.to_lowercase())
                && top.replacement.len() > text.len()
            {
                let suffix: String = top.replacement.chars().skip(text.chars().count()).collect();
                if suffix.is_empty() { None } else { Some(suffix) }
            } else {
                None
            }
        });

        (merged, ghost)
    }

    fn request_redraw(&self) {
        if let Some(ref w) = self.window {
            w.request_redraw();
        }
    }

    fn logical_size_from_physical(phys_w: u32, phys_h: u32, scale: f64) -> (u32, u32) {
        let s = scale.max(1.0);
        let lw = ((phys_w as f64) / s).round().max(1.0) as u32;
        let lh = ((phys_h as f64) / s).round().max(1.0) as u32;
        (lw, lh)
    }

    fn handle_resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.phys_width = width;
        self.phys_height = height;
        let (lw, lh) = Self::logical_size_from_physical(width, height, self.scale_factor);
        self.layout = Layout::compute_with_sidebar(lw, lh, self.ui.sidebar_visible);
        if let Some(ref mut surface) = self.surface {
            if let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) {
                let _ = surface.resize(w, h);
            }
        }
        self.resize_sessions_for_current_layout();
        self.ui.clear_selection();
        self.request_redraw();
    }

    fn handle_scale_factor_changed(&mut self, scale_factor: f64) {
        self.scale_factor = scale_factor.max(1.0);
        self.font.set_scale_factor(self.scale_factor as f32);
        if self.phys_width > 0 && self.phys_height > 0 {
            let (lw, lh) =
                Self::logical_size_from_physical(self.phys_width, self.phys_height, self.scale_factor);
            self.layout = Layout::compute_with_sidebar(lw, lh, self.ui.sidebar_visible);
            self.resize_sessions_for_current_layout();
        }
        self.ui.clear_selection();
        self.request_redraw();
    }

    fn apply_palette_selection(&mut self, row_idx: usize, run_immediately: bool) {
        let cmd_opt = self
            .snippets
            .filter(&self.ui.palette.query)
            .get(row_idx)
            .map(|(_, s)| s.command.clone());
        let Some(cmd) = cmd_opt else {
            return;
        };
        self.ui.palette.open = false;
        self.ui.palette.query.clear();
        self.ui.palette.selected = 0;
        if let Some(sess) = self.sessions.active_mut() {
            if run_immediately {
                let badge = sess.shell.badge.clone();
                sess.submit_command(&cmd, self.user.accent);
                self.history.record(&cmd, &badge);
            } else {
                sess.input.set_text(cmd);
            }
        }
    }

    fn apply_history_selection(&mut self, row_idx: usize, run_immediately: bool) {
        let cmd_opt = self
            .history
            .search_fuzzy(&self.ui.history_modal.query, 7)
            .get(row_idx)
            .map(|s| s.command.clone());
        let Some(cmd) = cmd_opt else {
            return;
        };
        self.ui.history_modal.open = false;
        self.ui.history_modal.query.clear();
        self.ui.history_modal.selected = 0;
        if let Some(sess) = self.sessions.active_mut() {
            if run_immediately {
                let badge = sess.shell.badge.clone();
                sess.submit_command(&cmd, self.user.accent);
                self.history.record(&cmd, &badge);
            } else {
                sess.input.set_text(cmd);
            }
        }
    }

    fn toggle_sidebar(&mut self) {
        self.ui.sidebar_visible = !self.ui.sidebar_visible;
        if !self.ui.sidebar_visible {
            self.ui.picker_open = false;
            self.ui.renaming_session = None;
        }
        self.layout = Layout::compute_with_sidebar(
            self.layout.width,
            self.layout.height,
            self.ui.sidebar_visible,
        );
        self.resize_sessions_for_current_layout();
    }

    fn handle_mouse_down(&mut self) {
        self.ui.toast = None;
        let (px, py) = self.cursor_pos;
        let (suggestions, _) = self.current_suggestions();
        let hit = self.hit_test_all(px, py, suggestions.len());
        match hit {
            HitTarget::HistoryClose => {
                self.ui.history_modal.open = false;
            }
            HitTarget::HistoryRow(i) => {
                self.apply_history_selection(i, false);
            }
            HitTarget::PortsToggle => {
                self.ui.picker_open = false;
                self.ui.settings.open = false;
                self.ui.palette.open = false;
                self.ui.search.open = false;
                self.ui.ports.open = !self.ui.ports.open;
                if self.ui.ports.open {
                    self.refresh_ports_async();
                }
            }
            HitTarget::PortsClose => {
                self.ui.ports.open = false;
            }
            HitTarget::PortsRefresh => {
                self.refresh_ports_async();
            }
            HitTarget::PortOpen(i) => {
                if let Some(lp) = self.ui.ports.ports.get(i) {
                    let url = format!("http://localhost:{}", lp.port);
                    open_url_in_browser(&url);
                    self.ui.toast = Some(format!("Opened {url}"));
                }
            }
            HitTarget::PortKill(i) => {
                if let Some(lp) = self.ui.ports.ports.get(i).cloned() {
                    if kill_process(lp.pid) {
                        self.ui.toast = Some(format!("Killed {} (PID {})", lp.process, lp.pid));
                    } else {
                        self.ui.toast = Some(format!("Failed to kill PID {}", lp.pid));
                    }
                    self.refresh_ports_async();
                }
            }
            HitTarget::DuplicateSession => {
                self.ui.picker_open = false;
                let recent = self.history.recent_commands(200);
                if self.sessions.duplicate_active(&recent).is_ok() {
                    self.ui.toast = Some("Duplicated session in same folder!".to_string());
                    self.save_workspace_state();
                }
            }
            HitTarget::CycleSessionColor(idx) => {
                if self.sessions.cycle_session_color(idx) {
                    self.save_workspace_state();
                }
            }
            HitTarget::SettingsThemePreset(idx) => {
                if let Some(preset) = THEME_PRESETS.get(idx) {
                    self.config.apply_preset(preset);
                    self.apply_config_live(false);
                    self.ui
                        .settings
                        .sync_from_config(&self.config, self.user.accent);
                    self.ui.toast = Some(format!("Applied theme: {}", preset.name));
                }
            }
            HitTarget::SplitToggle => {
                self.ui.picker_open = false;
                self.toggle_split();
            }
            HitTarget::SnippetsToggle => {
                self.ui.picker_open = false;
                self.ui.settings.open = false;
                self.ui.search.open = false;
                self.ui.palette.open = !self.ui.palette.open;
                self.ui.palette.adding = false;
                self.ui.palette.query.clear();
                self.ui.palette.selected = 0;
            }
            HitTarget::PaletteClose => {
                self.ui.palette.open = false;
                self.ui.palette.adding = false;
            }
            HitTarget::PaletteAddToggle => {
                self.ui.palette.adding = !self.ui.palette.adding;
                self.ui.palette.query.clear();
            }
            HitTarget::PaletteRow(i) => {
                self.apply_palette_selection(i, false);
            }
            HitTarget::PaletteDelete(i) => {
                let real_idx = self
                    .snippets
                    .filter(&self.ui.palette.query)
                    .get(i)
                    .map(|(idx, _)| *idx);
                if let Some(idx) = real_idx {
                    self.snippets.remove(idx);
                    self.ui.palette.selected = 0;
                }
            }
            HitTarget::SearchClose => {
                self.ui.search.open = false;
                self.ui.search.matches.clear();
            }
            HitTarget::BlockCopy(b_idx) => {
                if let Some(sess) = self.sessions.active() {
                    if let Some(text) = sess.block_output_text(b_idx) {
                        if copy_text(&text) {
                            self.ui.toast = Some("Copied block output to clipboard!".to_string());
                        }
                    }
                }
            }
            HitTarget::BlockRerun(b_idx) => {
                if let Some(sess) = self.sessions.active_mut() {
                    if let Some(cmd) = sess.blocks.get(b_idx).map(|b| b.command.clone()) {
                        let badge = sess.shell.badge.clone();
                        sess.submit_command(&cmd, self.user.accent);
                        self.history.record(&cmd, &badge);
                    }
                }
            }
            HitTarget::SettingsToggle => {
                self.ui.picker_open = false;
                self.ui.search.open = false;
                self.ui.palette.open = false;
                self.ui.settings.open = !self.ui.settings.open;
                self.ui.settings.focused_field = None;
                if self.ui.settings.open {
                    self.ui
                        .settings
                        .sync_from_config(&self.config, self.user.accent);
                }
            }
            HitTarget::SettingsClose => {
                self.ui.settings.open = false;
                self.ui.settings.focused_field = None;
            }
            HitTarget::SettingsSwatch(idx) => {
                if let Some((_, color)) = PRESET_ACCENTS.get(idx) {
                    self.config.accent = Some(*color);
                    self.apply_config_live(false);
                    self.ui
                        .settings
                        .sync_from_config(&self.config, self.user.accent);
                }
            }
            HitTarget::SettingsField(field) => {
                self.ui.settings.focused_field = Some(field);
            }
            HitTarget::SettingsFontPrev => self.cycle_font(-1),
            HitTarget::SettingsFontNext => self.cycle_font(1),
            HitTarget::SettingsSizeDown => self.adjust_font_size(-1.0),
            HitTarget::SettingsSizeUp => self.adjust_font_size(1.0),
            HitTarget::SettingsPluginAgy => {
                self.config.plugin_agy = !self.config.plugin_agy;
                let _ = self.config.save();
                self.refresh_plugins_async();
            }
            HitTarget::SettingsPluginClaude => {
                self.config.plugin_claude = !self.config.plugin_claude;
                let _ = self.config.save();
                self.refresh_plugins_async();
            }
            HitTarget::SettingsPluginCodex => {
                self.config.plugin_codex = !self.config.plugin_codex;
                let _ = self.config.save();
                self.refresh_plugins_async();
            }
            HitTarget::UsageCardRefresh => {
                self.refresh_plugins_async();
                self.ui.toast = Some("Refreshing 24h agent token usage...".to_string());
            }
            HitTarget::SettingsReset => {
                self.config.reset_to_defaults();
                self.apply_config_live(true);
                self.refresh_plugins_async();
                self.ui
                    .settings
                    .sync_from_config(&self.config, self.user.accent);
                self.ui.settings.focused_field = None;
            }
            HitTarget::SuggestionRow(idx) => {
                if let Some(sug) = suggestions.get(idx) {
                    if let Some(sess) = self.sessions.active_mut() {
                        sess.input.set_text(sug.replacement.clone());
                    }
                    self.ui.suggestion_selected = 0;
                }
            }
            HitTarget::NewSessionToggle => {
                self.ui.settings.open = false;
                self.ui.palette.open = false;
                self.ui.picker_open = !self.ui.picker_open;
            }
            HitTarget::PickShell(idx) => {
                self.ui.picker_open = false;
                if let Some(sh) = self.shells.get(idx).cloned() {
                    if self.sessions.spawn_session(sh).is_ok() {
                        if let Some(sess) = self.sessions.active_mut() {
                            sess.input.seed_history(self.history.recent_commands(200));
                        }
                    }
                }
            }
            HitTarget::SessionRow(idx) => {
                self.ui.picker_open = false;
                self.ui.suggestion_selected = 0;
                self.ui.clear_selection();
                let now = Instant::now();
                let is_double_click = matches!(
                    self.last_session_click,
                    Some((prev_idx, prev_t))
                        if prev_idx == idx && now.duration_since(prev_t) < Duration::from_millis(400)
                );
                self.last_session_click = Some((idx, now));
                self.sessions.select(idx);
                if is_double_click {
                    if let Some(title) = self.sessions.get(idx).map(|s| s.title.clone()) {
                        let mut edit = InputBar::new();
                        edit.set_text(title);
                        self.ui.renaming_session = Some((idx, edit));
                    }
                } else {
                    self.ui.renaming_session = None;
                }
                if self.ui.search.open {
                    self.refresh_search();
                }
            }
            HitTarget::CloseSession(idx) => {
                self.ui.picker_open = false;
                self.ui.suggestion_selected = 0;
                self.ui.clear_selection();
                let was_split = self.sessions.is_split();
                self.sessions.close(idx);
                if was_split && !self.sessions.is_split() {
                    self.resize_sessions_for_current_layout();
                }
            }
            HitTarget::UpdateAction | HitTarget::SettingsCheckUpdate => {
                self.handle_update_click();
            }
            HitTarget::TogglePassthrough => {
                self.ui.picker_open = false;
                if let Some(sess) = self.sessions.active_mut() {
                    sess.input.passthrough = !sess.input.passthrough;
                }
            }
            HitTarget::TerminalViewport => {
                self.ui.picker_open = false;
                self.ui.palette.open = false;
                // In split mode, clicking either pane focuses it first
                if let Some((left_idx, right_idx)) = self.sessions.split_panes() {
                    let (left_vp, right_vp) = self.layout.split_viewports();
                    if left_vp.contains(px, py) && self.sessions.active_index() != left_idx {
                        self.sessions.select(left_idx);
                        self.ui.clear_selection();
                        self.request_redraw();
                        return;
                    } else if right_vp.contains(px, py) && self.sessions.active_index() != right_idx
                    {
                        self.sessions.select(right_idx);
                        self.ui.clear_selection();
                        self.request_redraw();
                        return;
                    }
                }
                let active_vp = self.active_pane_rect();
                if let Some(cell) =
                    Layout::pixel_to_cell_in(active_vp, px, py, self.font.cell_w, self.font.cell_h)
                {
                    self.ui.selection_start = Some(cell);
                    self.ui.selection_end = Some(cell);
                    self.ui.selecting = true;
                }
            }
            _ => {
                self.ui.picker_open = false;
                self.ui.clear_selection();
                if self.ui.settings.open {
                    self.ui.settings.focused_field = None;
                }
            }
        }
        self.request_redraw();
    }

    fn handle_mouse_up(&mut self) {
        if !self.ui.selecting {
            return;
        }
        self.ui.selecting = false;
        if let (Some(a), Some(b)) = (self.ui.selection_start, self.ui.selection_end) {
            if a != b {
                // Auto-copy drag selection to clipboard so copying inside Claude Code / agy
                // never risks sending Ctrl+C (SIGINT) to the running agent.
                if let Some(sess) = self.sessions.active() {
                    let text = sess.grid.extract_selection(a, b, sess.scroll_offset);
                    if copy_text(&text) {
                        self.ui.toast = Some("Copied selection to clipboard!".to_string());
                    }
                }
            } else {
                self.ui.clear_selection();
                if let Some(ref target) = self.hovered_link_target {
                    open_link(target);
                }
            }
        }
        self.request_redraw();
    }

    fn primary_modifier(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.modifiers.super_key() || self.modifiers.control_key()
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.modifiers.control_key()
        }
    }

    fn allows_text_input(&self) -> bool {
        allows_text_input_for_modifiers(self.modifiers)
    }

    fn handle_history_modal_key(&mut self, event: KeyEvent) {
        let primary = self.primary_modifier();
        if primary {
            if let Key::Character(s) = &event.logical_key {
                if s.eq_ignore_ascii_case("v") {
                    if let Some(clip) = paste_text() {
                        self.ui
                            .history_modal
                            .query
                            .push_str(&clip.replace(['\r', '\n'], " "));
                        self.ui.history_modal.selected = 0;
                        self.request_redraw();
                    }
                    return;
                }
            }
        }

        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.ui.history_modal.open = false;
            }
            Key::Named(NamedKey::Enter) => {
                let run = !self.modifiers.shift_key();
                self.apply_history_selection(self.ui.history_modal.selected, run);
            }
            Key::Named(NamedKey::Tab) => {
                self.apply_history_selection(self.ui.history_modal.selected, false);
            }
            Key::Named(NamedKey::ArrowDown) => {
                let count = self.history.search_fuzzy(&self.ui.history_modal.query, 7).len();
                if count > 0 {
                    self.ui.history_modal.selected = (self.ui.history_modal.selected + 1) % count;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                let count = self.history.search_fuzzy(&self.ui.history_modal.query, 7).len();
                if count > 0 {
                    self.ui.history_modal.selected = if self.ui.history_modal.selected == 0 {
                        count - 1
                    } else {
                        self.ui.history_modal.selected - 1
                    };
                }
            }
            Key::Named(NamedKey::Backspace) => {
                self.ui.history_modal.query.pop();
                self.ui.history_modal.selected = 0;
            }
            Key::Named(NamedKey::Space) => {
                self.ui.history_modal.query.push(' ');
                self.ui.history_modal.selected = 0;
            }
            _ => {
                if self.allows_text_input() {
                    if let Some(text) = &event.text {
                        for ch in text.chars() {
                            if !ch.is_control() {
                                self.ui.history_modal.query.push(ch);
                                self.ui.history_modal.selected = 0;
                            }
                        }
                    }
                }
            }
        }
        self.request_redraw();
    }

    fn handle_palette_key(&mut self, event: KeyEvent) {
        let shift = self.modifiers.shift_key();
        let primary = self.primary_modifier();
        if primary {
            if let Key::Character(s) = &event.logical_key {
                if s.eq_ignore_ascii_case("v") {
                    if let Some(clip) = paste_text() {
                        self.ui.palette.query.push_str(&clip.replace(['\r', '\n'], " "));
                        self.ui.palette.selected = 0;
                        self.request_redraw();
                    }
                    return;
                }
            }
        }

        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                if self.ui.palette.adding {
                    self.ui.palette.adding = false;
                    self.ui.palette.query.clear();
                } else {
                    self.ui.palette.open = false;
                }
            }
            Key::Named(NamedKey::Enter) => {
                if self.ui.palette.adding {
                    if let Some((name, cmd)) = self.ui.palette.query.split_once('=') {
                        if self.snippets.upsert(name, cmd) {
                            self.ui.palette.adding = false;
                            self.ui.palette.query.clear();
                            self.ui.palette.selected = 0;
                            self.ui.toast = Some("Saved local snippet!".to_string());
                        }
                    }
                } else {
                    self.apply_palette_selection(self.ui.palette.selected, shift);
                }
            }
            Key::Named(NamedKey::ArrowDown) if !self.ui.palette.adding => {
                let count = self.snippets.filter(&self.ui.palette.query).len().min(7);
                if count > 0 {
                    self.ui.palette.selected = (self.ui.palette.selected + 1) % count;
                }
            }
            Key::Named(NamedKey::ArrowUp) if !self.ui.palette.adding => {
                let count = self.snippets.filter(&self.ui.palette.query).len().min(7);
                if count > 0 {
                    self.ui.palette.selected = if self.ui.palette.selected == 0 {
                        count - 1
                    } else {
                        self.ui.palette.selected - 1
                    };
                }
            }
            Key::Named(NamedKey::Backspace) => {
                self.ui.palette.query.pop();
                self.ui.palette.selected = 0;
            }
            Key::Named(NamedKey::Space) => {
                self.ui.palette.query.push(' ');
                self.ui.palette.selected = 0;
            }
            _ => {
                if self.allows_text_input() {
                    if let Some(text) = &event.text {
                        for ch in text.chars() {
                            if !ch.is_control() {
                                self.ui.palette.query.push(ch);
                                self.ui.palette.selected = 0;
                            }
                        }
                    }
                }
            }
        }
        self.request_redraw();
    }

    fn handle_settings_key(&mut self, event: KeyEvent) {
        let primary = self.primary_modifier();
        if primary {
            if let Key::Character(s) = &event.logical_key {
                if s.eq_ignore_ascii_case("v") {
                    if let (Some(field), Some(clip)) =
                        (self.ui.settings.focused_field, paste_text())
                    {
                        let cleaned: String = clip
                            .chars()
                            .filter(|c| *c == '#' || c.is_ascii_hexdigit())
                            .take(7)
                            .map(|c| c.to_ascii_uppercase())
                            .collect();
                        if !cleaned.is_empty() {
                            *self.ui.settings.field_buf_mut(field) = cleaned;
                            self.apply_hex_field_if_valid(field);
                            self.request_redraw();
                        }
                    }
                    return;
                }
            }
        }

        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.ui.settings.open = false;
                self.ui.settings.focused_field = None;
                self.request_redraw();
                return;
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(field) = self.ui.settings.focused_field {
                    self.apply_hex_field_if_valid(field);
                }
                self.ui.settings.focused_field = None;
                self.request_redraw();
                return;
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(field) = self.ui.settings.focused_field {
                    self.ui.settings.field_buf_mut(field).pop();
                    self.apply_hex_field_if_valid(field);
                    self.request_redraw();
                }
                return;
            }
            _ => {}
        }

        if let Some(field) = self.ui.settings.focused_field {
            if let Some(text) = &event.text {
                let mut changed = false;
                for ch in text.chars() {
                    if ch == '#' || ch.is_ascii_hexdigit() {
                        let buf = self.ui.settings.field_buf_mut(field);
                        if buf.len() < 7 {
                            buf.push(ch.to_ascii_uppercase());
                            changed = true;
                        }
                    }
                }
                if changed {
                    self.apply_hex_field_if_valid(field);
                    self.request_redraw();
                }
            }
        }
    }

    fn handle_search_key(&mut self, event: KeyEvent) {
        let shift = self.modifiers.shift_key();
        let primary = self.primary_modifier();

        if primary {
            if let Key::Character(s) = &event.logical_key {
                if s.eq_ignore_ascii_case("v") {
                    if let Some(clip) = paste_text() {
                        let clean = clip.replace(['\r', '\n'], " ");
                        self.ui.search.query.push_str(&clean);
                        self.ui.search.active_idx = 0;
                        self.refresh_search();
                        self.request_redraw();
                    }
                    return;
                }
            }
        }

        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.ui.search.open = false;
                self.ui.search.matches.clear();
            }
            Key::Named(NamedKey::Enter) => {
                self.step_search(if shift { -1 } else { 1 });
            }
            Key::Named(NamedKey::ArrowDown) => self.step_search(1),
            Key::Named(NamedKey::ArrowUp) => self.step_search(-1),
            Key::Named(NamedKey::Backspace) => {
                self.ui.search.query.pop();
                self.ui.search.active_idx = 0;
                self.refresh_search();
            }
            Key::Named(NamedKey::Space) => {
                self.ui.search.query.push(' ');
                self.ui.search.active_idx = 0;
                self.refresh_search();
            }
            _ => {
                if self.allows_text_input() {
                    if let Some(text) = &event.text {
                        let mut changed = false;
                        for ch in text.chars() {
                            if !ch.is_control() {
                                self.ui.search.query.push(ch);
                                changed = true;
                            }
                        }
                        if changed {
                            self.ui.search.active_idx = 0;
                            self.refresh_search();
                        }
                    }
                }
            }
        }
        self.request_redraw();
    }

    fn handle_key(&mut self, event: KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        self.ui.toast = None;

        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();
        let primary = self.primary_modifier();

        // Global overlay toggles: Ctrl+, (Settings), Ctrl+K (Snippets), Ctrl+F (Search), Ctrl+Shift+D (Split)
        if primary {
            if let Key::Character(s) = &event.logical_key {
                if s == "," {
                    self.ui.picker_open = false;
                    self.ui.search.open = false;
                    self.ui.palette.open = false;
                    self.ui.settings.open = !self.ui.settings.open;
                    self.ui.settings.focused_field = None;
                    if self.ui.settings.open {
                        self.ui
                            .settings
                            .sync_from_config(&self.config, self.user.accent);
                    }
                    self.request_redraw();
                    return;
                }
                if s.eq_ignore_ascii_case("b") && !shift && !self.ui.settings.open {
                    self.toggle_sidebar();
                    self.request_redraw();
                    return;
                }
                if s.eq_ignore_ascii_case("r") && !shift && !self.ui.settings.open {
                    self.ui.picker_open = false;
                    self.ui.palette.open = false;
                    self.ui.ports.open = false;
                    self.ui.search.open = false;
                    self.ui.history_modal.open = !self.ui.history_modal.open;
                    self.ui.history_modal.query.clear();
                    self.ui.history_modal.selected = 0;
                    self.request_redraw();
                    return;
                }
                if s.eq_ignore_ascii_case("k") && !self.ui.settings.open {
                    self.ui.picker_open = false;
                    self.ui.search.open = false;
                    self.ui.palette.open = !self.ui.palette.open;
                    self.ui.palette.adding = false;
                    self.ui.palette.query.clear();
                    self.ui.palette.selected = 0;
                    self.request_redraw();
                    return;
                }
                if s.eq_ignore_ascii_case("f") && !self.ui.settings.open && !self.ui.palette.open {
                    self.ui.picker_open = false;
                    self.ui.search.open = !self.ui.search.open;
                    if self.ui.search.open {
                        self.refresh_search();
                    } else {
                        self.ui.search.matches.clear();
                    }
                    self.request_redraw();
                    return;
                }
                if shift && s.eq_ignore_ascii_case("o") && !self.ui.settings.open {
                    self.ui.picker_open = false;
                    self.ui.palette.open = false;
                    self.ui.search.open = false;
                    self.ui.ports.open = !self.ui.ports.open;
                    if self.ui.ports.open {
                        self.refresh_ports_async();
                    }
                    self.request_redraw();
                    return;
                }
                if shift && s.eq_ignore_ascii_case("d") && !self.ui.settings.open {
                    self.toggle_split();
                    self.request_redraw();
                    return;
                }
            }
        }

        if self.ui.settings.open {
            self.handle_settings_key(event);
            return;
        }
        if self.ui.history_modal.open {
            self.handle_history_modal_key(event);
            return;
        }
        if self.ui.ports.open {
            if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
                self.ui.ports.open = false;
                self.request_redraw();
            }
            return;
        }
        if self.ui.palette.open {
            self.handle_palette_key(event);
            return;
        }
        if self.ui.search.open {
            self.handle_search_key(event);
            return;
        }
        if self.ui.picker_open {
            let count = self.shells.len();
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.ui.picker_open = false;
                }
                Key::Named(NamedKey::ArrowDown) if count > 0 => {
                    self.ui.picker_selected = (self.ui.picker_selected + 1) % count;
                }
                Key::Named(NamedKey::ArrowUp) if count > 0 => {
                    self.ui.picker_selected = if self.ui.picker_selected == 0 {
                        count - 1
                    } else {
                        self.ui.picker_selected - 1
                    };
                }
                Key::Named(NamedKey::Enter) if count > 0 => {
                    let idx = self.ui.picker_selected.min(count - 1);
                    self.ui.picker_open = false;
                    if let Some(sh) = self.shells.get(idx).cloned() {
                        if self.sessions.spawn_session(sh).is_ok() {
                            if let Some(sess) = self.sessions.active_mut() {
                                sess.input.seed_history(self.history.recent_commands(200));
                            }
                            self.save_workspace_state();
                        }
                    }
                }
                _ => {}
            }
            self.request_redraw();
            return;
        }

        // Inline session rename mode (double-click or F2) with full Shift+Home/End/Arrows & Ctrl+A
        if let Some((idx, mut draft)) = self.ui.renaming_session.take() {
            if primary {
                match &event.logical_key {
                    Key::Character(s) if s.eq_ignore_ascii_case("a") => {
                        draft.select_all();
                        self.ui.renaming_session = Some((idx, draft));
                        self.request_redraw();
                        return;
                    }
                    Key::Character(s) if s.eq_ignore_ascii_case("c") => {
                        if let Some(sel) = draft.selected_text() {
                            copy_text(sel);
                        }
                        self.ui.renaming_session = Some((idx, draft));
                        self.request_redraw();
                        return;
                    }
                    Key::Character(s) if s.eq_ignore_ascii_case("v") => {
                        if let Some(clip) = paste_text() {
                            let clean: String = clip
                                .replace(['\r', '\n'], " ")
                                .chars()
                                .take(32usize.saturating_sub(draft.text().chars().count()))
                                .collect();
                            draft.insert_str(&clean);
                        }
                        self.ui.renaming_session = Some((idx, draft));
                        self.request_redraw();
                        return;
                    }
                    _ => {}
                }
            }
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.request_redraw();
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    self.sessions.rename(idx, draft.text());
                    self.save_workspace_state();
                    self.request_redraw();
                    return;
                }
                Key::Named(NamedKey::Backspace) => {
                    draft.backspace();
                    self.ui.renaming_session = Some((idx, draft));
                    self.request_redraw();
                    return;
                }
                Key::Named(NamedKey::Delete) => {
                    draft.delete();
                    self.ui.renaming_session = Some((idx, draft));
                    self.request_redraw();
                    return;
                }
                Key::Named(NamedKey::ArrowLeft) => {
                    draft.move_left_with_select(shift);
                    self.ui.renaming_session = Some((idx, draft));
                    self.request_redraw();
                    return;
                }
                Key::Named(NamedKey::ArrowRight) => {
                    draft.move_right_with_select(shift);
                    self.ui.renaming_session = Some((idx, draft));
                    self.request_redraw();
                    return;
                }
                Key::Named(NamedKey::Home) => {
                    draft.move_home_with_select(shift);
                    self.ui.renaming_session = Some((idx, draft));
                    self.request_redraw();
                    return;
                }
                Key::Named(NamedKey::End) => {
                    draft.move_end_with_select(shift);
                    self.ui.renaming_session = Some((idx, draft));
                    self.request_redraw();
                    return;
                }
                Key::Named(NamedKey::Space) => {
                    if draft.text().chars().count() < 32 || draft.selected_range().is_some() {
                        draft.insert_char(' ');
                    }
                    self.ui.renaming_session = Some((idx, draft));
                    self.request_redraw();
                    return;
                }
                _ => {
                    if self.allows_text_input() {
                        if let Some(text) = &event.text {
                            for ch in text.chars() {
                                if !ch.is_control()
                                    && (draft.text().chars().count() < 32
                                        || draft.selected_range().is_some())
                                {
                                    draft.insert_char(ch);
                                }
                            }
                        }
                    }
                    self.ui.renaming_session = Some((idx, draft));
                    self.request_redraw();
                    return;
                }
            }
        }

        // F2 starts inline rename of active session
        if matches!(event.logical_key, Key::Named(NamedKey::F2)) {
            let idx = self.sessions.active_index();
            if let Some(title) = self.sessions.get(idx).map(|s| s.title.clone()) {
                let mut edit = InputBar::new();
                edit.set_text(title);
                self.ui.renaming_session = Some((idx, edit));
                self.request_redraw();
                return;
            }
        }

        // Global shortcuts
        if primary {
            match &event.logical_key {
                Key::Character(s) if s.eq_ignore_ascii_case("a") => {
                    if let Some(sess) = self.sessions.active_mut() {
                        if !sess.effective_passthrough() {
                            sess.input.select_all();
                            self.request_redraw();
                            return;
                        }
                    }
                }
                Key::Character(s) if s == "+" || s == "=" => {
                    self.adjust_font_size(1.0);
                    self.request_redraw();
                    return;
                }
                Key::Character(s) if s == "-" || s == "_" => {
                    self.adjust_font_size(-1.0);
                    self.request_redraw();
                    return;
                }
                Key::Character(s) if s == "0" => {
                    self.reset_font_size();
                    self.request_redraw();
                    return;
                }
                Key::Character(s) if s.eq_ignore_ascii_case("v") => {
                    if let Some(clip) = paste_text() {
                        if let Some(sess) = self.sessions.active_mut() {
                            if sess.effective_passthrough() {
                                let is_multiline = clip.contains('\n') || clip.contains('\r');
                                if is_multiline && sess.grid.bracketed_paste_enabled() {
                                    sess.write_bytes(b"\x1b[200~");
                                    sess.write_bytes(clip.as_bytes());
                                    sess.write_bytes(b"\x1b[201~");
                                } else {
                                    sess.write_bytes(clip.as_bytes());
                                }
                            } else {
                                let single_line = clip.replace("\r\n", " ").replace('\n', " ");
                                sess.input.insert_str(&single_line);
                                self.ui.suggestion_selected = 0;
                            }
                        }
                        self.request_redraw();
                    }
                    return;
                }
                Key::Character(s) if s.eq_ignore_ascii_case("c") => {
                    // 1. Copy Roby Input bar selection if present
                    if let Some(sess) = self.sessions.active() {
                        if !sess.effective_passthrough() {
                            if let Some(sel) = sess.input.selected_text() {
                                if copy_text(sel) {
                                    self.ui.toast = Some("Copied input selection!".to_string());
                                }
                                self.request_redraw();
                                return;
                            }
                        }
                    }
                    // 2. Copy terminal viewport drag selection if present
                    if let (Some(a), Some(b)) = (self.ui.selection_start, self.ui.selection_end) {
                        if a != b {
                            if let Some(sess) = self.sessions.active() {
                                let text = sess.grid.extract_selection(a, b, sess.scroll_offset);
                                if copy_text(&text) {
                                    self.ui.toast =
                                        Some("Copied selection to clipboard!".to_string());
                                }
                            }
                            self.ui.clear_selection();
                            self.request_redraw();
                            return;
                        }
                    }
                    if shift {
                        if let Some(sess) = self.sessions.active() {
                            if let Some(last_idx) = sess.blocks.len().checked_sub(1) {
                                if let Some(text) = sess.block_output_text(last_idx) {
                                    if copy_text(&text) {
                                        self.ui.toast =
                                            Some("Copied last block output!".to_string());
                                    }
                                }
                            }
                        }
                        self.request_redraw();
                        return;
                    }
                    if let Some(sess) = self.sessions.active_mut() {
                        sess.write_bytes(&[0x03]);
                    }
                    self.request_redraw();
                    return;
                }
                Key::Character(s) if shift && s.eq_ignore_ascii_case("t") => {
                    self.ui.picker_open = false;
                    let recent = self.history.recent_commands(200);
                    if self.sessions.duplicate_active(&recent).is_ok() {
                        self.ui.toast = Some("Duplicated session in same folder!".to_string());
                        self.save_workspace_state();
                    }
                    self.request_redraw();
                    return;
                }
                Key::Character(s) if s.eq_ignore_ascii_case("t") => {
                    self.ui.picker_open = !self.ui.picker_open;
                    self.ui.picker_selected = 0;
                    self.request_redraw();
                    return;
                }
                Key::Character(s) if s.eq_ignore_ascii_case("w") => {
                    let was_split = self.sessions.is_split();
                    self.sessions.close_active();
                    if was_split && !self.sessions.is_split() {
                        self.resize_sessions_for_current_layout();
                    }
                    self.ui.suggestion_selected = 0;
                    self.ui.clear_selection();
                    self.request_redraw();
                    return;
                }
                Key::Character(s) if shift && s.eq_ignore_ascii_case("p") => {
                    if let Some(sess) = self.sessions.active_mut() {
                        sess.input.passthrough = !sess.input.passthrough;
                    }
                    self.request_redraw();
                    return;
                }
                Key::Named(NamedKey::Tab) => {
                    self.sessions.select_next();
                    self.ui.suggestion_selected = 0;
                    self.ui.clear_selection();
                    self.request_redraw();
                    return;
                }
                Key::Character(s) if s.len() == 1 => {
                    if let Some(digit) = s.chars().next().and_then(|c| c.to_digit(10)) {
                        if (1..=9).contains(&digit) {
                            self.sessions.select((digit - 1) as usize);
                            self.ui.suggestion_selected = 0;
                            self.ui.clear_selection();
                            self.request_redraw();
                            return;
                        }
                    }
                }
                _ => {}
            }
        }

        if ctrl && !shift {
            if let Key::Character(s) = &event.logical_key {
                if s.eq_ignore_ascii_case("u") {
                    if let Some(sess) = self.sessions.active_mut() {
                        if !sess.effective_passthrough() {
                            sess.input.clear();
                            self.ui.suggestion_selected = 0;
                            self.request_redraw();
                            return;
                        }
                    }
                }
                if s.eq_ignore_ascii_case("n") {
                    let (suggestions, _) = self.current_suggestions();
                    if !suggestions.is_empty() {
                        self.ui.suggestion_selected =
                            (self.ui.suggestion_selected + 1) % suggestions.len();
                        self.ui.suggestion_navigated = true;
                        self.request_redraw();
                        return;
                    }
                }
                if s.eq_ignore_ascii_case("p") {
                    let (suggestions, _) = self.current_suggestions();
                    if !suggestions.is_empty() {
                        self.ui.suggestion_selected = if self.ui.suggestion_selected == 0 {
                            suggestions.len() - 1
                        } else {
                            self.ui.suggestion_selected - 1
                        };
                        self.ui.suggestion_navigated = true;
                        self.request_redraw();
                        return;
                    }
                }
            }
        }

        let (suggestions, ghost_suffix) = self.current_suggestions();

        let Some(sess) = self.sessions.active_mut() else {
            return;
        };

        if sess.effective_passthrough() {
            sess.scroll_offset = 0;
            self.ui.clear_selection();
            if ctrl {
                if let Key::Character(s) = &event.logical_key {
                    if let Some(ch) = s.chars().next() {
                        let lower = ch.to_ascii_lowercase();
                        if lower.is_ascii_lowercase() {
                            let ctrl_byte = (lower as u8) - b'a' + 1;
                            sess.write_bytes(&[ctrl_byte]);
                            return;
                        }
                    }
                }
            }
            let alt = self.modifiers.alt_key();
            match &event.logical_key {
                Key::Named(NamedKey::Enter) => {
                    if shift {
                        // Shift+Enter inserts a newline in Claude Code, agy, Codex & modern TUIs
                        if sess.grid.kitty_keyboard_enabled() {
                            sess.write_bytes(b"\x1b[13;2u");
                        } else if sess.grid.modify_other_keys_level() >= 1 {
                            sess.write_bytes(b"\x1b[27;2;13~");
                        } else {
                            sess.write_bytes(b"\x1b\r");
                        }
                    } else if alt {
                        if sess.grid.kitty_keyboard_enabled() {
                            sess.write_bytes(b"\x1b[13;3u");
                        } else {
                            sess.write_bytes(b"\x1b\r");
                        }
                    } else if ctrl {
                        if sess.grid.kitty_keyboard_enabled() {
                            sess.write_bytes(b"\x1b[13;5u");
                        } else if sess.grid.modify_other_keys_level() >= 1 {
                            sess.write_bytes(b"\x1b[27;5;13~");
                        } else {
                            sess.write_bytes(b"\n");
                        }
                    } else {
                        sess.write_bytes(b"\r");
                    }
                }
                Key::Named(NamedKey::Backspace) => sess.write_bytes(&[0x7f]),
                Key::Named(NamedKey::Tab) => {
                    if shift {
                        // BackTab / Shift+Tab (used by Claude Code, agy & TUIs to switch modes)
                        sess.write_bytes(b"\x1b[Z");
                    } else {
                        sess.write_bytes(b"\t");
                    }
                }
                Key::Named(NamedKey::Escape) => sess.write_bytes(&[0x1b]),
                Key::Named(NamedKey::ArrowUp) => sess.write_bytes(b"\x1b[A"),
                Key::Named(NamedKey::ArrowDown) => sess.write_bytes(b"\x1b[B"),
                Key::Named(NamedKey::ArrowRight) => sess.write_bytes(b"\x1b[C"),
                Key::Named(NamedKey::ArrowLeft) => sess.write_bytes(b"\x1b[D"),
                Key::Named(NamedKey::Home) => sess.write_bytes(b"\x1b[H"),
                Key::Named(NamedKey::End) => sess.write_bytes(b"\x1b[F"),
                Key::Named(NamedKey::PageUp) => sess.write_bytes(b"\x1b[5~"),
                Key::Named(NamedKey::PageDown) => sess.write_bytes(b"\x1b[6~"),
                Key::Named(NamedKey::Delete) => sess.write_bytes(b"\x1b[3~"),
                _ => {
                    if let Some(text) = &event.text {
                        sess.write_bytes(text.as_bytes());
                    }
                }
            }
            self.request_redraw();
            return;
        }

        match &event.logical_key {
            Key::Named(NamedKey::Enter) => {
                // If the user navigated the completion popup with Up/Down, Enter selects AND runs it directly!
                if self.ui.suggestion_navigated && !suggestions.is_empty() {
                    let sel_idx = self
                        .ui
                        .suggestion_selected
                        .min(suggestions.len().saturating_sub(1));
                    if let Some(sug) = suggestions.get(sel_idx) {
                        sess.input.set_text(sug.replacement.clone());
                    }
                }
                let cmd = sess.input.submit();
                let badge = sess.shell.badge.clone();
                sess.submit_command(&cmd, self.user.accent);
                self.history.record(&cmd, &badge);
                self.ui.suggestion_selected = 0;
                self.ui.suggestion_navigated = false;
                self.ui.clear_selection();
            }
            Key::Named(NamedKey::Tab) => {
                let path_items: Vec<CompletionItem> = suggestions
                    .iter()
                    .filter(|s| {
                        matches!(s.kind, CompletionKind::Directory | CompletionKind::File)
                    })
                    .cloned()
                    .collect();

                if path_items.len() > 1
                    && !self.ui.suggestion_navigated
                    && self.ui.suggestion_selected == 0
                {
                    if let Some(lcp) = longest_common_prefix(&path_items) {
                        if lcp.len() > sess.input.text().len() {
                            sess.input.set_text(lcp);
                            self.ui.suggestion_selected = 0;
                            self.ui.suggestion_navigated = false;
                            self.request_redraw();
                            return;
                        }
                    }
                }

                if !suggestions.is_empty() {
                    let sel_idx = self
                        .ui
                        .suggestion_selected
                        .min(suggestions.len().saturating_sub(1));
                    let target = &suggestions[sel_idx].replacement;
                    if target.eq_ignore_ascii_case(sess.input.text()) && suggestions.len() > 1 {
                        let next = (sel_idx + 1) % suggestions.len();
                        sess.input.set_text(suggestions[next].replacement.clone());
                        self.ui.suggestion_selected = next;
                        self.ui.suggestion_navigated = true;
                    } else {
                        sess.input.set_text(target.clone());
                        self.ui.suggestion_selected = 0;
                        self.ui.suggestion_navigated = false;
                    }
                } else if let Some(ghost) = ghost_suffix {
                    sess.input.insert_str(&ghost);
                    self.ui.suggestion_selected = 0;
                    self.ui.suggestion_navigated = false;
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                if !shift && sess.input.is_cursor_at_end() {
                    if let Some(ghost) = ghost_suffix {
                        sess.input.insert_str(&ghost);
                        self.ui.suggestion_selected = 0;
                        self.ui.suggestion_navigated = false;
                    }
                } else {
                    sess.input.move_right_with_select(shift);
                }
            }
            Key::Named(NamedKey::Backspace) => {
                sess.input.backspace();
                self.ui.suggestion_selected = 0;
                self.ui.suggestion_navigated = false;
            }
            Key::Named(NamedKey::Delete) => {
                sess.input.delete();
                self.ui.suggestion_selected = 0;
                self.ui.suggestion_navigated = false;
            }
            Key::Named(NamedKey::ArrowLeft) => sess.input.move_left_with_select(shift),
            Key::Named(NamedKey::Home) => sess.input.move_home_with_select(shift),
            Key::Named(NamedKey::End) => sess.input.move_end_with_select(shift),
            Key::Named(NamedKey::ArrowUp) => {
                if !suggestions.is_empty() {
                    self.ui.suggestion_selected = if self.ui.suggestion_selected == 0 {
                        suggestions.len() - 1
                    } else {
                        self.ui.suggestion_selected - 1
                    };
                    self.ui.suggestion_navigated = true;
                } else {
                    sess.input.history_prev();
                    self.ui.suggestion_selected = 0;
                    self.ui.suggestion_navigated = false;
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                if !suggestions.is_empty() {
                    self.ui.suggestion_selected =
                        (self.ui.suggestion_selected + 1) % suggestions.len();
                    self.ui.suggestion_navigated = true;
                } else {
                    sess.input.history_next();
                    self.ui.suggestion_selected = 0;
                    self.ui.suggestion_navigated = false;
                }
            }
            Key::Named(NamedKey::Escape) => {
                self.ui.picker_open = false;
                self.ui.suggestion_selected = 0;
                self.ui.suggestion_navigated = false;
                self.ui.clear_selection();
                sess.input.clear();
            }
            Key::Named(NamedKey::Space) => {
                sess.input.insert_char(' ');
                self.ui.suggestion_selected = 0;
                self.ui.suggestion_navigated = false;
            }
            _ => {
                if allows_text_input_for_modifiers(self.modifiers) {
                    if let Some(text) = &event.text {
                        for ch in text.chars() {
                            if !ch.is_control() {
                                sess.input.insert_char(ch);
                                self.ui.suggestion_selected = 0;
                                self.ui.suggestion_navigated = false;
                            }
                        }
                    }
                }
            }
        }
        self.request_redraw();
    }

    fn redraw(&mut self) {
        self.sessions.poll_all();
        self.poll_plugin_usage();
        self.poll_ports();
        self.poll_updates();
        let (suggestions, ghost_suffix) = self.current_suggestions();
        if !suggestions.is_empty() && self.ui.suggestion_selected >= suggestions.len() {
            self.ui.suggestion_selected = 0;
        }
        let Some(ref mut surface) = self.surface else {
            return;
        };
        if let Ok(mut buffer) = surface.buffer_mut() {
            render_app(
                &mut buffer,
                self.phys_width,
                self.phys_height,
                self.scale_factor as f32,
                &self.layout,
                &mut self.font,
                &self.theme,
                &self.user,
                &self.shells,
                &self.sessions,
                &self.ui,
                &suggestions,
                ghost_suffix.as_deref(),
                &self.config,
                &self.snippets,
                &self.history,
                &self.agent_usage,
            );
            let _ = buffer.present();
        }
    }
}

impl ApplicationHandler<()> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let mut attrs = Window::default_attributes()
            .with_title(format!("Roby-Term — {}@{}", self.user.username, self.user.hostname))
            .with_inner_size(LogicalSize::new(1120.0, 720.0))
            .with_min_inner_size(LogicalSize::new(640.0, 400.0));
        if let Ok(icon) = Icon::from_rgba(build_app_icon_rgba(self.user.accent), 32, 32) {
            attrs = attrs.with_window_icon(Some(icon));
        }

        let window = Rc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );
        let context = Context::new(window.clone()).expect("failed to create softbuffer context");
        let mut surface =
            Surface::new(&context, window.clone()).expect("failed to create softbuffer surface");

        self.scale_factor = window.scale_factor().max(1.0);
        self.font.set_scale_factor(self.scale_factor as f32);
        let size = window.inner_size();
        if let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) {
            self.phys_width = size.width;
            self.phys_height = size.height;
            let _ = surface.resize(w, h);
            let (lw, lh) =
                Self::logical_size_from_physical(size.width, size.height, self.scale_factor);
            self.layout = Layout::compute_with_sidebar(lw, lh, self.ui.sidebar_visible);
            self.resize_sessions_for_current_layout();
        }

        window.request_redraw();
        self.window = Some(window);
        self._context = Some(context);
        self.surface = Some(surface);
        let _ = self.proxy.send_event(());
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        let sess_changed = self.sessions.poll_all();
        let plugin_changed = self.poll_plugin_usage();
        let ports_changed = self.poll_ports();
        let updates_changed = self.poll_updates();
        if sess_changed || plugin_changed || ports_changed || updates_changed {
            if sess_changed && self.ui.search.open {
                self.refresh_search();
            }
            let in_sync = self
                .sessions
                .active()
                .is_some_and(|s| s.grid.is_sync_output());
            if !in_sync || plugin_changed || ports_changed || updates_changed {
                self.request_redraw();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.sessions.poll_all();
                self.save_workspace_state();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => self.handle_resize(size.width, size.height),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.handle_scale_factor_changed(scale_factor);
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                let s = self.scale_factor.max(1.0);
                self.cursor_pos = (
                    (position.x / s).max(0.0) as u32,
                    (position.y / s).max(0.0) as u32,
                );
                if self.ui.selecting {
                    let active_vp = self.active_pane_rect();
                    if let Some(cell) = Layout::pixel_to_cell_in(
                        active_vp,
                        self.cursor_pos.0,
                        self.cursor_pos.1,
                        self.font.cell_w,
                        self.font.cell_h,
                    ) {
                        if self.ui.selection_end != Some(cell) {
                            self.ui.selection_end = Some(cell);
                            self.request_redraw();
                        }
                    }
                    return;
                }
                let (suggestions, _) = self.current_suggestions();
                let hit = self.hit_test_all(self.cursor_pos.0, self.cursor_pos.1, suggestions.len());
                let link_changed = self.update_hovered_link();
                if hit != self.ui.hovered || link_changed {
                    self.ui.hovered = hit;
                    self.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.handle_mouse_down(),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.handle_mouse_up(),
            WindowEvent::MouseWheel { delta, .. } => {
                let s = self.scale_factor.max(1.0);
                let lines: i32 = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y * 3.0).round() as i32,
                    MouseScrollDelta::PixelDelta(pos) => ((pos.y / s) / 16.0).round() as i32,
                };
                if lines == 0 {
                    return;
                }
                self.ui.clear_selection();
                let active_vp = self.active_pane_rect();
                let (cell_row, cell_col) = Layout::pixel_to_cell_in(
                    active_vp,
                    self.cursor_pos.0,
                    self.cursor_pos.1,
                    self.font.cell_w,
                    self.font.cell_h,
                )
                .map(|(r, c)| (r + 1, c + 1))
                .unwrap_or((1, 1));

                if let Some(sess) = self.sessions.active_mut() {
                    if sess.grid.mouse_tracking_enabled() {
                        // Forward mouse wheel events to TUI/agents (Claude Code, agy, vim, lazygit)
                        let ticks = lines.unsigned_abs().clamp(1, 5);
                        let btn = if lines > 0 { 64 } else { 65 };
                        for _ in 0..ticks {
                            if sess.grid.sgr_mouse_enabled() {
                                let seq = format!("\x1b[<{btn};{cell_col};{cell_row}M");
                                sess.write_bytes(seq.as_bytes());
                            } else {
                                let cb = (btn as u8).saturating_add(32);
                                let cx = (cell_col.min(223) as u8).saturating_add(32);
                                let cy = (cell_row.min(223) as u8).saturating_add(32);
                                sess.write_bytes(&[0x1b, b'[', b'M', cb, cx, cy]);
                            }
                        }
                    } else if sess.grid.is_alt_screen() {
                        // Alternate scroll mode for alt-screen apps without mouse tracking (less, man)
                        let ticks = lines.unsigned_abs().clamp(1, 5);
                        let seq: &[u8] = if lines > 0 { b"\x1b[A" } else { b"\x1b[B" };
                        for _ in 0..ticks {
                            sess.write_bytes(seq);
                        }
                    } else if lines > 0 {
                        sess.scroll_up(lines as usize);
                    } else {
                        sess.scroll_down((-lines) as usize);
                    }
                    self.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => self.handle_key(event),
            WindowEvent::DroppedFile(path) => self.handle_dropped_file(path),
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }
}

/// Returns true when the active modifier combination permits inserting printable
/// `event.text` into text inputs.
///
/// On macOS, Option (`ALT`) is used to compose symbols like `@`, `#`, `~`, `|`, `\`, `[`, `]`, `{`, `}`.
/// On Windows/Linux, AltGr is reported as `CONTROL | ALT` and is also used for symbol composition.
fn allows_text_input_for_modifiers(modifiers: ModifiersState) -> bool {
    if modifiers.super_key() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        !modifiers.control_key()
    }
    #[cfg(not(target_os = "macos"))]
    {
        (!modifiers.control_key() && !modifiers.alt_key())
            || (modifiers.control_key() && modifiers.alt_key())
    }
}

fn main() {
    let event_loop = EventLoop::<()>::with_user_event()
        .build()
        .expect("failed to build event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();
    let mut app = App::new(proxy).expect("failed to initialize my-warp");
    event_loop.run_app(&mut app).expect("event loop error");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_plain_and_shift_text_input() {
        assert!(allows_text_input_for_modifiers(ModifiersState::empty()));
        assert!(allows_text_input_for_modifiers(ModifiersState::SHIFT));
    }

    #[test]
    fn blocks_super_and_plain_control_text_input() {
        assert!(!allows_text_input_for_modifiers(ModifiersState::SUPER));
        assert!(!allows_text_input_for_modifiers(
            ModifiersState::SUPER | ModifiersState::ALT
        ));
        assert!(!allows_text_input_for_modifiers(ModifiersState::CONTROL));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn allows_macos_option_symbol_composition() {
        assert!(allows_text_input_for_modifiers(ModifiersState::ALT));
        assert!(allows_text_input_for_modifiers(
            ModifiersState::ALT | ModifiersState::SHIFT
        ));
        assert!(!allows_text_input_for_modifiers(
            ModifiersState::CONTROL | ModifiersState::ALT
        ));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn allows_altgr_symbol_composition_on_non_macos() {
        assert!(allows_text_input_for_modifiers(
            ModifiersState::CONTROL | ModifiersState::ALT
        ));
        assert!(!allows_text_input_for_modifiers(ModifiersState::ALT));
    }
}

use crate::acp::{parse_markdown, AcpSessionState, Agent, ConnectionState, MdLineKind};
use crate::complete::{abbreviate_path, CompletionItem, CompletionKind};
use crate::config::{format_hex_color, parse_hex_color, UserConfig, PRESET_ACCENTS, THEME_PRESETS};
use crate::history::HistoryStore;
use crate::input::{InputBar, TokenKind};
use crate::plugins::{format_compact_tokens, AgentUsage};
use crate::ports::ListeningPort;
use crate::session::{format_duration_compact, Session, SessionManager};
use crate::shell::ShellProfile;
use crate::snippets::SnippetStore;
use crate::terminal::SearchMatch;
use crate::theme::{Rgb, Theme, UserIdentity};
use crate::updater::{UpdateStatus, CURRENT_VERSION};
use fontdue::{Font, FontSettings, Metrics};
use std::collections::HashMap;
use std::path::Path;

pub const SIDEBAR_WIDTH: u32 = 228;
pub const HEADER_HEIGHT: u32 = 38;
pub const INPUT_BAR_HEIGHT: u32 = 62;
pub const SESSION_ROW_HEIGHT: u32 = 36;
pub const PICKER_ROW_HEIGHT: u32 = 30;
pub const SUGGESTION_ROW_HEIGHT: u32 = 26;
pub const FONT_SIZE_UI: f32 = 13.5;
pub const FONT_SIZE_SMALL: f32 = 11.5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemFont {
    pub name: String,
    pub path: String,
}

/// Discovers installed monospace fonts on the current operating system.
pub fn discover_system_mono_fonts() -> Vec<SystemFont> {
    #[cfg(target_os = "windows")]
    let candidates: &[(&str, &str)] = &[
        ("Cascadia Mono", r"C:\Windows\Fonts\CascadiaMono.ttf"),
        ("Cascadia Code", r"C:\Windows\Fonts\CascadiaCode.ttf"),
        ("Consolas", r"C:\Windows\Fonts\consola.ttf"),
        ("Lucida Console", r"C:\Windows\Fonts\lucon.ttf"),
        ("Courier New", r"C:\Windows\Fonts\cour.ttf"),
    ];

    #[cfg(target_os = "macos")]
    let candidates: &[(&str, &str)] = &[
        ("SF Mono", "/System/Library/Fonts/SFNSMono.ttf"),
        ("Menlo", "/System/Library/Fonts/Menlo.ttc"),
        ("Monaco", "/System/Library/Fonts/Monaco.ttf"),
        ("Courier New", "/Library/Fonts/Courier New.ttf"),
    ];

    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let candidates: &[(&str, &str)] = &[
        ("DejaVu Sans Mono", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"),
        ("DejaVu Sans Mono", "/usr/share/fonts/TTF/DejaVuSansMono.ttf"),
        ("Liberation Mono", "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf"),
    ];

    candidates
        .iter()
        .filter(|(_, path)| Path::new(path).is_file())
        .map(|(name, path)| SystemFont {
            name: (*name).to_string(),
            path: (*path).to_string(),
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, px: u32, py: u32) -> bool {
        px >= self.x
            && py >= self.y
            && px < self.x.saturating_add(self.w)
            && py < self.y.saturating_add(self.h)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsField {
    AccentHex,
    BgHex,
    CmdHex,
    FlagHex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    SessionRow(usize),
    CycleSessionColor(usize),
    CloseSession(usize),
    SettingsToggle,
    SnippetsToggle,
    PortsToggle,
    SplitToggle,
    NewSessionToggle,
    DuplicateSession,
    PickShell(usize),
    SuggestionRow(usize),
    TogglePassthrough,
    SearchClose,
    PaletteClose,
    PaletteAddToggle,
    PaletteRow(usize),
    PaletteDelete(usize),
    HistoryClose,
    HistoryRow(usize),
    PortsClose,
    PortsRefresh,
    PortOpen(usize),
    PortKill(usize),
    BlockCopy(usize),
    BlockRerun(usize),
    SettingsClose,
    SettingsThemePreset(usize),
    SettingsSwatch(usize),
    SettingsField(SettingsField),
    SettingsFontPrev,
    SettingsFontNext,
    SettingsSizeDown,
    SettingsSizeUp,
    SettingsPluginAgy,
    SettingsPluginClaude,
    SettingsPluginCodex,
    SettingsReset,
    SettingsCheckUpdate,
    UpdateAction,
    UsageCardRefresh,
    AcpToggle,
    AcpClose,
    AcpAgentSelector,
    AcpAgentOption(usize),
    AcpPanel,
    AcpInput,
    AcpSend,
    AcpContextToggle,
    AcpAutoToggle,
    AcpPermissionApprove,
    AcpPermissionReject,
    ResizeSidebar,
    ResizeAcp,
    TerminalViewport,
    InputBar,
    None,
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    pub sidebar: Rect,
    pub user_card: Rect,
    pub sessions_top: u32,
    pub settings_btn: Rect,
    pub snippets_btn: Rect,
    pub ports_btn: Rect,
    pub split_btn: Rect,
    pub acp_btn: Rect,
    pub new_session_btn: Rect,
    pub dup_session_btn: Rect,
    pub header: Rect,
    pub passthrough_pill: Rect,
    pub viewport: Rect,
    pub input_bar: Rect,
    pub acp_panel: Option<Rect>,
}

impl Layout {
    pub fn compute(width: u32, height: u32) -> Self {
        Self::compute_with_sidebar(width, height, true)
    }

    pub fn compute_with_sidebar(width: u32, height: u32, sidebar_visible: bool) -> Self {
        Self::compute_with_options(width, height, sidebar_visible, false)
    }

    pub fn compute_with_options(
        width: u32,
        height: u32,
        sidebar_visible: bool,
        acp_open: bool,
    ) -> Self {
        Self::compute_with_sizes(width, height, sidebar_visible, SIDEBAR_WIDTH, acp_open, None)
    }

    pub fn compute_with_sizes(
        width: u32,
        height: u32,
        sidebar_visible: bool,
        sidebar_width: u32,
        acp_open: bool,
        acp_width: Option<u32>,
    ) -> Self {
        let sidebar_w = if sidebar_visible {
            let max_sw = (width / 2).max(120).min(width);
            let min_sw = 150.min(max_sw);
            sidebar_width.clamp(min_sw, max_sw)
        } else {
            0
        };
        let main_x = sidebar_w;
        let main_w = width.saturating_sub(sidebar_w);
        let header_h = HEADER_HEIGHT.min(height);
        let input_h = INPUT_BAR_HEIGHT.min(height.saturating_sub(header_h));
        let viewport_h = height.saturating_sub(header_h + input_h);

        let user_card = Rect::new(10, 10, sidebar_w.saturating_sub(20), 46);
        let sessions_top = user_card.y + user_card.h + 24;
        let new_btn_h = 34;
        let dup_w = 46;
        let new_y = height.saturating_sub(new_btn_h + 12);
        let new_session_btn = Rect::new(
            12,
            new_y,
            sidebar_w.saturating_sub(24 + dup_w + 6),
            new_btn_h,
        );
        let dup_session_btn = Rect::new(
            new_session_btn.x + new_session_btn.w + 6,
            new_y,
            dup_w,
            new_btn_h,
        );

        let btn_h = header_h.saturating_sub(14);
        let settings_w = 70;
        let settings_btn = Rect::new(
            main_x + main_w.saturating_sub(settings_w + 10),
            7,
            settings_w,
            btn_h,
        );
        let snippets_w = 72;
        let snippets_btn = Rect::new(
            settings_btn.x.saturating_sub(snippets_w + 6),
            7,
            snippets_w,
            btn_h,
        );
        let ports_w = 54;
        let ports_btn = Rect::new(
            snippets_btn.x.saturating_sub(ports_w + 6),
            7,
            ports_w,
            btn_h,
        );
        let split_w = 48;
        let split_btn = Rect::new(
            ports_btn.x.saturating_sub(split_w + 6),
            7,
            split_w,
            btn_h,
        );
        let acp_w_btn = 58;
        let acp_btn = Rect::new(
            split_btn.x.saturating_sub(acp_w_btn + 6),
            7,
            acp_w_btn,
            btn_h,
        );
        let pill_w = 104;
        let passthrough_pill = Rect::new(
            acp_btn.x.saturating_sub(pill_w + 6),
            7,
            pill_w,
            btn_h,
        );

        let (term_w, acp_panel) = if acp_open && main_w > 0 {
            let panel_w = if let Some(custom) = acp_width {
                let max_aw = main_w.saturating_sub(180).max(main_w / 2);
                let min_aw = 220.min(max_aw);
                custom.clamp(min_aw, max_aw)
            } else if main_w >= 460 {
                (main_w * 38 / 100)
                    .clamp(280, 440)
                    .min(main_w.saturating_sub(180))
            } else {
                main_w / 2
            };
            let tw = main_w.saturating_sub(panel_w + 1);
            (
                tw,
                Some(Rect::new(
                    main_x + tw + 1,
                    header_h,
                    panel_w,
                    viewport_h + input_h,
                )),
            )
        } else {
            (main_w, None)
        };

        Self {
            width,
            height,
            sidebar: Rect::new(0, 0, sidebar_w, height),
            user_card,
            sessions_top,
            settings_btn,
            snippets_btn,
            ports_btn,
            split_btn,
            acp_btn,
            new_session_btn,
            dup_session_btn,
            header: Rect::new(main_x, 0, main_w, header_h),
            passthrough_pill,
            viewport: Rect::new(main_x, header_h, term_w, viewport_h),
            input_bar: Rect::new(main_x, header_h + viewport_h, term_w, input_h),
            acp_panel,
        }
    }

    pub fn session_row_rect(&self, index: usize) -> Rect {
        let y = self.sessions_top + (index as u32) * (SESSION_ROW_HEIGHT + 4);
        Rect::new(10, y, self.sidebar.w.saturating_sub(20), SESSION_ROW_HEIGHT)
    }

    pub fn session_badge_rect(&self, index: usize) -> Rect {
        let row = self.session_row_rect(index);
        Rect::new(row.x + 9, row.y + 8, 36, 20)
    }

    pub fn session_close_rect(&self, index: usize) -> Rect {
        let row = self.session_row_rect(index);
        Rect::new(
            row.x + row.w.saturating_sub(24),
            row.y + 8,
            18,
            row.h.saturating_sub(16),
        )
    }

    pub fn shell_picker_panel(&self, shell_count: usize) -> Rect {
        let h = (shell_count as u32) * PICKER_ROW_HEIGHT + 12;
        let y = self.new_session_btn.y.saturating_sub(h + 8);
        Rect::new(12, y, self.sidebar.w.saturating_sub(24), h)
    }

    pub fn shell_picker_row(&self, shell_count: usize, idx: usize) -> Rect {
        let panel = self.shell_picker_panel(shell_count);
        Rect::new(
            panel.x + 6,
            panel.y + 6 + (idx as u32) * PICKER_ROW_HEIGHT,
            panel.w.saturating_sub(12),
            PICKER_ROW_HEIGHT.saturating_sub(2),
        )
    }

    pub fn suggestions_panel(&self, suggestion_count: usize) -> Rect {
        let h = (suggestion_count as u32) * SUGGESTION_ROW_HEIGHT + 10;
        let w = self.input_bar.w.saturating_sub(24).min(680);
        let x = self.input_bar.x + 12;
        let y = self.input_bar.y.saturating_sub(h + 4);
        Rect::new(x, y, w, h)
    }

    pub fn suggestion_row(&self, suggestion_count: usize, idx: usize) -> Rect {
        let panel = self.suggestions_panel(suggestion_count);
        Rect::new(
            panel.x + 5,
            panel.y + 5 + (idx as u32) * SUGGESTION_ROW_HEIGHT,
            panel.w.saturating_sub(10),
            SUGGESTION_ROW_HEIGHT.saturating_sub(2),
        )
    }

    pub fn settings_panel(&self) -> Rect {
        let w = self.viewport.w.saturating_sub(40).min(520);
        let h = self.height.saturating_sub(48).min(496);
        let x = self.viewport.x + self.viewport.w.saturating_sub(w) / 2;
        let y = self.header.h + self.viewport.h.saturating_sub(h) / 2;
        Rect::new(x, y, w, h)
    }

    pub fn settings_close_rect(&self) -> Rect {
        let p = self.settings_panel();
        Rect::new(p.x + p.w.saturating_sub(34), p.y + 10, 24, 24)
    }

    pub fn settings_theme_preset_rect(&self, idx: usize) -> Rect {
        let p = self.settings_panel();
        let w = 90;
        let gap = 7;
        let x = p.x + 20 + (idx as u32) * (w + gap);
        Rect::new(x, p.y + 62, w, 24)
    }

    pub fn settings_swatch_rect(&self, idx: usize) -> Rect {
        let p = self.settings_panel();
        let sw = 38;
        let gap = 8;
        let x = p.x + 20 + (idx as u32) * (sw + gap);
        Rect::new(x, p.y + 114, sw, 24)
    }

    pub fn settings_field_rect(&self, field: SettingsField) -> Rect {
        let p = self.settings_panel();
        let field_x = p.x + 168;
        let field_w = 132;
        let y = match field {
            SettingsField::AccentHex => p.y + 150,
            SettingsField::BgHex => p.y + 190,
            SettingsField::CmdHex => p.y + 226,
            SettingsField::FlagHex => p.y + 262,
        };
        Rect::new(field_x, y, field_w, 28)
    }

    pub fn settings_font_prev_rect(&self) -> Rect {
        let p = self.settings_panel();
        Rect::new(p.x + 168, p.y + 312, 28, 28)
    }

    pub fn settings_font_next_rect(&self) -> Rect {
        let p = self.settings_panel();
        Rect::new(p.x + p.w.saturating_sub(48), p.y + 312, 28, 28)
    }

    pub fn settings_size_down_rect(&self) -> Rect {
        let p = self.settings_panel();
        Rect::new(p.x + 168, p.y + 348, 30, 28)
    }

    pub fn settings_size_up_rect(&self) -> Rect {
        let p = self.settings_panel();
        Rect::new(p.x + 266, p.y + 348, 30, 28)
    }

    pub fn settings_reset_rect(&self) -> Rect {
        let p = self.settings_panel();
        Rect::new(p.x + 20, p.y + p.h.saturating_sub(36), 132, 26)
    }

    pub fn settings_update_rect(&self) -> Rect {
        let reset = self.settings_reset_rect();
        Rect::new(reset.x + reset.w + 10, reset.y, 146, reset.h)
    }

    /// Floating header pill positioned to the left of `passthrough_pill` when an update is ready.
    pub fn update_pill_rect(&self) -> Rect {
        let w = 118;
        Rect::new(
            self.passthrough_pill.x.saturating_sub(w + 6),
            self.passthrough_pill.y,
            w,
            self.passthrough_pill.h,
        )
    }

    /// Returns `(agy, claude, codex)` checkbox rects inside the Settings modal.
    pub fn settings_plugin_rects(&self) -> (Rect, Rect, Rect) {
        let p = self.settings_panel();
        let y = p.y + 412;
        let w = 148;
        let gap = 12;
        let r0 = Rect::new(p.x + 20, y, w, 28);
        let r1 = Rect::new(r0.x + w + gap, y, w, 28);
        let r2 = Rect::new(r1.x + w + gap, y, w, 28);
        (r0, r1, r2)
    }

    pub fn ports_panel(&self) -> Rect {
        let w = self.viewport.w.saturating_sub(40).min(560);
        let h = self.height.saturating_sub(80).min(360);
        let x = self.viewport.x + self.viewport.w.saturating_sub(w) / 2;
        let y = self.header.h + 28;
        Rect::new(x, y, w, h)
    }

    pub fn ports_close_rect(&self) -> Rect {
        let p = self.ports_panel();
        Rect::new(p.x + p.w.saturating_sub(32), p.y + 10, 22, 22)
    }

    pub fn ports_refresh_rect(&self) -> Rect {
        let p = self.ports_panel();
        Rect::new(p.x + p.w.saturating_sub(112), p.y + 10, 72, 22)
    }

    pub fn ports_row_rect(&self, idx: usize) -> Rect {
        let p = self.ports_panel();
        Rect::new(p.x + 14, p.y + 48 + (idx as u32) * 34, p.w.saturating_sub(28), 30)
    }

    pub fn ports_action_rects(&self, idx: usize) -> (Rect, Rect) {
        let r = self.ports_row_rect(idx);
        let kill_w = 46;
        let open_w = 50;
        let kill_r = Rect::new(r.x + r.w.saturating_sub(kill_w + 4), r.y + 4, kill_w, 22);
        let open_r = Rect::new(kill_r.x.saturating_sub(open_w + 6), r.y + 4, open_w, 22);
        (open_r, kill_r)
    }

    pub fn usage_card_rect(&self, enabled_count: usize) -> Option<Rect> {
        if enabled_count == 0 {
            return None;
        }
        let h = 24 + (enabled_count as u32) * 18;
        let y = self.new_session_btn.y.saturating_sub(h + 8);
        Some(Rect::new(12, y, self.sidebar.w.saturating_sub(24), h))
    }

    pub fn search_bar_rect(&self) -> Rect {
        let w = self.viewport.w.saturating_sub(24).min(340);
        let x = self.viewport.x + self.viewport.w.saturating_sub(w + 12);
        let y = self.viewport.y + 8;
        Rect::new(x, y, w, 32)
    }

    pub fn search_close_rect(&self) -> Rect {
        let r = self.search_bar_rect();
        Rect::new(r.x + r.w.saturating_sub(26), r.y + 5, 20, 22)
    }

    /// Returns `(copy_rect, rerun_rect)` for a Warp command block header on `screen_row` inside `vp`.
    pub fn block_action_rects_in(&self, vp: Rect, screen_row: usize, cell_h: u32) -> (Rect, Rect) {
        let y = vp.y + 6 + (screen_row as u32) * cell_h;
        let h = cell_h.clamp(16, 20);
        let rerun_w = 54;
        let copy_w = 46;
        let rerun_x = vp.x + vp.w.saturating_sub(rerun_w + 10);
        let copy_x = rerun_x.saturating_sub(copy_w + 6);
        (Rect::new(copy_x, y, copy_w, h), Rect::new(rerun_x, y, rerun_w, h))
    }

    pub fn split_viewports(&self) -> (Rect, Rect) {
        let half = self.viewport.w / 2;
        let left = Rect::new(
            self.viewport.x,
            self.viewport.y,
            half.saturating_sub(1),
            self.viewport.h,
        );
        let right = Rect::new(
            self.viewport.x + half + 1,
            self.viewport.y,
            self.viewport.w.saturating_sub(half + 1),
            self.viewport.h,
        );
        (left, right)
    }

    pub fn viewport_grid_size(vp: Rect, cell_w: u32, cell_h: u32) -> (usize, usize) {
        let pad_x = 16;
        let pad_y = 12;
        let usable_w = vp.w.saturating_sub(pad_x);
        let usable_h = vp.h.saturating_sub(pad_y);
        let cols = (usable_w / cell_w.max(1)).max(10) as usize;
        let rows = (usable_h / cell_h.max(1)).max(4) as usize;
        (cols, rows)
    }

    pub fn pixel_to_cell_in(vp: Rect, px: u32, py: u32, cell_w: u32, cell_h: u32) -> Option<(usize, usize)> {
        if !vp.contains(px, py) {
            return None;
        }
        let start_x = vp.x + 8;
        let start_y = vp.y + 6;
        if px < start_x || py < start_y {
            return None;
        }
        let col = ((px - start_x) / cell_w.max(1)) as usize;
        let row = ((py - start_y) / cell_h.max(1)) as usize;
        Some((row, col))
    }

    pub fn palette_panel(&self) -> Rect {
        let w = self.viewport.w.saturating_sub(40).min(560);
        let h = self.height.saturating_sub(90).min(340);
        let x = self.viewport.x + self.viewport.w.saturating_sub(w) / 2;
        let y = self.header.h + 28;
        Rect::new(x, y, w, h)
    }

    pub fn palette_close_rect(&self) -> Rect {
        let p = self.palette_panel();
        Rect::new(p.x + p.w.saturating_sub(32), p.y + 10, 22, 22)
    }

    pub fn palette_add_rect(&self) -> Rect {
        let p = self.palette_panel();
        Rect::new(p.x + p.w.saturating_sub(116), p.y + 10, 76, 22)
    }

    pub fn palette_row_rect(&self, idx: usize) -> Rect {
        let p = self.palette_panel();
        Rect::new(p.x + 14, p.y + 84 + (idx as u32) * 32, p.w.saturating_sub(28), 28)
    }

    pub fn palette_delete_rect(&self, idx: usize) -> Rect {
        let r = self.palette_row_rect(idx);
        Rect::new(r.x + r.w.saturating_sub(24), r.y + 4, 20, 20)
    }

    pub fn acp_header_rect(&self) -> Option<Rect> {
        let p = self.acp_panel?;
        Some(Rect::new(p.x, p.y, p.w, 36.min(p.h)))
    }

    pub fn acp_close_rect(&self) -> Option<Rect> {
        let h = self.acp_header_rect()?;
        Some(Rect::new(h.x + h.w.saturating_sub(28), h.y + 7, 20, 22))
    }

    pub fn acp_agent_selector_rect(&self) -> Option<Rect> {
        let h = self.acp_header_rect()?;
        Some(Rect::new(h.x + 10, h.y + 6, 118.min(h.w.saturating_sub(48)), 24))
    }

    pub fn acp_dropdown_panel(&self, count: usize) -> Option<Rect> {
        let sel = self.acp_agent_selector_rect()?;
        let h = (count as u32) * 26 + 8;
        Some(Rect::new(sel.x, sel.y + sel.h + 4, sel.w.max(132), h))
    }

    pub fn acp_agent_option_rect(&self, idx: usize) -> Option<Rect> {
        let panel = self.acp_dropdown_panel(Agent::ALL.len())?;
        Some(Rect::new(
            panel.x + 4,
            panel.y + 4 + (idx as u32) * 26,
            panel.w.saturating_sub(8),
            24,
        ))
    }

    pub fn acp_input_rect(&self) -> Option<Rect> {
        let p = self.acp_panel?;
        let send_w = 56u32;
        let bar_h = 34u32;
        let y = p.y + p.h.saturating_sub(bar_h + 8);
        let w = p.w.saturating_sub(20 + send_w + 6);
        Some(Rect::new(p.x + 10, y, w, bar_h))
    }

    pub fn acp_send_rect(&self) -> Option<Rect> {
        let inp = self.acp_input_rect()?;
        Some(Rect::new(inp.x + inp.w + 6, inp.y, 56, inp.h))
    }

    pub fn acp_context_bar_rect(&self) -> Option<Rect> {
        let p = self.acp_panel?;
        let inp = self.acp_input_rect()?;
        let h = 24u32;
        let auto_w = 82u32;
        let w = p.w.saturating_sub(20 + auto_w + 6);
        Some(Rect::new(p.x + 10, inp.y.saturating_sub(h + 6), w, h))
    }

    pub fn acp_auto_toggle_rect(&self) -> Option<Rect> {
        let ctx = self.acp_context_bar_rect()?;
        Some(Rect::new(ctx.x + ctx.w + 6, ctx.y, 82, ctx.h))
    }

    pub fn acp_permission_card_rect(&self) -> Option<Rect> {
        let p = self.acp_panel?;
        let ctx = self.acp_context_bar_rect()?;
        let h = 76u32;
        Some(Rect::new(
            p.x + 10,
            ctx.y.saturating_sub(h + 6),
            p.w.saturating_sub(20),
            h,
        ))
    }

    pub fn sidebar_divider_rect(&self) -> Option<Rect> {
        if self.sidebar.w == 0 {
            return None;
        }
        Some(Rect::new(self.sidebar.w.saturating_sub(3), 0, 7, self.height))
    }

    pub fn acp_divider_rect(&self) -> Option<Rect> {
        let p = self.acp_panel?;
        Some(Rect::new(p.x.saturating_sub(4), p.y, 7, p.h))
    }

    pub fn acp_permission_approve_rect(&self) -> Option<Rect> {
        let card = self.acp_permission_card_rect()?;
        Some(Rect::new(card.x + 8, card.y + card.h.saturating_sub(28), 82, 22))
    }

    pub fn acp_permission_reject_rect(&self) -> Option<Rect> {
        let approve = self.acp_permission_approve_rect()?;
        Some(Rect::new(approve.x + approve.w + 8, approve.y, 82, 22))
    }

    pub fn acp_messages_rect(&self) -> Option<Rect> {
        let p = self.acp_panel?;
        let h = self.acp_header_rect()?;
        let ctx = self.acp_context_bar_rect()?;
        let top = h.y + h.h + 4;
        let bottom = ctx.y.saturating_sub(6);
        Some(Rect::new(
            p.x + 8,
            top,
            p.w.saturating_sub(16),
            bottom.saturating_sub(top),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn hit_test(
        &self,
        px: u32,
        py: u32,
        session_count: usize,
        shell_count: usize,
        picker_open: bool,
        suggestion_count: usize,
        settings_open: bool,
    ) -> HitTarget {
        if settings_open {
            let panel = self.settings_panel();
            if panel.contains(px, py) {
                if self.settings_close_rect().contains(px, py) {
                    return HitTarget::SettingsClose;
                }
                for i in 0..THEME_PRESETS.len() {
                    if self.settings_theme_preset_rect(i).contains(px, py) {
                        return HitTarget::SettingsThemePreset(i);
                    }
                }
                for i in 0..PRESET_ACCENTS.len() {
                    if self.settings_swatch_rect(i).contains(px, py) {
                        return HitTarget::SettingsSwatch(i);
                    }
                }
                for field in [
                    SettingsField::AccentHex,
                    SettingsField::BgHex,
                    SettingsField::CmdHex,
                    SettingsField::FlagHex,
                ] {
                    if self.settings_field_rect(field).contains(px, py) {
                        return HitTarget::SettingsField(field);
                    }
                }
                if self.settings_font_prev_rect().contains(px, py) {
                    return HitTarget::SettingsFontPrev;
                }
                if self.settings_font_next_rect().contains(px, py) {
                    return HitTarget::SettingsFontNext;
                }
                if self.settings_size_down_rect().contains(px, py) {
                    return HitTarget::SettingsSizeDown;
                }
                if self.settings_size_up_rect().contains(px, py) {
                    return HitTarget::SettingsSizeUp;
                }
                let (p_agy, p_claude, p_codex) = self.settings_plugin_rects();
                if p_agy.contains(px, py) {
                    return HitTarget::SettingsPluginAgy;
                }
                if p_claude.contains(px, py) {
                    return HitTarget::SettingsPluginClaude;
                }
                if p_codex.contains(px, py) {
                    return HitTarget::SettingsPluginCodex;
                }
                if self.settings_reset_rect().contains(px, py) {
                    return HitTarget::SettingsReset;
                }
                if self.settings_update_rect().contains(px, py) {
                    return HitTarget::SettingsCheckUpdate;
                }
                return HitTarget::None;
            }
        }

        if suggestion_count > 0 && !settings_open {
            let panel = self.suggestions_panel(suggestion_count);
            if panel.contains(px, py) {
                for i in 0..suggestion_count {
                    if self.suggestion_row(suggestion_count, i).contains(px, py) {
                        return HitTarget::SuggestionRow(i);
                    }
                }
                return HitTarget::None;
            }
        }

        if picker_open {
            let panel = self.shell_picker_panel(shell_count);
            if panel.contains(px, py) {
                for i in 0..shell_count {
                    if self.shell_picker_row(shell_count, i).contains(px, py) {
                        return HitTarget::PickShell(i);
                    }
                }
                return HitTarget::None;
            }
        }

        if self.settings_btn.contains(px, py) {
            return HitTarget::SettingsToggle;
        }
        if self.snippets_btn.contains(px, py) {
            return HitTarget::SnippetsToggle;
        }
        if self.ports_btn.contains(px, py) {
            return HitTarget::PortsToggle;
        }
        if self.split_btn.contains(px, py) {
            return HitTarget::SplitToggle;
        }
        if self.acp_btn.contains(px, py) {
            return HitTarget::AcpToggle;
        }
        if self.sidebar.w > 0 {
            if self.new_session_btn.contains(px, py) {
                return HitTarget::NewSessionToggle;
            }
            if self.dup_session_btn.contains(px, py) {
                return HitTarget::DuplicateSession;
            }

            for i in 0..session_count {
                let row = self.session_row_rect(i);
                if row.y + row.h >= self.new_session_btn.y {
                    break;
                }
                if self.session_close_rect(i).contains(px, py) {
                    return HitTarget::CloseSession(i);
                }
                if self.session_badge_rect(i).contains(px, py) {
                    return HitTarget::CycleSessionColor(i);
                }
                if row.contains(px, py) {
                    return HitTarget::SessionRow(i);
                }
            }
        }

        if self.passthrough_pill.contains(px, py) {
            return HitTarget::TogglePassthrough;
        }
        if self.input_bar.contains(px, py) {
            return HitTarget::InputBar;
        }
        if self.viewport.contains(px, py) {
            return HitTarget::TerminalViewport;
        }
        HitTarget::None
    }

    pub fn terminal_grid_size(&self, cell_w: u32, cell_h: u32) -> (usize, usize) {
        let pad_x = 16;
        let pad_y = 12;
        let usable_w = self.viewport.w.saturating_sub(pad_x);
        let usable_h = self.viewport.h.saturating_sub(pad_y);
        let cols = (usable_w / cell_w.max(1)).max(10) as usize;
        let rows = (usable_h / cell_h.max(1)).max(4) as usize;
        (cols, rows)
    }

    /// Maps window pixel coordinates inside the terminal viewport to `(screen_row, col)`.
    pub fn pixel_to_grid_cell(&self, px: u32, py: u32, cell_w: u32, cell_h: u32) -> Option<(usize, usize)> {
        if !self.viewport.contains(px, py) {
            return None;
        }
        let start_x = self.viewport.x + 8;
        let start_y = self.viewport.y + 6;
        if px < start_x || py < start_y {
            return None;
        }
        let col = ((px - start_x) / cell_w.max(1)) as usize;
        let row = ((py - start_y) / cell_h.max(1)) as usize;
        Some((row, col))
    }
}

/// Minimal glyph cache rasterizing characters on demand with `fontdue`.
pub struct FontRenderer {
    font: Font,
    cache: HashMap<(char, u16), (Metrics, Vec<u8>)>,
    pub font_name: String,
    pub term_px: f32,
    pub scale: f32,
    pub cell_w: u32,
    pub cell_h: u32,
    pub baseline: i32,
}

impl FontRenderer {
    pub fn load_with_config(
        preferred_name: Option<&str>,
        term_px: f32,
        available: &[SystemFont],
    ) -> Result<Self, String> {
        let chosen = preferred_name
            .and_then(|name| {
                available
                    .iter()
                    .find(|f| f.name.eq_ignore_ascii_case(name))
            })
            .or_else(|| available.first())
            .ok_or_else(|| "No system monospace font found on this machine".to_string())?;

        let bytes = std::fs::read(&chosen.path)
            .map_err(|e| format!("failed to read font {}: {e}", chosen.path))?;
        let font = Font::from_bytes(bytes, FontSettings::default())
            .map_err(|e| format!("failed to parse system monospace font: {e}"))?;

        let mut renderer = Self {
            font,
            cache: HashMap::with_capacity(128),
            font_name: chosen.name.clone(),
            term_px,
            scale: 1.0,
            cell_w: 9,
            cell_h: 19,
            baseline: 14,
        };
        renderer.recompute_metrics();
        Ok(renderer)
    }

    pub fn set_term_size(&mut self, term_px: f32) {
        self.term_px = term_px;
        self.cache.clear();
        self.recompute_metrics();
    }

    pub fn set_scale_factor(&mut self, scale: f32) {
        let next = scale.max(1.0);
        if (self.scale - next).abs() > 0.001 {
            self.scale = next;
            self.cache.clear();
            self.recompute_metrics();
        }
    }

    fn recompute_metrics(&mut self) {
        let s = self.scale.max(1.0);
        let phys_px = self.term_px * s;
        let (m_metrics, _) = self.font.rasterize('M', phys_px);
        self.cell_w = ((m_metrics.advance_width / s).ceil() as u32).max(7);
        let (cell_h, baseline) = if let Some(lm) = self.font.horizontal_line_metrics(phys_px) {
            let h = ((lm.new_line_size / s).ceil() as u32).max(14);
            let base = (lm.ascent / s).ceil() as i32;
            (h, base)
        } else {
            ((self.term_px * 1.3) as u32, self.term_px as i32)
        };
        self.cell_h = cell_h;
        self.baseline = baseline;
    }

    fn glyph(&mut self, ch: char, px: f32) -> &(Metrics, Vec<u8>) {
        let key = (ch, (px * 10.0) as u16);
        self.cache
            .entry(key)
            .or_insert_with(|| self.font.rasterize(ch, px))
    }

    pub fn measure_text(&mut self, text: &str, px: f32) -> u32 {
        let s = self.scale.max(1.0);
        let phys_px = px * s;
        let mut w = 0.0f32;
        for ch in text.chars() {
            let (m, _) = self.glyph(ch, phys_px);
            w += m.advance_width;
        }
        (w / s).ceil() as u32
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_text(
        &mut self,
        canvas: &mut Canvas<'_>,
        x: i32,
        y: i32,
        px: f32,
        color: Rgb,
        text: &str,
        max_x: i32,
    ) -> i32 {
        let s = canvas.scale();
        let phys_px = px * s;
        let phys_max_x = ((max_x as f32) * s).round() as i32;
        let ascent = self
            .font
            .horizontal_line_metrics(phys_px)
            .map(|lm| lm.ascent.ceil() as i32)
            .unwrap_or((phys_px * 0.8) as i32);
        let baseline_y = ((y as f32) * s).round() as i32 + ascent;
        let mut pen_x = (x as f32) * s;

        for ch in text.chars() {
            if pen_x as i32 >= phys_max_x {
                break;
            }
            let (metrics, bitmap) = self.glyph(ch, phys_px);
            let gx = pen_x.round() as i32 + metrics.xmin;
            let gy = baseline_y - metrics.height as i32 - metrics.ymin;
            if !bitmap.is_empty() && ch != ' ' {
                canvas.blit_alpha(gx, gy, metrics.width, metrics.height, bitmap, color, phys_max_x);
            }
            pen_x += metrics.advance_width;
        }
        (pen_x / s).round() as i32
    }

    pub fn draw_cell_char(
        &mut self,
        canvas: &mut Canvas<'_>,
        cell_x: i32,
        cell_y: i32,
        ch: char,
        color: Rgb,
        max_x: i32,
    ) {
        if ch == ' ' || ch == '\0' {
            return;
        }
        let s = canvas.scale();
        let phys_px = self.term_px * s;
        let phys_max_x = ((max_x as f32) * s).round() as i32;
        let phys_x = ((cell_x as f32) * s).round() as i32;
        let phys_y = ((cell_y as f32) * s).round() as i32;
        let ascent = self
            .font
            .horizontal_line_metrics(phys_px)
            .map(|lm| lm.ascent.ceil() as i32)
            .unwrap_or(((self.baseline as f32) * s).round() as i32);
        let baseline_y = phys_y + ascent;
        let (metrics, bitmap) = self.glyph(ch, phys_px);
        if bitmap.is_empty() {
            return;
        }
        let gx = phys_x + metrics.xmin;
        let gy = baseline_y - metrics.height as i32 - metrics.ymin;
        canvas.blit_alpha(gx, gy, metrics.width, metrics.height, bitmap, color, phys_max_x);
    }
}

/// Zero-dependency 2D software framebuffer wrapper over `softbuffer`'s `&mut [u32]`.
/// Accepts logical UI coordinates and renders into the physical framebuffer at `scale`.
pub struct Canvas<'a> {
    pixels: &'a mut [u32],
    width: u32,
    height: u32,
    scale: f32,
}

impl<'a> Canvas<'a> {
    pub fn new(pixels: &'a mut [u32], width: u32, height: u32) -> Self {
        Self::new_scaled(pixels, width, height, 1.0)
    }

    pub fn new_scaled(pixels: &'a mut [u32], width: u32, height: u32, scale: f32) -> Self {
        Self {
            pixels,
            width,
            height,
            scale: scale.max(1.0),
        }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn clear(&mut self, color: Rgb) {
        self.pixels.fill(color.to_u32());
    }

    pub fn fill_rect(&mut self, r: Rect, color: Rgb) {
        if r.w == 0 || r.h == 0 || self.width == 0 || self.height == 0 {
            return;
        }
        let c = color.to_u32();
        let s = self.scale;
        let x0 = (((r.x as f32) * s).round().max(0.0) as u32).min(self.width);
        let y0 = (((r.y as f32) * s).round().max(0.0) as u32).min(self.height);
        let x1 = (((r.x.saturating_add(r.w) as f32) * s).round().max(0.0) as u32).min(self.width);
        let y1 = (((r.y.saturating_add(r.h) as f32) * s).round().max(0.0) as u32).min(self.height);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for y in y0..y1 {
            let row_start = (y as usize) * (self.width as usize);
            let start = row_start + x0 as usize;
            let end = row_start + x1 as usize;
            if end <= self.pixels.len() {
                self.pixels[start..end].fill(c);
            }
        }
    }

    pub fn stroke_rect(&mut self, r: Rect, color: Rgb) {
        if r.w == 0 || r.h == 0 {
            return;
        }
        self.fill_rect(Rect::new(r.x, r.y, r.w, 1), color);
        self.fill_rect(Rect::new(r.x, r.y + r.h.saturating_sub(1), r.w, 1), color);
        self.fill_rect(Rect::new(r.x, r.y, 1, r.h), color);
        self.fill_rect(Rect::new(r.x + r.w.saturating_sub(1), r.y, 1, r.h), color);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn blit_alpha(
        &mut self,
        x: i32,
        y: i32,
        w: usize,
        h: usize,
        alpha_map: &[u8],
        color: Rgb,
        clip_max_x: i32,
    ) {
        let max_x = clip_max_x.min(self.width as i32);
        let max_y = self.height as i32;
        for row in 0..h {
            let py = y + row as i32;
            if py < 0 || py >= max_y {
                continue;
            }
            let dst_row = (py as u32 * self.width) as usize;
            let src_row = row * w;
            for col in 0..w {
                let px = x + col as i32;
                if px < 0 || px >= max_x {
                    continue;
                }
                let Some(&a) = alpha_map.get(src_row + col) else {
                    continue;
                };
                if a == 0 {
                    continue;
                }
                let idx = dst_row + px as usize;
                if let Some(dst) = self.pixels.get_mut(idx) {
                    *dst = color.blend_over(*dst, a);
                }
            }
        }
    }
}

pub struct SettingsUiState {
    pub open: bool,
    pub focused_field: Option<SettingsField>,
    pub accent_buf: String,
    pub bg_buf: String,
    pub cmd_buf: String,
    pub flag_buf: String,
}

impl SettingsUiState {
    pub fn from_config(cfg: &UserConfig, active_accent: Rgb) -> Self {
        Self {
            open: false,
            focused_field: None,
            accent_buf: format_hex_color(cfg.accent.unwrap_or(active_accent)),
            bg_buf: format_hex_color(cfg.bg_terminal),
            cmd_buf: format_hex_color(cfg.syntax_command),
            flag_buf: format_hex_color(cfg.syntax_flag),
        }
    }

    pub fn sync_from_config(&mut self, cfg: &UserConfig, active_accent: Rgb) {
        self.accent_buf = format_hex_color(cfg.accent.unwrap_or(active_accent));
        self.bg_buf = format_hex_color(cfg.bg_terminal);
        self.cmd_buf = format_hex_color(cfg.syntax_command);
        self.flag_buf = format_hex_color(cfg.syntax_flag);
    }

    pub fn field_buf(&self, field: SettingsField) -> &str {
        match field {
            SettingsField::AccentHex => &self.accent_buf,
            SettingsField::BgHex => &self.bg_buf,
            SettingsField::CmdHex => &self.cmd_buf,
            SettingsField::FlagHex => &self.flag_buf,
        }
    }

    pub fn field_buf_mut(&mut self, field: SettingsField) -> &mut String {
        match field {
            SettingsField::AccentHex => &mut self.accent_buf,
            SettingsField::BgHex => &mut self.bg_buf,
            SettingsField::CmdHex => &mut self.cmd_buf,
            SettingsField::FlagHex => &mut self.flag_buf,
        }
    }
}

pub struct SearchUiState {
    pub open: bool,
    pub query: String,
    pub matches: Vec<SearchMatch>,
    pub active_idx: usize,
}

impl Default for SearchUiState {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            matches: Vec::new(),
            active_idx: 0,
        }
    }
}

pub struct PaletteUiState {
    pub open: bool,
    pub query: String,
    pub selected: usize,
    pub adding: bool,
}

impl Default for PaletteUiState {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            selected: 0,
            adding: false,
        }
    }
}

pub struct PortsUiState {
    pub open: bool,
    pub loading: bool,
    pub ports: Vec<ListeningPort>,
}

impl Default for PortsUiState {
    fn default() -> Self {
        Self {
            open: false,
            loading: false,
            ports: Vec::new(),
        }
    }
}

pub struct HistoryModalUiState {
    pub open: bool,
    pub query: String,
    pub selected: usize,
}

impl Default for HistoryModalUiState {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            selected: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeHandle {
    Sidebar,
    AcpPanel,
}

pub struct UiState {
    pub sidebar_visible: bool,
    pub picker_open: bool,
    pub picker_selected: usize,
    pub hovered: HitTarget,
    pub hovered_path: Option<(usize, usize, usize)>,
    pub hovered_path_label: Option<String>,
    /// Drag endpoints as `(absolute line, column)`; alternate-screen lines are screen-local.
    pub selection_start: Option<(usize, usize)>,
    pub selection_end: Option<(usize, usize)>,
    pub selecting: bool,
    pub toast: Option<String>,
    pub renaming_session: Option<(usize, InputBar)>,
    pub suggestion_selected: usize,
    pub suggestion_navigated: bool,
    pub search: SearchUiState,
    pub palette: PaletteUiState,
    pub history_modal: HistoryModalUiState,
    pub ports: PortsUiState,
    pub settings: SettingsUiState,
    pub update_status: UpdateStatus,
    pub acp_focused: bool,
    pub acp_dropdown_open: bool,
    pub sidebar_width: u32,
    pub acp_width: Option<u32>,
    pub resizing: Option<ResizeHandle>,
}

impl UiState {
    pub fn new(cfg: &UserConfig, active_accent: Rgb) -> Self {
        Self {
            sidebar_visible: true,
            picker_open: false,
            picker_selected: 0,
            hovered: HitTarget::None,
            hovered_path: None,
            hovered_path_label: None,
            selection_start: None,
            selection_end: None,
            selecting: false,
            toast: None,
            renaming_session: None,
            suggestion_selected: 0,
            suggestion_navigated: false,
            search: SearchUiState::default(),
            palette: PaletteUiState::default(),
            history_modal: HistoryModalUiState::default(),
            ports: PortsUiState::default(),
            settings: SettingsUiState::from_config(cfg, active_accent),
            update_status: UpdateStatus::Idle,
            acp_focused: false,
            acp_dropdown_open: false,
            sidebar_width: SIDEBAR_WIDTH,
            acp_width: None,
            resizing: None,
        }
    }

    pub fn has_non_empty_selection(&self) -> bool {
        matches!((self.selection_start, self.selection_end), (Some(a), Some(b)) if a != b)
    }

    pub fn clear_selection(&mut self) {
        self.selection_start = None;
        self.selection_end = None;
        self.selecting = false;
    }
}

/// Renders the entire terminal application into the 2D software framebuffer.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub fn render_app(
    pixels: &mut [u32],
    phys_width: u32,
    phys_height: u32,
    scale: f32,
    layout: &Layout,
    font: &mut FontRenderer,
    theme: &Theme,
    user: &UserIdentity,
    shells: &[ShellProfile],
    sessions: &SessionManager,
    ui: &UiState,
    suggestions: &[CompletionItem],
    ghost_suffix: Option<&str>,
    cfg: &UserConfig,
    snippets: &SnippetStore,
    history: &HistoryStore,
    usage: &AgentUsage,
) {
    let mut canvas = Canvas::new_scaled(pixels, phys_width, phys_height, scale);
    canvas.clear(theme.bg_terminal);

    // 1..7 Sidebar (skipped when collapsed via Ctrl+B)
    if layout.sidebar.w > 0 {
    // 1. Sidebar background + divider
    canvas.fill_rect(layout.sidebar, theme.bg_sidebar);
    let sb_div_active =
        ui.hovered == HitTarget::ResizeSidebar || ui.resizing == Some(ResizeHandle::Sidebar);
    canvas.fill_rect(
        Rect::new(
            layout.sidebar.w.saturating_sub(if sb_div_active { 2 } else { 1 }),
            0,
            if sb_div_active { 2 } else { 1 },
            layout.height,
        ),
        if sb_div_active { user.accent } else { theme.border },
    );

    // 2. User identity card (user-differentiated color)
    canvas.fill_rect(layout.user_card, user.badge_bg);
    canvas.stroke_rect(layout.user_card, user.accent.lerp(theme.border, 120));
    canvas.fill_rect(
        Rect::new(layout.user_card.x, layout.user_card.y, 4, layout.user_card.h),
        user.accent,
    );
    let user_max_x = (layout.user_card.x + layout.user_card.w - 8) as i32;
    font.draw_text(
        &mut canvas,
        (layout.user_card.x + 12) as i32,
        (layout.user_card.y + 6) as i32,
        FONT_SIZE_UI,
        user.accent,
        &user.username,
        user_max_x,
    );
    font.draw_text(
        &mut canvas,
        (layout.user_card.x + 12) as i32,
        (layout.user_card.y + 24) as i32,
        FONT_SIZE_SMALL,
        theme.fg_muted,
        &format!("@{}", user.hostname),
        user_max_x,
    );

    // 3. Sessions header label
    font.draw_text(
        &mut canvas,
        14,
        (layout.sessions_top.saturating_sub(18)) as i32,
        FONT_SIZE_SMALL,
        theme.fg_muted,
        "SESSIONS",
        (layout.sidebar.w - 10) as i32,
    );

    // 4. Session list rows
    for (idx, sess) in sessions.sessions().iter().enumerate() {
        let row = layout.session_row_rect(idx);
        if row.y + row.h >= layout.new_session_btn.y {
            break;
        }
        let is_active = idx == sessions.active_index();
        let is_hovered =
            matches!(ui.hovered, HitTarget::SessionRow(i) | HitTarget::CloseSession(i) if i == idx);
        let bg = if is_active {
            theme.bg_sidebar_active
        } else if is_hovered {
            theme.bg_sidebar_hover
        } else {
            theme.bg_sidebar
        };
        let sess_accent = sess.display_accent();
        canvas.fill_rect(row, bg);
        if is_active {
            canvas.fill_rect(Rect::new(row.x, row.y, 3, row.h), sess_accent);
        }

        // Shell badge pill (click to cycle per-session color)
        let badge_rect = layout.session_badge_rect(idx);
        canvas.fill_rect(badge_rect, sess_accent.lerp(theme.bg_sidebar, 190));
        if ui.hovered == HitTarget::CycleSessionColor(idx) {
            canvas.stroke_rect(badge_rect, sess_accent);
        }
        font.draw_text(
            &mut canvas,
            (badge_rect.x + 4) as i32,
            (badge_rect.y + 4) as i32,
            FONT_SIZE_SMALL,
            sess_accent,
            &sess.shell.badge,
            (badge_rect.x + badge_rect.w) as i32,
        );

        let is_renaming = matches!(&ui.renaming_session, Some((r_idx, _)) if *r_idx == idx);
        if let Some((_, ref draft)) = ui
            .renaming_session
            .as_ref()
            .filter(|(r_idx, _)| *r_idx == idx)
        {
            let edit_r = Rect::new(
                row.x + 50,
                row.y + 6,
                row.w.saturating_sub(56),
                row.h.saturating_sub(12),
            );
            canvas.fill_rect(edit_r, theme.bg_input);
            canvas.stroke_rect(edit_r, user.accent);
            let text_x = (edit_r.x + 5) as i32;
            let text_y = (edit_r.y + 4) as i32;
            let text_max_x = (edit_r.x + edit_r.w - 4) as i32;
            let txt = draft.text();

            // Selection highlight inside session rename box
            if let Some((c0, c1)) = draft.selected_char_range() {
                let pre: String = txt.chars().take(c0).collect();
                let sel: String = txt.chars().skip(c0).take(c1 - c0).collect();
                let sx = text_x + font.measure_text(&pre, FONT_SIZE_UI) as i32;
                let sw = font.measure_text(&sel, FONT_SIZE_UI).max(4);
                let clipped_w = sw.min((text_max_x - sx).max(0) as u32);
                if clipped_w > 0 && sx >= 0 {
                    canvas.fill_rect(
                        Rect::new(sx as u32, edit_r.y + 3, clipped_w, edit_r.h.saturating_sub(6)),
                        user.accent.lerp(theme.bg_input, 155),
                    );
                }
            }

            font.draw_text(
                &mut canvas,
                text_x,
                text_y,
                FONT_SIZE_UI,
                theme.fg_primary,
                txt,
                text_max_x,
            );

            let caret_pre: String = txt.chars().take(draft.cursor_char_index()).collect();
            let caret_x = (text_x + font.measure_text(&caret_pre, FONT_SIZE_UI) as i32)
                .min(text_max_x - 2);
            if caret_x >= 0 {
                canvas.fill_rect(
                    Rect::new(caret_x as u32, edit_r.y + 3, 2, edit_r.h.saturating_sub(6)),
                    user.accent,
                );
            }
        } else {
            let title_color = if !sess.alive {
                theme.fg_muted
            } else if is_active {
                theme.fg_primary
            } else {
                theme.fg_muted.lerp(theme.fg_primary, 140)
            };
            font.draw_text(
                &mut canvas,
                (row.x + 52) as i32,
                (row.y + 10) as i32,
                FONT_SIZE_UI,
                title_color,
                &sess.title,
                (row.x + row.w.saturating_sub(34)) as i32,
            );
        }

        // Background activity / exited status dot
        if !is_renaming {
            let dot_r = Rect::new(row.x + row.w.saturating_sub(32), row.y + 15, 6, 6);
            if !sess.alive {
                canvas.fill_rect(dot_r, theme.danger);
            } else if sess.has_unread_output && !is_active {
                canvas.fill_rect(dot_r, theme.syntax.string_literal);
            }
        }

        // Close button ('x')
        let close_r = layout.session_close_rect(idx);
        let close_color = if is_renaming {
            theme.bg_sidebar
        } else if ui.hovered == HitTarget::CloseSession(idx) {
            theme.danger
        } else if is_active || is_hovered {
            theme.fg_muted
        } else {
            theme.bg_sidebar
        };
        if close_color != theme.bg_sidebar {
            font.draw_text(
                &mut canvas,
                (close_r.x + 4) as i32,
                (close_r.y + 2) as i32,
                FONT_SIZE_UI,
                close_color,
                "x",
                (close_r.x + close_r.w) as i32,
            );
        }
    }

    // 4.5 Optional Agent Token Usage card in sidebar (shown only when enabled in Settings)
    let enabled_rows: Vec<(&str, Rgb, u64)> = [
        usage.agy.map(|t| ("agy", user.accent, t.total())),
        usage
            .claude
            .map(|t| ("claude", theme.syntax.flag, t.total())),
        usage
            .codex
            .map(|t| ("codex", theme.syntax.string_literal, t.total())),
    ]
    .into_iter()
    .flatten()
    .collect();

    if let Some(card) = layout.usage_card_rect(enabled_rows.len()) {
        let card_bg = if ui.hovered == HitTarget::UsageCardRefresh {
            theme.bg_sidebar_active
        } else {
            theme.bg_header
        };
        canvas.fill_rect(card, card_bg);
        canvas.stroke_rect(card, theme.border);
        font.draw_text(
            &mut canvas,
            (card.x + 8) as i32,
            (card.y + 4) as i32,
            FONT_SIZE_SMALL,
            theme.fg_muted,
            "TOKENS (24h)",
            (card.x + card.w - 6) as i32,
        );
        for (i, (label, color, total)) in enabled_rows.iter().enumerate() {
            let ry = card.y + 20 + (i as u32) * 18;
            canvas.fill_rect(Rect::new(card.x + 8, ry + 5, 5, 5), *color);
            font.draw_text(
                &mut canvas,
                (card.x + 18) as i32,
                ry as i32,
                FONT_SIZE_SMALL,
                theme.fg_primary,
                label,
                (card.x + card.w - 64) as i32,
            );
            let val = format_compact_tokens(*total);
            let vw = font.measure_text(&val, FONT_SIZE_SMALL);
            font.draw_text(
                &mut canvas,
                (card.x + card.w).saturating_sub(vw + 8) as i32,
                ry as i32,
                FONT_SIZE_SMALL,
                *color,
                &val,
                (card.x + card.w - 4) as i32,
            );
        }
    }

    // 5. "+ New" and "Dup" buttons at bottom of sidebar
    let new_btn_bg = if ui.picker_open || ui.hovered == HitTarget::NewSessionToggle {
        user.accent.lerp(theme.bg_sidebar, 175)
    } else {
        theme.bg_sidebar_hover
    };
    canvas.fill_rect(layout.new_session_btn, new_btn_bg);
    canvas.stroke_rect(layout.new_session_btn, user.accent.lerp(theme.border, 130));
    font.draw_text(
        &mut canvas,
        (layout.new_session_btn.x + 10) as i32,
        (layout.new_session_btn.y + 9) as i32,
        FONT_SIZE_UI,
        theme.fg_primary,
        if ui.picker_open { "v Select Shell" } else { "+ New Session" },
        (layout.new_session_btn.x + layout.new_session_btn.w - 4) as i32,
    );

    let dup_bg = if ui.hovered == HitTarget::DuplicateSession {
        user.accent.lerp(theme.bg_sidebar, 175)
    } else {
        theme.bg_sidebar_hover
    };
    canvas.fill_rect(layout.dup_session_btn, dup_bg);
    canvas.stroke_rect(layout.dup_session_btn, theme.border);
    font.draw_text(
        &mut canvas,
        (layout.dup_session_btn.x + 10) as i32,
        (layout.dup_session_btn.y + 9) as i32,
        FONT_SIZE_SMALL,
        theme.fg_primary,
        "Dup",
        (layout.dup_session_btn.x + layout.dup_session_btn.w - 4) as i32,
    );

    // 7. Shell picker popup (when open)
    if ui.picker_open {
        let panel = layout.shell_picker_panel(shells.len());
        canvas.fill_rect(panel, theme.bg_header);
        canvas.stroke_rect(panel, user.accent);
        for (i, sh) in shells.iter().enumerate() {
            let r = layout.shell_picker_row(shells.len(), i);
            if i == ui.picker_selected || ui.hovered == HitTarget::PickShell(i) {
                canvas.fill_rect(r, theme.bg_sidebar_active);
            }
            canvas.fill_rect(
                Rect::new(r.x + 4, r.y + 6, 4, r.h.saturating_sub(12)),
                sh.accent,
            );
            font.draw_text(
                &mut canvas,
                (r.x + 14) as i32,
                (r.y + 6) as i32,
                FONT_SIZE_UI,
                theme.fg_primary,
                &sh.name,
                (r.x + r.w - 6) as i32,
            );
        }
    }
    } // end if layout.sidebar.w > 0

    // 8. Top header bar
    canvas.fill_rect(layout.header, theme.bg_header);
    canvas.fill_rect(
        Rect::new(
            layout.header.x,
            layout.header.h.saturating_sub(1),
            layout.header.w,
            1,
        ),
        theme.border,
    );

    // Top-right header action buttons: [ACP] [Split] [Ports] [Snippets] [Settings]
    let acp_active = sessions.active().is_some_and(|s| s.acp.open);
    let acp_dot_color = sessions
        .active()
        .map(|s| acp_status_color(&s.acp, theme))
        .unwrap_or(theme.fg_muted);
    let acp_bg = if acp_active || ui.hovered == HitTarget::AcpToggle {
        theme.bg_sidebar_active
    } else {
        theme.bg_sidebar_hover
    };
    canvas.fill_rect(layout.acp_btn, acp_bg);
    canvas.stroke_rect(
        layout.acp_btn,
        if acp_active { user.accent } else { theme.border },
    );
    canvas.fill_rect(
        Rect::new(layout.acp_btn.x + 7, layout.acp_btn.y + 9, 6, 6),
        acp_dot_color,
    );
    font.draw_text(
        &mut canvas,
        (layout.acp_btn.x + 17) as i32,
        (layout.acp_btn.y + 5) as i32,
        FONT_SIZE_SMALL,
        if acp_active { user.accent } else { theme.fg_primary },
        "ACP",
        (layout.acp_btn.x + layout.acp_btn.w - 4) as i32,
    );

    let split_active = sessions.is_split();
    let split_bg = if split_active || ui.hovered == HitTarget::SplitToggle {
        theme.bg_sidebar_active
    } else {
        theme.bg_sidebar_hover
    };
    canvas.fill_rect(layout.split_btn, split_bg);
    canvas.stroke_rect(
        layout.split_btn,
        if split_active { user.accent } else { theme.border },
    );
    font.draw_text(
        &mut canvas,
        (layout.split_btn.x + 8) as i32,
        (layout.split_btn.y + 5) as i32,
        FONT_SIZE_SMALL,
        if split_active { user.accent } else { theme.fg_primary },
        "Split",
        (layout.split_btn.x + layout.split_btn.w - 4) as i32,
    );

    let ports_bg = if ui.ports.open || ui.hovered == HitTarget::PortsToggle {
        theme.bg_sidebar_active
    } else {
        theme.bg_sidebar_hover
    };
    canvas.fill_rect(layout.ports_btn, ports_bg);
    canvas.stroke_rect(
        layout.ports_btn,
        if ui.ports.open { user.accent } else { theme.border },
    );
    font.draw_text(
        &mut canvas,
        (layout.ports_btn.x + 8) as i32,
        (layout.ports_btn.y + 5) as i32,
        FONT_SIZE_SMALL,
        if ui.ports.open { user.accent } else { theme.fg_primary },
        "Ports",
        (layout.ports_btn.x + layout.ports_btn.w - 4) as i32,
    );

    let snip_bg = if ui.palette.open || ui.hovered == HitTarget::SnippetsToggle {
        theme.bg_sidebar_active
    } else {
        theme.bg_sidebar_hover
    };
    canvas.fill_rect(layout.snippets_btn, snip_bg);
    canvas.stroke_rect(
        layout.snippets_btn,
        if ui.palette.open { user.accent } else { theme.border },
    );
    font.draw_text(
        &mut canvas,
        (layout.snippets_btn.x + 10) as i32,
        (layout.snippets_btn.y + 5) as i32,
        FONT_SIZE_SMALL,
        if ui.palette.open { user.accent } else { theme.fg_primary },
        "Snippets",
        (layout.snippets_btn.x + layout.snippets_btn.w - 4) as i32,
    );

    let settings_bg = if ui.settings.open || ui.hovered == HitTarget::SettingsToggle {
        theme.bg_sidebar_active
    } else {
        theme.bg_sidebar_hover
    };
    canvas.fill_rect(layout.settings_btn, settings_bg);
    canvas.stroke_rect(
        layout.settings_btn,
        if ui.settings.open { user.accent } else { theme.border },
    );
    font.draw_text(
        &mut canvas,
        (layout.settings_btn.x + 10) as i32,
        (layout.settings_btn.y + 5) as i32,
        FONT_SIZE_SMALL,
        if ui.settings.open { user.accent } else { theme.fg_primary },
        "Settings",
        (layout.settings_btn.x + layout.settings_btn.w - 4) as i32,
    );

    if let Some(active) = sessions.active() {
        canvas.fill_rect(
            Rect::new(layout.header.x, 0, layout.header.w, 2),
            user.accent,
        );

        let right_edge_x = if ui.update_status.shows_header_pill() {
            layout.update_pill_rect().x
        } else {
            layout.passthrough_pill.x
        };
        let header_max_x = (right_edge_x.saturating_sub(10)) as i32;
        let badge = format!("[{}]", active.shell.badge);
        let x0 = (layout.header.x + 14) as i32;
        let next_x = font.draw_text(
            &mut canvas,
            x0,
            10,
            FONT_SIZE_UI,
            active.shell.accent,
            &badge,
            header_max_x,
        );
        let status = if active.alive { "" } else { " (exited)" };
        let after_title = font.draw_text(
            &mut canvas,
            next_x + 8,
            10,
            FONT_SIZE_UI,
            theme.fg_primary,
            &format!("{}{}", active.title, status),
            header_max_x,
        );

        // Live CWD + Git branch context in header (0ms, no subprocess)
        let cwd_text = abbreviate_path(&active.cwd);
        let after_cwd = font.draw_text(
            &mut canvas,
            after_title + 12,
            11,
            FONT_SIZE_SMALL,
            theme.syntax.path,
            &cwd_text,
            header_max_x,
        );
        if let Some(ref branch) = active.git_branch {
            let git_pill = format!("({branch})");
            font.draw_text(
                &mut canvas,
                after_cwd + 8,
                11,
                FONT_SIZE_SMALL,
                theme.syntax.env_var,
                &git_pill,
                header_max_x,
            );
        }

        if ui.update_status.shows_header_pill() {
            let upd_r = layout.update_pill_rect();
            let (upd_color, upd_label) = match &ui.update_status {
                UpdateStatus::Available(info) => (
                    theme.syntax.string_literal,
                    format!("Update v{}", info.version),
                ),
                UpdateStatus::Downloading(_) => (user.accent, "Updating...".to_string()),
                UpdateStatus::ReadyToRestart { version, .. } => (
                    theme.syntax.string_literal,
                    format!("Restart v{version}"),
                ),
                UpdateStatus::Error {
                    from_install: true, ..
                } => (theme.danger, "Open Releases".to_string()),
                _ => (theme.fg_muted, String::new()),
            };
            let upd_bg = if ui.hovered == HitTarget::UpdateAction {
                upd_color.lerp(theme.bg_header, 145)
            } else {
                upd_color.lerp(theme.bg_header, 195)
            };
            canvas.fill_rect(upd_r, upd_bg);
            canvas.stroke_rect(upd_r, upd_color);
            font.draw_text(
                &mut canvas,
                (upd_r.x + 8) as i32,
                (upd_r.y + 5) as i32,
                FONT_SIZE_SMALL,
                upd_color,
                &upd_label,
                (upd_r.x + upd_r.w - 4) as i32,
            );
        }

        let is_pt = active.effective_passthrough();
        let pill_color = if is_pt {
            theme.syntax.flag
        } else {
            theme.syntax.command
        };
        canvas.fill_rect(
            layout.passthrough_pill,
            pill_color.lerp(theme.bg_header, 200),
        );
        canvas.stroke_rect(layout.passthrough_pill, pill_color.lerp(theme.border, 100));
        let pill_label = if active.grid.is_tui_active() {
            "TUI PASSTHROUGH"
        } else if active.input.passthrough {
            "PASSTHROUGH"
        } else {
            "VELT INPUT"
        };
        font.draw_text(
            &mut canvas,
            (layout.passthrough_pill.x + 10) as i32,
            (layout.passthrough_pill.y + 5) as i32,
            FONT_SIZE_SMALL,
            pill_color,
            pill_label,
            (layout.passthrough_pill.x + layout.passthrough_pill.w - 4) as i32,
        );

        // 9. Terminal grid viewport (single pane or side-by-side Split Panes)
        if let Some((left_idx, right_idx)) = sessions.split_panes() {
            let (left_vp, right_vp) = layout.split_viewports();
            let active_idx = sessions.active_index();
            if let Some(left_sess) = sessions.get(left_idx) {
                render_session_pane(
                    &mut canvas,
                    layout,
                    left_vp,
                    left_sess,
                    active_idx == left_idx,
                    font,
                    theme,
                    user,
                    ui,
                );
            }
            // Vertical split divider
            canvas.fill_rect(
                Rect::new(left_vp.x + left_vp.w, layout.viewport.y, 2, layout.viewport.h),
                theme.border,
            );
            if let Some(right_sess) = sessions.get(right_idx) {
                render_session_pane(
                    &mut canvas,
                    layout,
                    right_vp,
                    right_sess,
                    active_idx == right_idx,
                    font,
                    theme,
                    user,
                    ui,
                );
            }
        } else {
            render_session_pane(
                &mut canvas,
                layout,
                layout.viewport,
                active,
                true,
                font,
                theme,
                user,
                ui,
            );
        }

        // 10. Warp-style syntax-colored input bar
        let term_px = font.term_px;
        canvas.fill_rect(layout.input_bar, theme.bg_input);
        canvas.fill_rect(
            Rect::new(layout.input_bar.x, layout.input_bar.y, layout.input_bar.w, 2),
            if is_pt { theme.border } else { user.accent },
        );

        let ib_max_x = (layout.input_bar.x + layout.input_bar.w - 12) as i32;
        if is_pt {
            let (pt_msg, pt_color) = if let Some(ref toast) = ui.toast {
                (toast.as_str(), theme.syntax.string_literal)
            } else if let Some(ref link_label) = ui.hovered_path_label {
                (link_label.as_str(), user.accent)
            } else {
                (
                    "Passthrough mode active — drag to copy | Click paths & URLs | Ctrl+Shift+P to return to Velt Input",
                    theme.fg_muted,
                )
            };
            font.draw_text(
                &mut canvas,
                (layout.input_bar.x + 14) as i32,
                (layout.input_bar.y + 18) as i32,
                FONT_SIZE_UI,
                pt_color,
                pt_msg,
                ib_max_x,
            );
        } else {
            let prompt = format!("{} > ", user.username);
            let input_y = (layout.input_bar.y + 10) as i32;
            let text_start_x = font.draw_text(
                &mut canvas,
                (layout.input_bar.x + 14) as i32,
                input_y,
                term_px,
                user.accent,
                &prompt,
                ib_max_x,
            );

            let line = active.input.text();
            if line.is_empty() {
                font.draw_text(
                    &mut canvas,
                    text_start_x,
                    input_y,
                    term_px,
                    theme.fg_muted,
                    "Type a command (flags, paths, strings & vars highlight live)...",
                    ib_max_x,
                );
            } else {
                // Visual selection highlight in Velt Input bar (Shift+Home/End/Arrows, Ctrl+A)
                if let Some((c0, c1)) = active.input.selected_char_range() {
                    let pre: String = line.chars().take(c0).collect();
                    let sel: String = line.chars().skip(c0).take(c1 - c0).collect();
                    let sx = text_start_x + font.measure_text(&pre, term_px) as i32;
                    let sw = font.measure_text(&sel, term_px).max(4);
                    let clipped_w = sw.min((ib_max_x - sx).max(0) as u32);
                    if clipped_w > 0 && sx >= 0 {
                        canvas.fill_rect(
                            Rect::new(sx as u32, (input_y + 1) as u32, clipped_w, font.cell_h),
                            user.accent.lerp(theme.bg_input, 155),
                        );
                    }
                }

                let mut pen_x = text_start_x;
                for tok in active.input.tokens() {
                    let slice = &line[tok.range];
                    let color = if tok.kind == TokenKind::Whitespace {
                        theme.fg_primary
                    } else {
                        theme.token_color(tok.kind)
                    };
                    pen_x = font.draw_text(
                        &mut canvas,
                        pen_x,
                        input_y,
                        term_px,
                        color,
                        slice,
                        ib_max_x,
                    );
                }

                if active.input.is_cursor_at_end() {
                    if let Some(ghost) = ghost_suffix {
                        font.draw_text(
                            &mut canvas,
                            pen_x,
                            input_y,
                            term_px,
                            theme.fg_muted,
                            ghost,
                            ib_max_x,
                        );
                    }
                }
            }

            let prefix_chars = active.input.cursor_char_index();
            let prefix_text: String = line.chars().take(prefix_chars).collect();
            let caret_offset = font.measure_text(&prefix_text, term_px) as i32;
            let caret_x = (text_start_x + caret_offset).min(ib_max_x - 2);
            if caret_x >= 0 {
                canvas.fill_rect(
                    Rect::new(caret_x as u32, (input_y + 1) as u32, 2, font.cell_h),
                    user.accent,
                );
            }

            let (hint, hint_color) = if let Some(ref toast) = ui.toast {
                (toast.clone(), theme.syntax.string_literal)
            } else if let Some(ref path_label) = ui.hovered_path_label {
                (path_label.clone(), user.accent)
            } else {
                (
                    "Enter run | Tab complete | Ctrl+F search | Ctrl+C/V copy/paste | Ctrl+, settings | Click paths & URLs"
                        .to_string(),
                    theme.fg_muted,
                )
            };
            font.draw_text(
                &mut canvas,
                (layout.input_bar.x + 14) as i32,
                (layout.input_bar.y + layout.input_bar.h.saturating_sub(18)) as i32,
                FONT_SIZE_SMALL,
                hint_color,
                &hint,
                ib_max_x,
            );

            // 11. Floating top-5 completions & suggestions popup (hidden when Settings modal is open)
            if !suggestions.is_empty() && !ui.settings.open {
                let panel = layout.suggestions_panel(suggestions.len());
                canvas.fill_rect(panel, theme.bg_header);
                canvas.stroke_rect(panel, user.accent.lerp(theme.border, 120));

                for (i, sug) in suggestions.iter().enumerate() {
                    let row = layout.suggestion_row(suggestions.len(), i);
                    let is_sel = i == ui.suggestion_selected;
                    let is_hov = ui.hovered == HitTarget::SuggestionRow(i);
                    if is_sel {
                        canvas.fill_rect(row, theme.bg_sidebar_active);
                        canvas.fill_rect(Rect::new(row.x, row.y, 3, row.h), user.accent);
                    } else if is_hov {
                        canvas.fill_rect(row, theme.bg_sidebar_hover);
                    }

                    let meta_color = match sug.kind {
                        CompletionKind::Directory => theme.syntax.path,
                        CompletionKind::File => theme.syntax.string_literal,
                        CompletionKind::Command => theme.syntax.command,
                        CompletionKind::History => theme.fg_muted,
                    };
                    let meta_w = font.measure_text(&sug.meta, FONT_SIZE_SMALL);
                    let meta_x = (row.x + row.w).saturating_sub(meta_w + 8) as i32;

                    font.draw_text(
                        &mut canvas,
                        (row.x + 10) as i32,
                        (row.y + 5) as i32,
                        FONT_SIZE_UI,
                        if is_sel {
                            theme.fg_primary
                        } else {
                            theme.fg_muted.lerp(theme.fg_primary, 150)
                        },
                        &sug.display,
                        meta_x - 8,
                    );
                    font.draw_text(
                        &mut canvas,
                        meta_x,
                        (row.y + 6) as i32,
                        FONT_SIZE_SMALL,
                        meta_color,
                        &sug.meta,
                        (row.x + row.w - 4) as i32,
                    );
                }
            }
        }
    } else {
        font.draw_text(
            &mut canvas,
            (layout.viewport.x + 24) as i32,
            (layout.viewport.y + 24) as i32,
            font.term_px,
            theme.fg_muted,
            "No active sessions. Click '+ New Session' or press Ctrl+T to open a shell.",
            (layout.viewport.x + layout.viewport.w - 16) as i32,
        );
    }

    // 11b. Optional ACP agent side panel
    if let (Some(panel), Some(active)) = (layout.acp_panel, sessions.active()) {
        if active.acp.open {
            render_acp_panel(
                &mut canvas,
                layout,
                panel,
                &active.acp,
                font,
                theme,
                user,
                ui,
            );
        }
    }

    // 12. Floating Search bar (Ctrl+F)
    if ui.search.open && !ui.settings.open {
        let r = layout.search_bar_rect();
        canvas.fill_rect(r, theme.bg_header);
        canvas.stroke_rect(r, user.accent);
        let count_str = if ui.search.query.is_empty() {
            String::new()
        } else if ui.search.matches.is_empty() {
            "0/0".to_string()
        } else {
            format!("{}/{}", ui.search.active_idx + 1, ui.search.matches.len())
        };
        let close_r = layout.search_close_rect();
        let count_w = font.measure_text(&count_str, FONT_SIZE_SMALL);
        let count_x = close_r.x.saturating_sub(count_w + 8) as i32;

        let display_q = if ui.search.query.is_empty() {
            "Find in output... (Enter/Esc)".to_string()
        } else {
            format!("{}|", ui.search.query)
        };
        font.draw_text(
            &mut canvas,
            (r.x + 10) as i32,
            (r.y + 8) as i32,
            FONT_SIZE_UI,
            if ui.search.query.is_empty() {
                theme.fg_muted
            } else {
                theme.fg_primary
            },
            &display_q,
            count_x - 6,
        );
        if !count_str.is_empty() {
            font.draw_text(
                &mut canvas,
                count_x,
                (r.y + 9) as i32,
                FONT_SIZE_SMALL,
                theme.fg_muted,
                &count_str,
                (close_r.x - 2) as i32,
            );
        }
        font.draw_text(
            &mut canvas,
            (close_r.x + 6) as i32,
            (close_r.y + 3) as i32,
            FONT_SIZE_UI,
            if ui.hovered == HitTarget::SearchClose {
                theme.danger
            } else {
                theme.fg_muted
            },
            "x",
            (close_r.x + close_r.w) as i32,
        );
    }

    // 13. Command Palette / Snippets modal (Ctrl+K)
    if ui.palette.open && !ui.settings.open {
        render_palette_panel(&mut canvas, layout, font, theme, user, ui, snippets);
    }

    // 14. Local Listening Ports modal (Ctrl+Shift+O)
    if ui.ports.open && !ui.settings.open && !ui.palette.open && !ui.history_modal.open {
        render_ports_panel(&mut canvas, layout, font, theme, user, ui);
    }

    // 15. Fuzzy History Search modal (Ctrl+R)
    if ui.history_modal.open && !ui.settings.open {
        render_history_modal(&mut canvas, layout, font, theme, user, ui, history);
    }

    // 16. Settings (Preferences) modal overlay
    if ui.settings.open {
        render_settings_panel(&mut canvas, layout, font, theme, user, ui, cfg);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_session_pane(
    canvas: &mut Canvas<'_>,
    layout: &Layout,
    vp: Rect,
    sess: &Session,
    is_focused: bool,
    font: &mut FontRenderer,
    theme: &Theme,
    user: &UserIdentity,
    ui: &UiState,
) {
    if is_focused && vp.w < layout.viewport.w {
        // Subtle focus indicator bar on top of active split pane
        canvas.fill_rect(Rect::new(vp.x, vp.y, vp.w, 2), user.accent);
    }
    let start_x = vp.x as i32 + 8;
    let start_y = vp.y as i32 + 6;
    let max_x = (vp.x + vp.w) as i32;
    let cw = font.cell_w;
    let ch = font.cell_h;
    let (cur_r, cur_c) = sess.grid.cursor();

    let sel_bounds = if is_focused {
        match (ui.selection_start, ui.selection_end) {
            (Some(a), Some(b)) if a != b => {
                if a <= b { Some((a, b)) } else { Some((b, a)) }
            }
            _ => None,
        }
    } else {
        None
    };
    let sel_bg = user.accent.lerp(theme.bg_terminal, 170);
    let match_bg = Rgb::new(249, 226, 175).lerp(theme.bg_terminal, 145);
    let active_match_bg = Rgb::new(250, 179, 135);

    for row_idx in 0..sess.grid.rows() {
        let cy = start_y + (row_idx as i32) * (ch as i32);
        if cy + ch as i32 > (vp.y + vp.h) as i32 {
            break;
        }
        let abs_line = sess.grid.screen_row_to_abs_line(row_idx, sess.scroll_offset);

        // Warp-style block gutter + top divider
        if !sess.grid.is_alt_screen() {
            if let Some((b_idx, block)) = sess.blocks.iter().enumerate().find(|(_, b)| {
                let end = b.end_abs_line.unwrap_or(usize::MAX);
                abs_line >= b.start_abs_line && abs_line <= end
            }) {
                let gutter_color = if b_idx % 2 == 0 {
                    user.accent.lerp(theme.bg_terminal, 90)
                } else {
                    sess.shell.accent.lerp(theme.bg_terminal, 90)
                };
                canvas.fill_rect(Rect::new(vp.x + 2, cy as u32, 3, ch), gutter_color);
                if abs_line == block.start_abs_line && row_idx > 0 {
                    canvas.fill_rect(
                        Rect::new(
                            vp.x + 6,
                            (cy as u32).saturating_sub(1),
                            vp.w.saturating_sub(12),
                            1,
                        ),
                        theme.border,
                    );
                }
            }
        }

        if let Some(cells) = sess.grid.visible_row(row_idx, sess.scroll_offset) {
            for (col_idx, cell) in cells.iter().enumerate() {
                let cx = start_x + (col_idx as i32) * (cw as i32);
                if cx >= max_x {
                    break;
                }
                let (mut fg, mut bg) = if cell.inverse {
                    (cell.bg, cell.fg)
                } else {
                    (cell.fg, cell.bg)
                };

                if let Some((start, end)) = sel_bounds {
                    let cell = (abs_line, col_idx);
                    if start <= cell && cell <= end {
                        bg = sel_bg;
                    }
                }

                if is_focused && ui.search.open && !ui.search.matches.is_empty() {
                    for (mi, m) in ui.search.matches.iter().enumerate() {
                        if m.abs_line == abs_line && col_idx >= m.col_start && col_idx < m.col_end {
                            if mi == ui.search.active_idx {
                                bg = active_match_bg;
                                fg = Rgb::new(17, 19, 26);
                            } else {
                                bg = match_bg;
                            }
                            break;
                        }
                    }
                }

                let is_hovered_path = is_focused
                    && matches!(
                        ui.hovered_path,
                        Some((hr, c0, c1)) if hr == row_idx && col_idx >= c0 && col_idx < c1
                    );
                if bg != theme.bg_terminal {
                    canvas.fill_rect(Rect::new(cx as u32, cy as u32, cw, ch), bg);
                }
                let base_fg = if cell.dim { fg.lerp(bg, 115) } else { fg };
                let draw_fg = if is_hovered_path { user.accent } else { base_fg };
                font.draw_cell_char(canvas, cx, cy, cell.ch, draw_fg, max_x);
                if cell.underline || is_hovered_path {
                    canvas.fill_rect(
                        Rect::new(cx as u32, (cy as u32) + ch.saturating_sub(2), cw, 1),
                        draw_fg,
                    );
                }
            }
        }

        // Warp-style duration badge + [Copy] and [Re-run] action pills on the focused pane's block start row
        if is_focused
            && !sess.grid.is_alt_screen()
            && !ui.search.open
            && !ui.settings.open
            && !ui.palette.open
            && !ui.ports.open
        {
            if let Some((b_idx, block)) = sess
                .blocks
                .iter()
                .enumerate()
                .find(|(_, b)| b.start_abs_line == abs_line)
            {
                let (copy_r, rerun_r) = layout.block_action_rects_in(vp, row_idx, ch);
                if copy_r.y + copy_r.h <= vp.y + vp.h {
                    // Command execution timer badge to the left of [Copy]
                    let (dur_text, dur_color) = match block.duration {
                        Some(d) => {
                            let c = if d.as_secs() >= 30 {
                                theme.syntax.flag
                            } else if d.as_secs() >= 2 {
                                theme.syntax.path
                            } else {
                                theme.fg_muted
                            };
                            (format_duration_compact(d), c)
                        }
                        None => (
                            format!("{}...", format_duration_compact(block.started_at.elapsed())),
                            user.accent,
                        ),
                    };
                    let dur_w = font.measure_text(&dur_text, FONT_SIZE_SMALL);
                    let dur_x = copy_r.x.saturating_sub(dur_w + 10);
                    if dur_x > vp.x + 80 {
                        font.draw_text(
                            canvas,
                            dur_x as i32,
                            (copy_r.y + 2) as i32,
                            FONT_SIZE_SMALL,
                            dur_color,
                            &dur_text,
                            (copy_r.x - 4) as i32,
                        );
                    }

                    let copy_bg = if ui.hovered == HitTarget::BlockCopy(b_idx) {
                        theme.bg_sidebar_active
                    } else {
                        theme.bg_header
                    };
                    canvas.fill_rect(copy_r, copy_bg);
                    canvas.stroke_rect(copy_r, theme.border);
                    font.draw_text(
                        canvas,
                        (copy_r.x + 8) as i32,
                        (copy_r.y + 2) as i32,
                        FONT_SIZE_SMALL,
                        theme.fg_primary,
                        "Copy",
                        (copy_r.x + copy_r.w - 2) as i32,
                    );

                    let rerun_bg = if ui.hovered == HitTarget::BlockRerun(b_idx) {
                        theme.bg_sidebar_active
                    } else {
                        theme.bg_header
                    };
                    canvas.fill_rect(rerun_r, rerun_bg);
                    canvas.stroke_rect(rerun_r, theme.border);
                    font.draw_text(
                        canvas,
                        (rerun_r.x + 6) as i32,
                        (rerun_r.y + 2) as i32,
                        FONT_SIZE_SMALL,
                        user.accent,
                        "Re-run",
                        (rerun_r.x + rerun_r.w - 2) as i32,
                    );
                }
            }
        }
    }

    if is_focused && sess.scroll_offset == 0 && sess.effective_passthrough() {
        let cx = start_x + (cur_c as i32) * (cw as i32);
        let cy = start_y + (cur_r as i32) * (ch as i32);
        if cx >= 0 && cy >= 0 && (cx as u32) + cw <= vp.x + vp.w {
            canvas.fill_rect(
                Rect::new(cx as u32, cy as u32 + ch.saturating_sub(3), cw, 3),
                user.accent,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_palette_panel(
    canvas: &mut Canvas<'_>,
    layout: &Layout,
    font: &mut FontRenderer,
    theme: &Theme,
    user: &UserIdentity,
    ui: &UiState,
    snippets: &SnippetStore,
) {
    let p = layout.palette_panel();
    let max_x = (p.x + p.w - 14) as i32;

    canvas.fill_rect(p, theme.bg_header);
    canvas.stroke_rect(p, user.accent);
    canvas.fill_rect(Rect::new(p.x, p.y, p.w, 3), user.accent);

    font.draw_text(
        canvas,
        (p.x + 16) as i32,
        (p.y + 12) as i32,
        14.5,
        theme.fg_primary,
        if ui.palette.adding {
            "New Snippet (name = command)"
        } else {
            "Snippets & Workflows (Ctrl+K)"
        },
        (p.x + p.w - 124) as i32,
    );

    let add_r = layout.palette_add_rect();
    canvas.fill_rect(
        add_r,
        if ui.palette.adding || ui.hovered == HitTarget::PaletteAddToggle {
            theme.bg_sidebar_active
        } else {
            theme.bg_input
        },
    );
    canvas.stroke_rect(add_r, if ui.palette.adding { user.accent } else { theme.border });
    font.draw_text(
        canvas,
        (add_r.x + 10) as i32,
        (add_r.y + 4) as i32,
        FONT_SIZE_SMALL,
        if ui.palette.adding { user.accent } else { theme.fg_primary },
        if ui.palette.adding { "Cancel" } else { "+ New" },
        (add_r.x + add_r.w - 4) as i32,
    );

    let close_r = layout.palette_close_rect();
    font.draw_text(
        canvas,
        (close_r.x + 6) as i32,
        (close_r.y + 3) as i32,
        FONT_SIZE_UI,
        if ui.hovered == HitTarget::PaletteClose {
            theme.danger
        } else {
            theme.fg_muted
        },
        "x",
        (close_r.x + close_r.w) as i32,
    );

    // Search / Add input box
    let input_r = Rect::new(p.x + 14, p.y + 42, p.w.saturating_sub(28), 30);
    canvas.fill_rect(input_r, theme.bg_input);
    canvas.stroke_rect(input_r, user.accent);
    let placeholder = if ui.palette.adding {
        "e.g. build-api = cargo build --release  (Enter to save)"
    } else {
        "Filter snippets... (Enter insert | Shift+Enter run | Esc close)"
    };
    let display = if ui.palette.query.is_empty() {
        placeholder.to_string()
    } else {
        format!("{}|", ui.palette.query)
    };
    font.draw_text(
        canvas,
        (input_r.x + 10) as i32,
        (input_r.y + 7) as i32,
        FONT_SIZE_UI,
        if ui.palette.query.is_empty() {
            theme.fg_muted
        } else {
            theme.fg_primary
        },
        &display,
        (input_r.x + input_r.w - 8) as i32,
    );

    if !ui.palette.adding {
        let filtered = snippets.filter(&ui.palette.query);
        for (row_idx, (_, snip)) in filtered.iter().take(7).enumerate() {
            let r = layout.palette_row_rect(row_idx);
            if r.y + r.h > p.y + p.h - 10 {
                break;
            }
            let is_sel = row_idx == ui.palette.selected;
            let is_hov = matches!(
                ui.hovered,
                HitTarget::PaletteRow(i) | HitTarget::PaletteDelete(i) if i == row_idx
            );
            if is_sel {
                canvas.fill_rect(r, theme.bg_sidebar_active);
                canvas.fill_rect(Rect::new(r.x, r.y, 3, r.h), user.accent);
            } else if is_hov {
                canvas.fill_rect(r, theme.bg_sidebar_hover);
            }

            let del_r = layout.palette_delete_rect(row_idx);
            let after_name = font.draw_text(
                canvas,
                (r.x + 10) as i32,
                (r.y + 6) as i32,
                FONT_SIZE_UI,
                user.accent,
                &snip.name,
                (r.x + 150) as i32,
            );
            font.draw_text(
                canvas,
                (after_name + 12).max((r.x + 120) as i32),
                (r.y + 6) as i32,
                FONT_SIZE_SMALL,
                theme.fg_primary,
                &snip.command,
                (del_r.x - 6) as i32,
            );

            if is_sel || is_hov {
                font.draw_text(
                    canvas,
                    (del_r.x + 5) as i32,
                    (del_r.y + 2) as i32,
                    FONT_SIZE_SMALL,
                    if ui.hovered == HitTarget::PaletteDelete(row_idx) {
                        theme.danger
                    } else {
                        theme.fg_muted
                    },
                    "x",
                    (del_r.x + del_r.w) as i32,
                );
            }
        }
        if filtered.is_empty() {
            font.draw_text(
                canvas,
                (p.x + 20) as i32,
                (p.y + 94) as i32,
                FONT_SIZE_UI,
                theme.fg_muted,
                "No matching snippets. Click '+ New' to save one.",
                max_x,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_settings_panel(
    canvas: &mut Canvas<'_>,
    layout: &Layout,
    font: &mut FontRenderer,
    theme: &Theme,
    user: &UserIdentity,
    ui: &UiState,
    cfg: &UserConfig,
) {
    let p = layout.settings_panel();
    let max_x = (p.x + p.w - 14) as i32;

    canvas.fill_rect(p, theme.bg_header);
    canvas.stroke_rect(p, user.accent);
    canvas.fill_rect(Rect::new(p.x, p.y, p.w, 3), user.accent);

    // Header
    font.draw_text(
        canvas,
        (p.x + 20) as i32,
        (p.y + 14) as i32,
        15.5,
        theme.fg_primary,
        "Preferences (Settings)",
        max_x - 36,
    );

    let close_r = layout.settings_close_rect();
    if ui.hovered == HitTarget::SettingsClose {
        canvas.fill_rect(close_r, theme.bg_sidebar_hover);
    }
    font.draw_text(
        canvas,
        (close_r.x + 7) as i32,
        (close_r.y + 4) as i32,
        FONT_SIZE_UI,
        if ui.hovered == HitTarget::SettingsClose {
            theme.danger
        } else {
            theme.fg_muted
        },
        "x",
        (close_r.x + close_r.w) as i32,
    );

    // 1-click Full Theme Presets row
    font.draw_text(
        canvas,
        (p.x + 20) as i32,
        (p.y + 44) as i32,
        FONT_SIZE_SMALL,
        theme.fg_muted,
        "THEME PRESETS (1-click full palette)",
        max_x,
    );
    for (i, preset) in THEME_PRESETS.iter().enumerate() {
        let r = layout.settings_theme_preset_rect(i);
        if r.x + r.w > p.x + p.w - 10 {
            break;
        }
        let is_active_preset = cfg.bg_terminal == preset.bg_terminal && user.accent == preset.accent;
        canvas.fill_rect(
            r,
            if is_active_preset || ui.hovered == HitTarget::SettingsThemePreset(i) {
                theme.bg_sidebar_active
            } else {
                theme.bg_input
            },
        );
        canvas.stroke_rect(
            r,
            if is_active_preset { preset.accent } else { theme.border },
        );
        canvas.fill_rect(Rect::new(r.x + 5, r.y + 7, 6, 10), preset.accent);
        font.draw_text(
            canvas,
            (r.x + 15) as i32,
            (r.y + 5) as i32,
            FONT_SIZE_SMALL,
            theme.fg_primary,
            preset.name,
            (r.x + r.w - 3) as i32,
        );
    }

    // Swatches label
    font.draw_text(
        canvas,
        (p.x + 20) as i32,
        (p.y + 96) as i32,
        FONT_SIZE_SMALL,
        theme.fg_muted,
        "ACCENT PRESETS (click or type any #HEX below)",
        max_x,
    );

    for (i, (_, color)) in PRESET_ACCENTS.iter().enumerate() {
        let r = layout.settings_swatch_rect(i);
        if r.x + r.w > p.x + p.w - 16 {
            break;
        }
        canvas.fill_rect(r, *color);
        let is_selected = user.accent == *color;
        if is_selected || ui.hovered == HitTarget::SettingsSwatch(i) {
            canvas.stroke_rect(r, theme.fg_primary);
        }
    }

    // Editable HEX fields
    let fields = [
        (SettingsField::AccentHex, "Accent Color", user.accent),
        (SettingsField::BgHex, "Background", cfg.bg_terminal),
        (SettingsField::CmdHex, "Commands", cfg.syntax_command),
        (SettingsField::FlagHex, "Flags / Params", cfg.syntax_flag),
    ];

    for (field, label, current_color) in fields {
        let r = layout.settings_field_rect(field);
        let focused = ui.settings.focused_field == Some(field);
        let buf = ui.settings.field_buf(field);
        let valid = parse_hex_color(buf).is_some();

        font.draw_text(
            canvas,
            (p.x + 20) as i32,
            (r.y + 6) as i32,
            FONT_SIZE_UI,
            theme.fg_primary,
            label,
            (r.x - 8) as i32,
        );

        canvas.fill_rect(r, theme.bg_input);
        let border_color = if focused {
            if valid { user.accent } else { theme.danger }
        } else {
            theme.border
        };
        canvas.stroke_rect(r, border_color);

        let display = if focused {
            format!("{buf}|")
        } else {
            buf.to_string()
        };
        font.draw_text(
            canvas,
            (r.x + 8) as i32,
            (r.y + 6) as i32,
            FONT_SIZE_UI,
            theme.fg_primary,
            &display,
            (r.x + r.w - 4) as i32,
        );

        // Live color preview square next to the HEX input
        let preview = Rect::new(r.x + r.w + 10, r.y + 2, 24, r.h.saturating_sub(4));
        canvas.fill_rect(preview, parse_hex_color(buf).unwrap_or(current_color));
        canvas.stroke_rect(preview, theme.border);
    }

    // Divider before typography
    canvas.fill_rect(
        Rect::new(p.x + 20, p.y + 300, p.w.saturating_sub(40), 1),
        theme.border,
    );

    // Font family row
    let prev_r = layout.settings_font_prev_rect();
    let next_r = layout.settings_font_next_rect();
    font.draw_text(
        canvas,
        (p.x + 20) as i32,
        (prev_r.y + 6) as i32,
        FONT_SIZE_UI,
        theme.fg_primary,
        "Font Family",
        (prev_r.x - 8) as i32,
    );
    canvas.fill_rect(
        prev_r,
        if ui.hovered == HitTarget::SettingsFontPrev {
            theme.bg_sidebar_active
        } else {
            theme.bg_input
        },
    );
    canvas.stroke_rect(prev_r, theme.border);
    font.draw_text(
        canvas,
        (prev_r.x + 9) as i32,
        (prev_r.y + 6) as i32,
        FONT_SIZE_UI,
        theme.fg_primary,
        "<",
        (prev_r.x + prev_r.w) as i32,
    );

    let font_box = Rect::new(
        prev_r.x + prev_r.w + 6,
        prev_r.y,
        next_r.x.saturating_sub(prev_r.x + prev_r.w + 12),
        prev_r.h,
    );
    canvas.fill_rect(font_box, theme.bg_input);
    canvas.stroke_rect(font_box, theme.border);
    font.draw_text(
        canvas,
        (font_box.x + 10) as i32,
        (font_box.y + 6) as i32,
        FONT_SIZE_UI,
        theme.fg_primary,
        &font.font_name.clone(),
        (font_box.x + font_box.w - 6) as i32,
    );

    canvas.fill_rect(
        next_r,
        if ui.hovered == HitTarget::SettingsFontNext {
            theme.bg_sidebar_active
        } else {
            theme.bg_input
        },
    );
    canvas.stroke_rect(next_r, theme.border);
    font.draw_text(
        canvas,
        (next_r.x + 9) as i32,
        (next_r.y + 6) as i32,
        FONT_SIZE_UI,
        theme.fg_primary,
        ">",
        (next_r.x + next_r.w) as i32,
    );

    // Font size row
    let down_r = layout.settings_size_down_rect();
    let up_r = layout.settings_size_up_rect();
    font.draw_text(
        canvas,
        (p.x + 20) as i32,
        (down_r.y + 6) as i32,
        FONT_SIZE_UI,
        theme.fg_primary,
        "Font Size",
        (down_r.x - 8) as i32,
    );
    canvas.fill_rect(
        down_r,
        if ui.hovered == HitTarget::SettingsSizeDown {
            theme.bg_sidebar_active
        } else {
            theme.bg_input
        },
    );
    canvas.stroke_rect(down_r, theme.border);
    font.draw_text(
        canvas,
        (down_r.x + 11) as i32,
        (down_r.y + 6) as i32,
        FONT_SIZE_UI,
        theme.fg_primary,
        "-",
        (down_r.x + down_r.w) as i32,
    );

    font.draw_text(
        canvas,
        (down_r.x + down_r.w + 14) as i32,
        (down_r.y + 6) as i32,
        FONT_SIZE_UI,
        theme.fg_primary,
        &format!("{:.0} px", cfg.font_size),
        (up_r.x - 4) as i32,
    );

    canvas.fill_rect(
        up_r,
        if ui.hovered == HitTarget::SettingsSizeUp {
            theme.bg_sidebar_active
        } else {
            theme.bg_input
        },
    );
    canvas.stroke_rect(up_r, theme.border);
    font.draw_text(
        canvas,
        (up_r.x + 10) as i32,
        (up_r.y + 6) as i32,
        FONT_SIZE_UI,
        theme.fg_primary,
        "+",
        (up_r.x + up_r.w) as i32,
    );

    // Agent Token Plugins (24h) opt-in toggles
    canvas.fill_rect(
        Rect::new(p.x + 20, p.y + 386, p.w.saturating_sub(40), 1),
        theme.border,
    );
    font.draw_text(
        canvas,
        (p.x + 20) as i32,
        (p.y + 394) as i32,
        FONT_SIZE_SMALL,
        theme.fg_muted,
        "CLI AGENT TOKEN PLUGINS (24h local logs)",
        max_x,
    );
    let (r_agy, r_claude, r_codex) = layout.settings_plugin_rects();
    let plugin_boxes = [
        (r_agy, cfg.plugin_agy, "Antigravity (agy)", HitTarget::SettingsPluginAgy),
        (r_claude, cfg.plugin_claude, "Claude Code", HitTarget::SettingsPluginClaude),
        (r_codex, cfg.plugin_codex, "Codex CLI", HitTarget::SettingsPluginCodex),
    ];
    for (r, enabled, label, target) in plugin_boxes {
        if r.x + r.w > p.x + p.w - 10 {
            continue;
        }
        canvas.fill_rect(
            r,
            if enabled {
                theme.bg_sidebar_active
            } else if ui.hovered == target {
                theme.bg_sidebar_hover
            } else {
                theme.bg_input
            },
        );
        canvas.stroke_rect(r, if enabled { user.accent } else { theme.border });
        let prefix = if enabled { "[x] " } else { "[ ] " };
        font.draw_text(
            canvas,
            (r.x + 8) as i32,
            (r.y + 7) as i32,
            FONT_SIZE_SMALL,
            if enabled { user.accent } else { theme.fg_primary },
            &format!("{prefix}{label}"),
            (r.x + r.w - 4) as i32,
        );
    }

    // Footer: Reset button + status note
    let reset_r = layout.settings_reset_rect();
    canvas.fill_rect(
        reset_r,
        if ui.hovered == HitTarget::SettingsReset {
            theme.bg_sidebar_active
        } else {
            theme.bg_input
        },
    );
    canvas.stroke_rect(reset_r, theme.border);
    font.draw_text(
        canvas,
        (reset_r.x + 12) as i32,
        (reset_r.y + 5) as i32,
        FONT_SIZE_SMALL,
        theme.fg_primary,
        "Reset Defaults",
        (reset_r.x + reset_r.w - 4) as i32,
    );

    let upd_r = layout.settings_update_rect();
    let (upd_btn_label, upd_btn_color) = match &ui.update_status {
        UpdateStatus::Checking => ("Checking...".to_string(), theme.fg_muted),
        UpdateStatus::Available(info) => (
            format!("Update v{}", info.version),
            theme.syntax.string_literal,
        ),
        UpdateStatus::Downloading(_) => ("Downloading...".to_string(), user.accent),
        UpdateStatus::ReadyToRestart { version, .. } => (
            format!("Restart v{version}"),
            theme.syntax.string_literal,
        ),
        UpdateStatus::UpToDate => ("Up to date".to_string(), theme.fg_muted),
        UpdateStatus::Error {
            from_install: true, ..
        } => ("Open Releases".to_string(), theme.danger),
        UpdateStatus::Error {
            from_install: false, ..
        } => ("Retry Update".to_string(), theme.danger),
        UpdateStatus::Idle => ("Check Updates".to_string(), theme.fg_primary),
    };
    canvas.fill_rect(
        upd_r,
        if ui.hovered == HitTarget::SettingsCheckUpdate {
            theme.bg_sidebar_active
        } else {
            theme.bg_input
        },
    );
    canvas.stroke_rect(
        upd_r,
        if matches!(
            ui.update_status,
            UpdateStatus::Available(_) | UpdateStatus::ReadyToRestart { .. }
        ) {
            theme.syntax.string_literal
        } else {
            theme.border
        },
    );
    font.draw_text(
        canvas,
        (upd_r.x + 12) as i32,
        (upd_r.y + 5) as i32,
        FONT_SIZE_SMALL,
        upd_btn_color,
        &upd_btn_label,
        (upd_r.x + upd_r.w - 4) as i32,
    );

    font.draw_text(
        canvas,
        (upd_r.x + upd_r.w + 12) as i32,
        (upd_r.y + 6) as i32,
        FONT_SIZE_SMALL,
        theme.fg_muted,
        &format!("v{CURRENT_VERSION}  |  Esc to close"),
        max_x,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_ports_panel(
    canvas: &mut Canvas<'_>,
    layout: &Layout,
    font: &mut FontRenderer,
    theme: &Theme,
    user: &UserIdentity,
    ui: &UiState,
) {
    let p = layout.ports_panel();
    let max_x = (p.x + p.w - 14) as i32;

    canvas.fill_rect(p, theme.bg_header);
    canvas.stroke_rect(p, user.accent);
    canvas.fill_rect(Rect::new(p.x, p.y, p.w, 3), user.accent);

    font.draw_text(
        canvas,
        (p.x + 16) as i32,
        (p.y + 12) as i32,
        14.5,
        theme.fg_primary,
        "Local Listening Ports (Ctrl+Shift+O)",
        (p.x + p.w - 120) as i32,
    );

    let ref_r = layout.ports_refresh_rect();
    canvas.fill_rect(
        ref_r,
        if ui.hovered == HitTarget::PortsRefresh {
            theme.bg_sidebar_active
        } else {
            theme.bg_input
        },
    );
    canvas.stroke_rect(ref_r, theme.border);
    font.draw_text(
        canvas,
        (ref_r.x + 10) as i32,
        (ref_r.y + 4) as i32,
        FONT_SIZE_SMALL,
        theme.fg_primary,
        "Refresh",
        (ref_r.x + ref_r.w - 4) as i32,
    );

    let close_r = layout.ports_close_rect();
    font.draw_text(
        canvas,
        (close_r.x + 6) as i32,
        (close_r.y + 3) as i32,
        FONT_SIZE_UI,
        if ui.hovered == HitTarget::PortsClose {
            theme.danger
        } else {
            theme.fg_muted
        },
        "x",
        (close_r.x + close_r.w) as i32,
    );

    if ui.ports.loading && ui.ports.ports.is_empty() {
        font.draw_text(
            canvas,
            (p.x + 18) as i32,
            (p.y + 60) as i32,
            FONT_SIZE_UI,
            theme.fg_muted,
            "Scanning local listening TCP ports...",
            max_x,
        );
        return;
    }

    if ui.ports.ports.is_empty() {
        font.draw_text(
            canvas,
            (p.x + 18) as i32,
            (p.y + 60) as i32,
            FONT_SIZE_UI,
            theme.fg_muted,
            "No listening TCP ports found.",
            max_x,
        );
        return;
    }

    for (i, lp) in ui.ports.ports.iter().take(8).enumerate() {
        let r = layout.ports_row_rect(i);
        if r.y + r.h > p.y + p.h - 10 {
            break;
        }
        canvas.fill_rect(r, theme.bg_input);
        canvas.stroke_rect(r, theme.border);

        let (open_r, kill_r) = layout.ports_action_rects(i);
        let port_str = format!(":{}", lp.port);
        let after_port = font.draw_text(
            canvas,
            (r.x + 10) as i32,
            (r.y + 7) as i32,
            FONT_SIZE_UI,
            theme.syntax.command,
            &port_str,
            (r.x + 90) as i32,
        );
        let proc_x = (after_port + 12).max((r.x + 76) as i32);
        font.draw_text(
            canvas,
            proc_x,
            (r.y + 7) as i32,
            FONT_SIZE_UI,
            theme.fg_primary,
            &lp.process,
            (open_r.x.saturating_sub(80)) as i32,
        );
        font.draw_text(
            canvas,
            (open_r.x.saturating_sub(74)) as i32,
            (r.y + 8) as i32,
            FONT_SIZE_SMALL,
            theme.fg_muted,
            &format!("PID {}", lp.pid),
            (open_r.x.saturating_sub(4)) as i32,
        );

        canvas.fill_rect(
            open_r,
            if ui.hovered == HitTarget::PortOpen(i) {
                theme.bg_sidebar_active
            } else {
                theme.bg_header
            },
        );
        canvas.stroke_rect(open_r, user.accent.lerp(theme.border, 120));
        font.draw_text(
            canvas,
            (open_r.x + 8) as i32,
            (open_r.y + 4) as i32,
            FONT_SIZE_SMALL,
            user.accent,
            "Open",
            (open_r.x + open_r.w - 2) as i32,
        );

        canvas.fill_rect(
            kill_r,
            if ui.hovered == HitTarget::PortKill(i) {
                theme.danger.lerp(theme.bg_header, 160)
            } else {
                theme.bg_header
            },
        );
        canvas.stroke_rect(kill_r, theme.danger.lerp(theme.border, 120));
        font.draw_text(
            canvas,
            (kill_r.x + 10) as i32,
            (kill_r.y + 4) as i32,
            FONT_SIZE_SMALL,
            theme.danger,
            "Kill",
            (kill_r.x + kill_r.w - 2) as i32,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_history_modal(
    canvas: &mut Canvas<'_>,
    layout: &Layout,
    font: &mut FontRenderer,
    theme: &Theme,
    user: &UserIdentity,
    ui: &UiState,
    history: &HistoryStore,
) {
    let p = layout.palette_panel();
    let max_x = (p.x + p.w - 14) as i32;

    canvas.fill_rect(p, theme.bg_header);
    canvas.stroke_rect(p, user.accent);
    canvas.fill_rect(Rect::new(p.x, p.y, p.w, 3), user.accent);

    font.draw_text(
        canvas,
        (p.x + 16) as i32,
        (p.y + 12) as i32,
        14.5,
        theme.fg_primary,
        "History Search (Ctrl+R)",
        (p.x + p.w - 44) as i32,
    );

    let close_r = layout.palette_close_rect();
    font.draw_text(
        canvas,
        (close_r.x + 6) as i32,
        (close_r.y + 3) as i32,
        FONT_SIZE_UI,
        if ui.hovered == HitTarget::HistoryClose {
            theme.danger
        } else {
            theme.fg_muted
        },
        "x",
        (close_r.x + close_r.w) as i32,
    );

    let input_r = Rect::new(p.x + 14, p.y + 42, p.w.saturating_sub(28), 30);
    canvas.fill_rect(input_r, theme.bg_input);
    canvas.stroke_rect(input_r, user.accent);
    let display = if ui.history_modal.query.is_empty() {
        "Filter history... (Enter run | Tab edit | Esc close)".to_string()
    } else {
        format!("{}|", ui.history_modal.query)
    };
    font.draw_text(
        canvas,
        (input_r.x + 10) as i32,
        (input_r.y + 7) as i32,
        FONT_SIZE_UI,
        if ui.history_modal.query.is_empty() {
            theme.fg_muted
        } else {
            theme.fg_primary
        },
        &display,
        (input_r.x + input_r.w - 8) as i32,
    );

    let hits = history.search_fuzzy(&ui.history_modal.query, 7);
    for (row_idx, item) in hits.iter().enumerate() {
        let r = layout.palette_row_rect(row_idx);
        if r.y + r.h > p.y + p.h - 10 {
            break;
        }
        let is_sel = row_idx == ui.history_modal.selected;
        let is_hov = ui.hovered == HitTarget::HistoryRow(row_idx);
        if is_sel {
            canvas.fill_rect(r, theme.bg_sidebar_active);
            canvas.fill_rect(Rect::new(r.x, r.y, 3, r.h), user.accent);
        } else if is_hov {
            canvas.fill_rect(r, theme.bg_sidebar_hover);
        }

        let meta = format!("{} {}x", item.shell_badge, item.count);
        let meta_w = font.measure_text(&meta, FONT_SIZE_SMALL);
        let meta_x = (r.x + r.w).saturating_sub(meta_w + 8) as i32;

        font.draw_text(
            canvas,
            (r.x + 10) as i32,
            (r.y + 6) as i32,
            FONT_SIZE_UI,
            if is_sel { theme.fg_primary } else { theme.fg_muted.lerp(theme.fg_primary, 150) },
            &item.command,
            meta_x - 8,
        );
        font.draw_text(
            canvas,
            meta_x,
            (r.y + 6) as i32,
            FONT_SIZE_SMALL,
            theme.fg_muted,
            &meta,
            (r.x + r.w - 4) as i32,
        );
    }
    if hits.is_empty() {
        font.draw_text(
            canvas,
            (p.x + 20) as i32,
            (p.y + 94) as i32,
            FONT_SIZE_UI,
            theme.fg_muted,
            "No matching commands in local history.",
            max_x,
        );
    }
}

fn acp_status_color(acp: &AcpSessionState, theme: &Theme) -> Rgb {
    match acp.chat.connection() {
        ConnectionState::Disconnected => theme.fg_muted,
        ConnectionState::Connecting => theme.syntax.flag,
        ConnectionState::Active => theme.syntax.string_literal,
        ConnectionState::Error(_) => theme.danger,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AcpRowKind {
    UserPrompt,
    Heading,
    Body,
    Bullet,
    Quote,
    CodeHeader,
    Code,
    Muted,
    Error,
}

struct AcpRow {
    kind: AcpRowKind,
    text: String,
}

#[allow(clippy::too_many_arguments)]
fn render_acp_panel(
    canvas: &mut Canvas<'_>,
    layout: &Layout,
    panel: Rect,
    acp: &AcpSessionState,
    font: &mut FontRenderer,
    theme: &Theme,
    user: &UserIdentity,
    ui: &UiState,
) {
    // Left divider + panel background
    let acp_div_active =
        ui.hovered == HitTarget::ResizeAcp || ui.resizing == Some(ResizeHandle::AcpPanel);
    canvas.fill_rect(
        Rect::new(
            panel.x.saturating_sub(if acp_div_active { 2 } else { 1 }),
            panel.y,
            if acp_div_active { 2 } else { 1 },
            panel.h,
        ),
        if acp_div_active { user.accent } else { theme.border },
    );
    canvas.fill_rect(panel, theme.bg_sidebar);

    if let Some(hdr) = layout.acp_header_rect() {
        canvas.fill_rect(hdr, theme.bg_header);
        canvas.fill_rect(
            Rect::new(hdr.x, hdr.y + hdr.h.saturating_sub(1), hdr.w, 1),
            theme.border,
        );
    }

    if let Some(sel_r) = layout.acp_agent_selector_rect() {
        let sel_bg = if ui.acp_dropdown_open || ui.hovered == HitTarget::AcpAgentSelector {
            theme.bg_sidebar_active
        } else {
            theme.bg_input
        };
        canvas.fill_rect(sel_r, sel_bg);
        canvas.stroke_rect(
            sel_r,
            if ui.acp_dropdown_open {
                user.accent
            } else {
                theme.border
            },
        );
        let label = format!("{} v", acp.agent.label());
        font.draw_text(
            canvas,
            (sel_r.x + 8) as i32,
            (sel_r.y + 5) as i32,
            FONT_SIZE_SMALL,
            theme.fg_primary,
            &label,
            (sel_r.x + sel_r.w - 6) as i32,
        );

        let status_color = acp_status_color(acp, theme);
        let status_x = sel_r.x + sel_r.w + 10;
        let close_x = layout
            .acp_close_rect()
            .map(|c| c.x)
            .unwrap_or(panel.x + panel.w - 28);
        if status_x + 24 < close_x {
            canvas.fill_rect(Rect::new(status_x, sel_r.y + 9, 6, 6), status_color);
            font.draw_text(
                canvas,
                (status_x + 10) as i32,
                (sel_r.y + 5) as i32,
                FONT_SIZE_SMALL,
                status_color,
                acp.status_label(),
                (close_x.saturating_sub(4)) as i32,
            );
        }
    }

    if let Some(close_r) = layout.acp_close_rect() {
        font.draw_text(
            canvas,
            (close_r.x + 6) as i32,
            (close_r.y + 2) as i32,
            FONT_SIZE_UI,
            if ui.hovered == HitTarget::AcpClose {
                theme.danger
            } else {
                theme.fg_muted
            },
            "x",
            (close_r.x + close_r.w) as i32,
        );
    }

    if let Some(mut msg_r) = layout.acp_messages_rect() {
        if acp.chat.pending_permission().is_some() {
            if let Some(card_r) = layout.acp_permission_card_rect() {
                msg_r.h = card_r.y.saturating_sub(msg_r.y + 6);
            }
        }
        let char_w = font.measure_text("M", FONT_SIZE_SMALL).max(6);
        let max_cols = ((msg_r.w.saturating_sub(18)) / char_w).max(12) as usize;
        let mut rows: Vec<AcpRow> = Vec::new();

        let has_error = matches!(acp.chat.connection(), ConnectionState::Error(_));
        if acp.turns.is_empty()
            && acp.pending_prompt.is_none()
            && acp.chat.response().is_empty()
            && !has_error
        {
            for line in crate::acp::markdown::wrap_text(
                "ACP local agent mode (OAuth via CLI, no API keys).",
                max_cols,
            ) {
                rows.push(AcpRow {
                    kind: AcpRowKind::Muted,
                    text: line,
                });
            }
            for line in crate::acp::markdown::wrap_text(
                "Type a prompt below and press Enter to start.",
                max_cols,
            ) {
                rows.push(AcpRow {
                    kind: AcpRowKind::Muted,
                    text: line,
                });
            }
        } else {
            for turn in &acp.turns {
                push_prompt_rows(&mut rows, &turn.prompt, max_cols);
                push_markdown_rows(&mut rows, &turn.response, max_cols);
                if turn.truncated {
                    rows.push(AcpRow {
                        kind: AcpRowKind::Muted,
                        text: "[response truncated at 64 KiB]".to_string(),
                    });
                }
                rows.push(AcpRow {
                    kind: AcpRowKind::Body,
                    text: String::new(),
                });
            }
            if let Some(pending) = &acp.pending_prompt {
                push_prompt_rows(&mut rows, pending, max_cols);
                if !acp.chat.response().is_empty() {
                    push_markdown_rows(&mut rows, acp.chat.response(), max_cols);
                    if acp.chat.response_truncated() {
                        rows.push(AcpRow {
                            kind: AcpRowKind::Muted,
                            text: "[response truncated at 64 KiB]".to_string(),
                        });
                    }
                } else if acp.chat.is_prompt_pending() {
                    rows.push(AcpRow {
                        kind: AcpRowKind::Muted,
                        text: "Thinking...".to_string(),
                    });
                }
            }
        }

        if let ConnectionState::Error(err) = acp.chat.connection() {
            if !rows.is_empty() {
                rows.push(AcpRow {
                    kind: AcpRowKind::Body,
                    text: String::new(),
                });
            }
            for part in crate::acp::markdown::wrap_text(err, max_cols) {
                rows.push(AcpRow {
                    kind: AcpRowKind::Error,
                    text: part,
                });
            }
        }

        let line_h = 18u32;
        let visible_rows = (msg_r.h / line_h).max(1) as usize;
        let max_scroll = rows.len().saturating_sub(visible_rows);
        let scroll = acp.scroll.min(max_scroll);
        let start = rows.len().saturating_sub(visible_rows + scroll);
        let end = (start + visible_rows).min(rows.len());
        let max_x = (msg_r.x + msg_r.w - 4) as i32;

        for (i, row) in rows[start..end].iter().enumerate() {
            let y = msg_r.y + (i as u32) * line_h;
            match row.kind {
                AcpRowKind::UserPrompt => {
                    canvas.fill_rect(Rect::new(msg_r.x, y, 2, line_h), user.accent);
                    font.draw_text(
                        canvas,
                        (msg_r.x + 8) as i32,
                        (y + 2) as i32,
                        FONT_SIZE_SMALL,
                        user.accent,
                        &row.text,
                        max_x,
                    );
                }
                AcpRowKind::Heading => {
                    font.draw_text(
                        canvas,
                        (msg_r.x + 4) as i32,
                        (y + 2) as i32,
                        FONT_SIZE_UI,
                        user.accent,
                        &row.text,
                        max_x,
                    );
                }
                AcpRowKind::Bullet => {
                    font.draw_text(
                        canvas,
                        (msg_r.x + 6) as i32,
                        (y + 2) as i32,
                        FONT_SIZE_SMALL,
                        theme.fg_primary,
                        &row.text,
                        max_x,
                    );
                }
                AcpRowKind::Quote => {
                    font.draw_text(
                        canvas,
                        (msg_r.x + 6) as i32,
                        (y + 2) as i32,
                        FONT_SIZE_SMALL,
                        theme.fg_muted,
                        &row.text,
                        max_x,
                    );
                }
                AcpRowKind::CodeHeader => {
                    canvas.fill_rect(
                        Rect::new(msg_r.x + 2, y, msg_r.w.saturating_sub(4), line_h),
                        theme.bg_header,
                    );
                    canvas.fill_rect(Rect::new(msg_r.x + 2, y, 2, line_h), theme.syntax.flag);
                    font.draw_text(
                        canvas,
                        (msg_r.x + 10) as i32,
                        (y + 2) as i32,
                        FONT_SIZE_SMALL,
                        theme.syntax.flag,
                        &row.text,
                        max_x,
                    );
                }
                AcpRowKind::Code => {
                    canvas.fill_rect(
                        Rect::new(msg_r.x + 2, y, msg_r.w.saturating_sub(4), line_h),
                        theme.bg_terminal,
                    );
                    canvas.fill_rect(Rect::new(msg_r.x + 2, y, 2, line_h), user.accent);
                    font.draw_text(
                        canvas,
                        (msg_r.x + 10) as i32,
                        (y + 2) as i32,
                        FONT_SIZE_SMALL,
                        theme.syntax.string_literal,
                        &row.text,
                        max_x,
                    );
                }
                AcpRowKind::Muted => {
                    font.draw_text(
                        canvas,
                        (msg_r.x + 4) as i32,
                        (y + 2) as i32,
                        FONT_SIZE_SMALL,
                        theme.fg_muted,
                        &row.text,
                        max_x,
                    );
                }
                AcpRowKind::Error => {
                    canvas.fill_rect(
                        Rect::new(msg_r.x + 2, y, msg_r.w.saturating_sub(4), line_h),
                        theme.danger.lerp(theme.bg_sidebar, 210),
                    );
                    font.draw_text(
                        canvas,
                        (msg_r.x + 8) as i32,
                        (y + 2) as i32,
                        FONT_SIZE_SMALL,
                        theme.danger,
                        &row.text,
                        max_x,
                    );
                }
                AcpRowKind::Body => {
                    if !row.text.is_empty() {
                        font.draw_text(
                            canvas,
                            (msg_r.x + 4) as i32,
                            (y + 2) as i32,
                            FONT_SIZE_SMALL,
                            theme.fg_primary,
                            &row.text,
                            max_x,
                        );
                    }
                }
            }
        }
    }

    if let Some(perm) = acp.chat.pending_permission() {
        if let Some(card_r) = layout.acp_permission_card_rect() {
            canvas.fill_rect(card_r, theme.bg_header);
            canvas.stroke_rect(card_r, theme.syntax.flag);
            canvas.fill_rect(Rect::new(card_r.x, card_r.y, card_r.w, 2), theme.syntax.flag);
            let card_max_x = (card_r.x + card_r.w - 8) as i32;
            font.draw_text(
                canvas,
                (card_r.x + 8) as i32,
                (card_r.y + 7) as i32,
                FONT_SIZE_SMALL,
                theme.fg_primary,
                &perm.title,
                card_max_x,
            );
            if let Some(detail) = &perm.detail {
                font.draw_text(
                    canvas,
                    (card_r.x + 8) as i32,
                    (card_r.y + 24) as i32,
                    FONT_SIZE_SMALL,
                    theme.fg_muted,
                    detail,
                    card_max_x,
                );
            }
            if let Some(app_r) = layout.acp_permission_approve_rect() {
                let app_bg = if ui.hovered == HitTarget::AcpPermissionApprove {
                    theme.syntax.string_literal.lerp(theme.bg_header, 140)
                } else {
                    theme.syntax.string_literal.lerp(theme.bg_header, 195)
                };
                canvas.fill_rect(app_r, app_bg);
                canvas.stroke_rect(app_r, theme.syntax.string_literal);
                font.draw_text(
                    canvas,
                    (app_r.x + 14) as i32,
                    (app_r.y + 4) as i32,
                    FONT_SIZE_SMALL,
                    theme.syntax.string_literal,
                    "Aprobar",
                    (app_r.x + app_r.w - 4) as i32,
                );
            }
            if let Some(rej_r) = layout.acp_permission_reject_rect() {
                let rej_bg = if ui.hovered == HitTarget::AcpPermissionReject {
                    theme.danger.lerp(theme.bg_header, 140)
                } else {
                    theme.danger.lerp(theme.bg_header, 195)
                };
                canvas.fill_rect(rej_r, rej_bg);
                canvas.stroke_rect(rej_r, theme.danger);
                font.draw_text(
                    canvas,
                    (rej_r.x + 12) as i32,
                    (rej_r.y + 4) as i32,
                    FONT_SIZE_SMALL,
                    theme.danger,
                    "Rechazar",
                    (rej_r.x + rej_r.w - 4) as i32,
                );
            }
        }
    }

    if let Some(ctx_r) = layout.acp_context_bar_rect() {
        let on = acp.include_context;
        let bg = if ui.hovered == HitTarget::AcpContextToggle {
            theme.bg_sidebar_active
        } else if on {
            user.accent.lerp(theme.bg_sidebar, 210)
        } else {
            theme.bg_input
        };
        canvas.fill_rect(ctx_r, bg);
        canvas.stroke_rect(
            ctx_r,
            if on { user.accent } else { theme.border },
        );
        let label = if on {
            "[x] Contexto PTY (cwd + 40 líneas)"
        } else {
            "[ ] Contexto PTY (cwd + 40 líneas)"
        };
        font.draw_text(
            canvas,
            (ctx_r.x + 8) as i32,
            (ctx_r.y + 5) as i32,
            FONT_SIZE_SMALL,
            if on { user.accent } else { theme.fg_muted },
            label,
            (ctx_r.x + ctx_r.w - 6) as i32,
        );
    }

    if let Some(auto_r) = layout.acp_auto_toggle_rect() {
        let auto_on = acp.auto_approve;
        let bg = if ui.hovered == HitTarget::AcpAutoToggle {
            theme.bg_sidebar_active
        } else if auto_on {
            theme.syntax.flag.lerp(theme.bg_sidebar, 200)
        } else {
            theme.bg_input
        };
        canvas.fill_rect(auto_r, bg);
        canvas.stroke_rect(
            auto_r,
            if auto_on { theme.syntax.flag } else { theme.border },
        );
        font.draw_text(
            canvas,
            (auto_r.x + 8) as i32,
            (auto_r.y + 5) as i32,
            FONT_SIZE_SMALL,
            if auto_on { theme.syntax.flag } else { theme.fg_muted },
            if auto_on { "[x] Auto" } else { "[ ] Auto" },
            (auto_r.x + auto_r.w - 4) as i32,
        );
    }

    if let Some(inp_r) = layout.acp_input_rect() {
        canvas.fill_rect(inp_r, theme.bg_input);
        canvas.stroke_rect(
            inp_r,
            if ui.acp_focused {
                user.accent
            } else {
                theme.border
            },
        );

        let text = acp.input.text();
        let text_x = (inp_r.x + 8) as i32;
        let text_y = (inp_r.y + 9) as i32;
        let inp_max_x = (inp_r.x + inp_r.w - 8) as i32;

        if text.is_empty() {
            font.draw_text(
                canvas,
                text_x,
                text_y,
                FONT_SIZE_SMALL,
                theme.fg_muted,
                "Message ACP agent...",
                inp_max_x,
            );
            if ui.acp_focused {
                canvas.fill_rect(Rect::new(inp_r.x + 8, inp_r.y + 7, 2, 18), user.accent);
            }
        } else {
            let cursor_char = acp.input.cursor_char_index();
            let avail_w = inp_r.w.saturating_sub(18);
            let mut view_start_char = 0usize;
            let chars: Vec<char> = text.chars().collect();
            while view_start_char < cursor_char {
                let slice: String = chars[view_start_char..cursor_char].iter().collect();
                if font.measure_text(&slice, FONT_SIZE_SMALL) <= avail_w {
                    break;
                }
                view_start_char += 1;
            }
            let visible_text: String = chars[view_start_char..].iter().collect();

            if let Some((sel_a, sel_b)) = acp.input.selected_char_range() {
                if sel_b > view_start_char {
                    let s0 = sel_a.saturating_sub(view_start_char);
                    let s1 = sel_b.saturating_sub(view_start_char);
                    let before: String =
                        chars[view_start_char..view_start_char + s0].iter().collect();
                    let span: String =
                        chars[view_start_char + s0..view_start_char + s1].iter().collect();
                    let x0 = text_x + font.measure_text(&before, FONT_SIZE_SMALL) as i32;
                    let sw = font.measure_text(&span, FONT_SIZE_SMALL).max(4);
                    let max_w = (inp_max_x - x0).max(0) as u32;
                    if max_w > 0 {
                        canvas.fill_rect(
                            Rect::new(x0.max(0) as u32, inp_r.y + 6, sw.min(max_w), 20),
                            user.accent.lerp(theme.bg_input, 160),
                        );
                    }
                }
            }

            font.draw_text(
                canvas,
                text_x,
                text_y,
                FONT_SIZE_SMALL,
                theme.fg_primary,
                &visible_text,
                inp_max_x,
            );

            if ui.acp_focused {
                let prefix: String = chars[view_start_char..cursor_char].iter().collect();
                let caret_x = (text_x + font.measure_text(&prefix, FONT_SIZE_SMALL) as i32)
                    .min(inp_max_x - 2);
                if caret_x >= 0 {
                    canvas.fill_rect(Rect::new(caret_x as u32, inp_r.y + 7, 2, 18), user.accent);
                }
            }
        }
    }

    if let Some(send_r) = layout.acp_send_rect() {
        let can_send = matches!(acp.chat.connection(), ConnectionState::Active)
            && !acp.chat.is_prompt_pending()
            && !acp.input.text().trim().is_empty();
        let bg = if can_send && ui.hovered == HitTarget::AcpSend {
            user.accent.lerp(theme.bg_input, 120)
        } else if can_send {
            user.accent.lerp(theme.bg_input, 170)
        } else {
            theme.bg_sidebar_hover
        };
        canvas.fill_rect(send_r, bg);
        canvas.stroke_rect(
            send_r,
            if can_send { user.accent } else { theme.border },
        );
        font.draw_text(
            canvas,
            (send_r.x + 12) as i32,
            (send_r.y + 9) as i32,
            FONT_SIZE_SMALL,
            if can_send {
                theme.fg_primary
            } else {
                theme.fg_muted
            },
            if acp.chat.is_prompt_pending() {
                "..."
            } else {
                "Send"
            },
            (send_r.x + send_r.w - 4) as i32,
        );
    }

    if ui.acp_dropdown_open {
        if let Some(drop_r) = layout.acp_dropdown_panel(Agent::ALL.len()) {
            canvas.fill_rect(drop_r, theme.bg_header);
            canvas.stroke_rect(drop_r, user.accent);
            for (i, ag) in Agent::ALL.iter().enumerate() {
                if let Some(opt_r) = layout.acp_agent_option_rect(i) {
                    let is_sel = *ag == acp.agent;
                    let is_hov = ui.hovered == HitTarget::AcpAgentOption(i);
                    if is_sel {
                        canvas.fill_rect(opt_r, theme.bg_sidebar_active);
                        canvas.fill_rect(Rect::new(opt_r.x, opt_r.y, 3, opt_r.h), user.accent);
                    } else if is_hov {
                        canvas.fill_rect(opt_r, theme.bg_sidebar_hover);
                    }
                    font.draw_text(
                        canvas,
                        (opt_r.x + 10) as i32,
                        (opt_r.y + 5) as i32,
                        FONT_SIZE_SMALL,
                        if is_sel {
                            user.accent
                        } else {
                            theme.fg_primary
                        },
                        ag.label(),
                        (opt_r.x + opt_r.w - 6) as i32,
                    );
                }
            }
        }
    }
}

fn push_prompt_rows(rows: &mut Vec<AcpRow>, prompt: &str, max_cols: usize) {
    let inner = max_cols.saturating_sub(2).max(6);
    for (idx, part) in crate::acp::markdown::wrap_text(prompt, inner)
        .into_iter()
        .enumerate()
    {
        let prefix = if idx == 0 { "> " } else { "  " };
        rows.push(AcpRow {
            kind: AcpRowKind::UserPrompt,
            text: format!("{prefix}{part}"),
        });
    }
}

fn push_markdown_rows(rows: &mut Vec<AcpRow>, raw: &str, max_cols: usize) {
    for md in parse_markdown(raw, max_cols) {
        let kind = match md.kind {
            MdLineKind::Heading => AcpRowKind::Heading,
            MdLineKind::Body => AcpRowKind::Body,
            MdLineKind::Bullet => AcpRowKind::Bullet,
            MdLineKind::Quote => AcpRowKind::Quote,
            MdLineKind::CodeHeader => AcpRowKind::CodeHeader,
            MdLineKind::Code => AcpRowKind::Code,
        };
        rows.push(AcpRow {
            kind,
            text: md.text,
        });
    }
}

/// Procedurally generates a crisp 32x32 RGBA app icon (`>_` inside a rounded dark frame).
/// Zero image-decoding dependencies; used for both window taskbar icon and `.exe` resource.
pub fn build_app_icon_rgba(accent: Rgb) -> Vec<u8> {
    const SIZE: usize = 32;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    let bg = Rgb::new(18, 21, 31);
    let chevron = Rgb::new(137, 220, 235);

    let set_px = |buf: &mut [u8], x: usize, y: usize, c: Rgb, a: u8| {
        if x < SIZE && y < SIZE {
            let i = (y * SIZE + x) * 4;
            buf[i] = c.r;
            buf[i + 1] = c.g;
            buf[i + 2] = c.b;
            buf[i + 3] = a;
        }
    };

    // Rounded 32x32 card with 2px accent border
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = if x < 6 { 6 - x } else if x >= SIZE - 6 { x - (SIZE - 7) } else { 0 };
            let dy = if y < 6 { 6 - y } else if y >= SIZE - 6 { y - (SIZE - 7) } else { 0 };
            let dist_sq = dx * dx + dy * dy;
            if dist_sq <= 36 {
                let is_border = x < 2 || y < 2 || x >= SIZE - 2 || y >= SIZE - 2 || dist_sq >= 22;
                let color = if is_border { accent } else { bg };
                set_px(&mut rgba, x, y, color, 255);
            }
        }
    }

    // Crisp `>` chevron (left side)
    for step in 0..6 {
        for thick in 0..3 {
            set_px(&mut rgba, 7 + step + thick, 10 + step, chevron, 255);
            set_px(&mut rgba, 7 + step + thick, 21 - step, chevron, 255);
        }
    }

    // Crisp `_` prompt cursor bar (right side)
    for y in 19..22 {
        for x in 17..25 {
            set_px(&mut rgba, x, y, accent, 255);
        }
    }

    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_and_hit_testing_cover_sidebar_settings_suggestions_and_input() {
        let layout = Layout::compute(1120, 720);
        assert_eq!(layout.sidebar.w, SIDEBAR_WIDTH);
        assert_eq!(layout.viewport.x, SIDEBAR_WIDTH);
        assert_eq!(layout.input_bar.h, INPUT_BAR_HEIGHT);

        let row0 = layout.session_row_rect(0);
        assert_eq!(
            layout.hit_test(row0.x + 20, row0.y + 10, 2, 3, false, 0, false),
            HitTarget::CycleSessionColor(0)
        );
        assert_eq!(
            layout.hit_test(row0.x + 80, row0.y + 10, 2, 3, false, 0, false),
            HitTarget::SessionRow(0)
        );

        let close0 = layout.session_close_rect(0);
        assert_eq!(
            layout.hit_test(close0.x + 2, close0.y + 2, 2, 3, false, 0, false),
            HitTarget::CloseSession(0)
        );

        assert_eq!(
            layout.hit_test(
                layout.settings_btn.x + 10,
                layout.settings_btn.y + 10,
                2,
                3,
                false,
                0,
                false
            ),
            HitTarget::SettingsToggle
        );

        assert_eq!(
            layout.hit_test(
                layout.new_session_btn.x + 10,
                layout.new_session_btn.y + 10,
                2,
                3,
                false,
                0,
                false
            ),
            HitTarget::NewSessionToggle
        );

        let pick1 = layout.shell_picker_row(3, 1);
        assert_eq!(
            layout.hit_test(pick1.x + 10, pick1.y + 10, 2, 3, true, 0, false),
            HitTarget::PickShell(1)
        );

        let sug1 = layout.suggestion_row(3, 1);
        assert_eq!(
            layout.hit_test(sug1.x + 10, sug1.y + 10, 2, 3, false, 3, false),
            HitTarget::SuggestionRow(1)
        );

        // Settings modal hit targets
        let sw0 = layout.settings_swatch_rect(0);
        assert_eq!(
            layout.hit_test(sw0.x + 5, sw0.y + 5, 2, 3, false, 0, true),
            HitTarget::SettingsSwatch(0)
        );
        let accent_field = layout.settings_field_rect(SettingsField::AccentHex);
        assert_eq!(
            layout.hit_test(accent_field.x + 10, accent_field.y + 10, 2, 3, false, 0, true),
            HitTarget::SettingsField(SettingsField::AccentHex)
        );
        let size_up = layout.settings_size_up_rect();
        assert_eq!(
            layout.hit_test(size_up.x + 5, size_up.y + 5, 2, 3, false, 0, true),
            HitTarget::SettingsSizeUp
        );
        let check_upd = layout.settings_update_rect();
        assert_eq!(
            layout.hit_test(check_upd.x + 5, check_upd.y + 5, 2, 3, false, 0, true),
            HitTarget::SettingsCheckUpdate
        );
        let upd_pill = layout.update_pill_rect();
        assert!(upd_pill.x + upd_pill.w < layout.passthrough_pill.x);

        assert_eq!(
            layout.hit_test(
                layout.passthrough_pill.x + 10,
                layout.passthrough_pill.y + 5,
                2,
                3,
                false,
                0,
                false
            ),
            HitTarget::TogglePassthrough
        );

        assert_eq!(
            layout.hit_test(
                layout.input_bar.x + 20,
                layout.input_bar.y + 20,
                2,
                3,
                false,
                0,
                false
            ),
            HitTarget::InputBar
        );
    }

    #[test]
    fn canvas_hidpi_scales_logical_rects_to_physical_pixels() {
        let mut pixels = vec![0u32; 20 * 20];
        let mut canvas = Canvas::new_scaled(&mut pixels, 20, 20, 2.0);
        let color = Rgb::new(0x12, 0x34, 0x56);
        canvas.fill_rect(Rect::new(2, 3, 4, 2), color);

        let expected = color.to_u32();
        for y in 0..20 {
            for x in 0..20 {
                let inside = (4..12).contains(&x) && (6..10).contains(&y);
                assert_eq!(
                    pixels[y * 20 + x] == expected,
                    inside,
                    "unexpected pixel at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn canvas_blit_alpha_does_not_panic_when_buffer_is_smaller_than_dimensions() {
        let mut pixels = vec![0u32; 4]; // smaller than 10x10
        let mut canvas = Canvas::new_scaled(&mut pixels, 10, 10, 1.0);
        let alpha = vec![255u8; 4 * 4];
        canvas.blit_alpha(0, 0, 4, 4, &alpha, Rgb::new(0xFF, 0xFF, 0xFF), 10);
    }

    #[test]
    fn layout_supports_optional_acp_panel_and_toggle_hit_target() {
        let closed = Layout::compute_with_options(1120, 720, true, false);
        assert!(closed.acp_panel.is_none());
        assert_eq!(
            closed.hit_test(
                closed.acp_btn.x + 10,
                closed.acp_btn.y + 5,
                1,
                1,
                false,
                0,
                false
            ),
            HitTarget::AcpToggle
        );
        assert!(closed.passthrough_pill.x + closed.passthrough_pill.w < closed.acp_btn.x);
        assert!(closed.acp_btn.x + closed.acp_btn.w < closed.split_btn.x);

        let open = Layout::compute_with_options(1120, 720, true, true);
        let panel = open.acp_panel.expect("acp panel should exist when open");
        assert!(open.viewport.w < closed.viewport.w);
        assert_eq!(open.viewport.w + 1 + panel.w, closed.viewport.w);
        assert_eq!(open.input_bar.w, open.viewport.w);
        assert!(open.acp_header_rect().is_some());
        assert!(open.acp_close_rect().is_some());
        assert!(open.acp_agent_selector_rect().is_some());
        assert!(open.acp_messages_rect().is_some());
        assert!(open.acp_context_bar_rect().is_some());
        assert!(open.acp_auto_toggle_rect().is_some());
        assert!(open.acp_permission_card_rect().is_some());
        assert!(open.acp_permission_approve_rect().is_some());
        assert!(open.acp_permission_reject_rect().is_some());
        assert!(open.acp_input_rect().is_some());
        assert!(open.acp_send_rect().is_some());
        let ctx_r = open.acp_context_bar_rect().unwrap();
        let auto_r = open.acp_auto_toggle_rect().unwrap();
        let card_r = open.acp_permission_card_rect().unwrap();
        let app_r = open.acp_permission_approve_rect().unwrap();
        let rej_r = open.acp_permission_reject_rect().unwrap();
        assert!(ctx_r.x + ctx_r.w < auto_r.x);
        assert!(card_r.y + card_r.h <= ctx_r.y);
        assert!(app_r.x + app_r.w <= rej_r.x);

        let custom = Layout::compute_with_sizes(1200, 800, true, 300, true, Some(480));
        assert_eq!(custom.sidebar.w, 300);
        assert_eq!(custom.acp_panel.unwrap().w, 480);
        assert!(custom.sidebar_divider_rect().is_some());
        assert!(custom.acp_divider_rect().is_some());

        let clamped = Layout::compute_with_sizes(1000, 700, true, 900, true, Some(20));
        assert_eq!(clamped.sidebar.w, 500);
        assert_eq!(clamped.acp_panel.unwrap().w, 220);
    }
}

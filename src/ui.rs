use crate::input::TokenKind;
use crate::session::SessionManager;
use crate::shell::ShellProfile;
use crate::theme::{Rgb, Theme, UserIdentity};
use fontdue::{Font, FontSettings, Metrics};
use std::collections::HashMap;
use std::path::Path;

pub const SIDEBAR_WIDTH: u32 = 228;
pub const HEADER_HEIGHT: u32 = 38;
pub const INPUT_BAR_HEIGHT: u32 = 62;
pub const SESSION_ROW_HEIGHT: u32 = 36;
pub const PICKER_ROW_HEIGHT: u32 = 30;
pub const FONT_SIZE_TERM: f32 = 15.0;
pub const FONT_SIZE_UI: f32 = 13.5;
pub const FONT_SIZE_SMALL: f32 = 11.5;

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
pub enum HitTarget {
    SessionRow(usize),
    CloseSession(usize),
    NewSessionToggle,
    PickShell(usize),
    TogglePassthrough,
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
    pub new_session_btn: Rect,
    pub header: Rect,
    pub passthrough_pill: Rect,
    pub viewport: Rect,
    pub input_bar: Rect,
}

impl Layout {
    pub fn compute(width: u32, height: u32) -> Self {
        let sidebar_w = SIDEBAR_WIDTH.min(width / 2);
        let main_x = sidebar_w;
        let main_w = width.saturating_sub(sidebar_w);
        let header_h = HEADER_HEIGHT.min(height);
        let input_h = INPUT_BAR_HEIGHT.min(height.saturating_sub(header_h));
        let viewport_h = height.saturating_sub(header_h + input_h);

        let user_card = Rect::new(10, 10, sidebar_w.saturating_sub(20), 46);
        let sessions_top = user_card.y + user_card.h + 24;
        let new_btn_h = 34;
        let new_session_btn = Rect::new(
            12,
            height.saturating_sub(new_btn_h + 12),
            sidebar_w.saturating_sub(24),
            new_btn_h,
        );

        let pill_w = 138;
        let passthrough_pill = Rect::new(
            main_x + main_w.saturating_sub(pill_w + 12),
            7,
            pill_w,
            header_h.saturating_sub(14),
        );

        Self {
            width,
            height,
            sidebar: Rect::new(0, 0, sidebar_w, height),
            user_card,
            sessions_top,
            new_session_btn,
            header: Rect::new(main_x, 0, main_w, header_h),
            passthrough_pill,
            viewport: Rect::new(main_x, header_h, main_w, viewport_h),
            input_bar: Rect::new(main_x, header_h + viewport_h, main_w, input_h),
        }
    }

    pub fn session_row_rect(&self, index: usize) -> Rect {
        let y = self.sessions_top + (index as u32) * (SESSION_ROW_HEIGHT + 4);
        Rect::new(10, y, self.sidebar.w.saturating_sub(20), SESSION_ROW_HEIGHT)
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

    pub fn hit_test(
        &self,
        px: u32,
        py: u32,
        session_count: usize,
        shell_count: usize,
        picker_open: bool,
    ) -> HitTarget {
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

        if self.new_session_btn.contains(px, py) {
            return HitTarget::NewSessionToggle;
        }

        for i in 0..session_count {
            let row = self.session_row_rect(i);
            if row.y + row.h >= self.new_session_btn.y {
                break;
            }
            if self.session_close_rect(i).contains(px, py) {
                return HitTarget::CloseSession(i);
            }
            if row.contains(px, py) {
                return HitTarget::SessionRow(i);
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
}

/// Minimal glyph cache rasterizing characters on demand with `fontdue`.
pub struct FontRenderer {
    font: Font,
    cache: HashMap<(char, u16), (Metrics, Vec<u8>)>,
    pub cell_w: u32,
    pub cell_h: u32,
    pub baseline: i32,
}

impl FontRenderer {
    pub fn load_system_mono() -> Result<Self, String> {
        let bytes = load_system_mono_bytes()?;
        let font = Font::from_bytes(bytes, FontSettings::default())
            .map_err(|e| format!("failed to parse system monospace font: {e}"))?;

        let (m_metrics, _) = font.rasterize('M', FONT_SIZE_TERM);
        let cell_w = (m_metrics.advance_width.ceil() as u32).max(8);
        let (cell_h, baseline) = if let Some(lm) = font.horizontal_line_metrics(FONT_SIZE_TERM) {
            let h = (lm.new_line_size.ceil() as u32).max(16);
            let base = lm.ascent.ceil() as i32;
            (h, base)
        } else {
            (20, 15)
        };

        Ok(Self {
            font,
            cache: HashMap::with_capacity(128),
            cell_w,
            cell_h,
            baseline,
        })
    }

    fn glyph(&mut self, ch: char, px: f32) -> &(Metrics, Vec<u8>) {
        let key = (ch, (px * 10.0) as u16);
        self.cache.entry(key).or_insert_with(|| self.font.rasterize(ch, px))
    }

    pub fn measure_text(&mut self, text: &str, px: f32) -> u32 {
        let mut w = 0.0f32;
        for ch in text.chars() {
            let (m, _) = self.glyph(ch, px);
            w += m.advance_width;
        }
        w.ceil() as u32
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
        let ascent = self
            .font
            .horizontal_line_metrics(px)
            .map(|lm| lm.ascent.ceil() as i32)
            .unwrap_or((px * 0.8) as i32);
        let baseline_y = y + ascent;
        let mut pen_x = x as f32;

        for ch in text.chars() {
            if pen_x as i32 >= max_x {
                break;
            }
            let (metrics, bitmap) = self.glyph(ch, px);
            let gx = pen_x.round() as i32 + metrics.xmin;
            let gy = baseline_y - metrics.height as i32 - metrics.ymin;
            if !bitmap.is_empty() && ch != ' ' {
                canvas.blit_alpha(gx, gy, metrics.width, metrics.height, bitmap, color, max_x);
            }
            pen_x += metrics.advance_width;
        }
        pen_x.round() as i32
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
        let baseline_y = cell_y + self.baseline;
        let (metrics, bitmap) = self.glyph(ch, FONT_SIZE_TERM);
        if bitmap.is_empty() {
            return;
        }
        let gx = cell_x + metrics.xmin;
        let gy = baseline_y - metrics.height as i32 - metrics.ymin;
        canvas.blit_alpha(gx, gy, metrics.width, metrics.height, bitmap, color, max_x);
    }
}

fn load_system_mono_bytes() -> Result<Vec<u8>, String> {
    #[cfg(target_os = "windows")]
    let candidates: &[&str] = &[
        r"C:\Windows\Fonts\CascadiaMono.ttf",
        r"C:\Windows\Fonts\CascadiaCode.ttf",
        r"C:\Windows\Fonts\consola.ttf",
        r"C:\Windows\Fonts\lucon.ttf",
        r"C:\Windows\Fonts\cour.ttf",
    ];

    #[cfg(target_os = "macos")]
    let candidates: &[&str] = &[
        "/System/Library/Fonts/SFNSMono.ttf",
        "/System/Library/Fonts/Menlo.ttc",
        "/System/Library/Fonts/Monaco.ttf",
        "/Library/Fonts/Courier New.ttf",
    ];

    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let candidates: &[&str] = &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
    ];

    for path in candidates {
        if Path::new(path).is_file() {
            if let Ok(bytes) = std::fs::read(path) {
                return Ok(bytes);
            }
        }
    }
    Err("No system monospace font found on this machine".to_string())
}

/// Zero-dependency 2D software framebuffer wrapper over `softbuffer`'s `&mut [u32]`.
pub struct Canvas<'a> {
    pixels: &'a mut [u32],
    width: u32,
    height: u32,
}

impl<'a> Canvas<'a> {
    pub fn new(pixels: &'a mut [u32], width: u32, height: u32) -> Self {
        Self {
            pixels,
            width,
            height,
        }
    }

    pub fn clear(&mut self, color: Rgb) {
        self.pixels.fill(color.to_u32());
    }

    pub fn fill_rect(&mut self, r: Rect, color: Rgb) {
        let c = color.to_u32();
        let x0 = r.x.min(self.width);
        let y0 = r.y.min(self.height);
        let x1 = r.x.saturating_add(r.w).min(self.width);
        let y1 = r.y.saturating_add(r.h).min(self.height);
        for y in y0..y1 {
            let row_start = (y * self.width) as usize;
            self.pixels[row_start + x0 as usize..row_start + x1 as usize].fill(c);
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
                let a = alpha_map[src_row + col];
                if a == 0 {
                    continue;
                }
                let idx = dst_row + px as usize;
                self.pixels[idx] = color.blend_over(self.pixels[idx], a);
            }
        }
    }
}

pub struct UiState {
    pub picker_open: bool,
    pub hovered: HitTarget,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            picker_open: false,
            hovered: HitTarget::None,
        }
    }
}

/// Renders the entire terminal application into the 2D software framebuffer.
#[allow(clippy::too_many_arguments)]
pub fn render_app(
    pixels: &mut [u32],
    layout: &Layout,
    font: &mut FontRenderer,
    theme: &Theme,
    user: &UserIdentity,
    shells: &[ShellProfile],
    sessions: &SessionManager,
    ui: &UiState,
) {
    let mut canvas = Canvas::new(pixels, layout.width, layout.height);
    canvas.clear(theme.bg_terminal);

    // 1. Sidebar background + divider
    canvas.fill_rect(layout.sidebar, theme.bg_sidebar);
    canvas.fill_rect(
        Rect::new(layout.sidebar.w.saturating_sub(1), 0, 1, layout.height),
        theme.border,
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
        let is_hovered = matches!(ui.hovered, HitTarget::SessionRow(i) | HitTarget::CloseSession(i) if i == idx);
        let bg = if is_active {
            theme.bg_sidebar_active
        } else if is_hovered {
            theme.bg_sidebar_hover
        } else {
            theme.bg_sidebar
        };
        canvas.fill_rect(row, bg);
        if is_active {
            canvas.fill_rect(Rect::new(row.x, row.y, 3, row.h), sess.shell.accent);
        }

        // Shell badge pill
        let badge_rect = Rect::new(row.x + 9, row.y + 8, 36, 20);
        canvas.fill_rect(badge_rect, sess.shell.accent.lerp(theme.bg_sidebar, 190));
        font.draw_text(
            &mut canvas,
            (badge_rect.x + 4) as i32,
            (badge_rect.y + 4) as i32,
            FONT_SIZE_SMALL,
            sess.shell.accent,
            &sess.shell.badge,
            (badge_rect.x + badge_rect.w) as i32,
        );

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
            (row.x + row.w.saturating_sub(28)) as i32,
        );

        // Close button ('x')
        let close_r = layout.session_close_rect(idx);
        let close_color = if ui.hovered == HitTarget::CloseSession(idx) {
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

    // 5. "+ New Session" button
    let new_btn_bg = if ui.picker_open || ui.hovered == HitTarget::NewSessionToggle {
        user.accent.lerp(theme.bg_sidebar, 175)
    } else {
        theme.bg_sidebar_hover
    };
    canvas.fill_rect(layout.new_session_btn, new_btn_bg);
    canvas.stroke_rect(layout.new_session_btn, user.accent.lerp(theme.border, 130));
    font.draw_text(
        &mut canvas,
        (layout.new_session_btn.x + 14) as i32,
        (layout.new_session_btn.y + 9) as i32,
        FONT_SIZE_UI,
        theme.fg_primary,
        if ui.picker_open { "v Select Shell" } else { "+ New Session" },
        (layout.new_session_btn.x + layout.new_session_btn.w - 8) as i32,
    );

    // 6. Shell picker popup (when open)
    if ui.picker_open {
        let panel = layout.shell_picker_panel(shells.len());
        canvas.fill_rect(panel, theme.bg_header);
        canvas.stroke_rect(panel, user.accent);
        for (i, sh) in shells.iter().enumerate() {
            let r = layout.shell_picker_row(shells.len(), i);
            if ui.hovered == HitTarget::PickShell(i) {
                canvas.fill_rect(r, theme.bg_sidebar_active);
            }
            canvas.fill_rect(Rect::new(r.x + 4, r.y + 6, 4, r.h.saturating_sub(12)), sh.accent);
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

    // 7. Top header bar
    canvas.fill_rect(layout.header, theme.bg_header);
    canvas.fill_rect(
        Rect::new(layout.header.x, layout.header.h.saturating_sub(1), layout.header.w, 1),
        theme.border,
    );

    if let Some(active) = sessions.active() {
        // Accent stripe along top edge of header (user + session blend)
        canvas.fill_rect(
            Rect::new(layout.header.x, 0, layout.header.w, 2),
            active.shell.accent,
        );

        let badge = format!("[{}]", active.shell.badge);
        let x0 = (layout.header.x + 14) as i32;
        let next_x = font.draw_text(
            &mut canvas,
            x0,
            10,
            FONT_SIZE_UI,
            active.shell.accent,
            &badge,
            (layout.passthrough_pill.x.saturating_sub(10)) as i32,
        );
        let status = if active.alive { "" } else { " (exited)" };
        font.draw_text(
            &mut canvas,
            next_x + 8,
            10,
            FONT_SIZE_UI,
            theme.fg_primary,
            &format!("{}{}", active.title, status),
            (layout.passthrough_pill.x.saturating_sub(10)) as i32,
        );

        // Passthrough / Warp Input toggle pill
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
        let pill_label = if active.grid.is_alt_screen() {
            "TUI PASSTHROUGH"
        } else if active.input.passthrough {
            "PASSTHROUGH"
        } else {
            "WARP INPUT"
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

        // 8. Terminal grid viewport
        let start_x = layout.viewport.x as i32 + 8;
        let start_y = layout.viewport.y as i32 + 6;
        let max_x = (layout.viewport.x + layout.viewport.w) as i32;
        let cw = font.cell_w;
        let ch = font.cell_h;
        let (cur_r, cur_c) = active.grid.cursor();

        for row_idx in 0..active.grid.rows() {
            let cy = start_y + (row_idx as i32) * (ch as i32);
            if cy + ch as i32 > (layout.viewport.y + layout.viewport.h) as i32 {
                break;
            }
            if let Some(cells) = active.grid.visible_row(row_idx, active.scroll_offset) {
                for (col_idx, cell) in cells.iter().enumerate() {
                    let cx = start_x + (col_idx as i32) * (cw as i32);
                    if cx >= max_x {
                        break;
                    }
                    let (fg, bg) = if cell.inverse {
                        (cell.bg, cell.fg)
                    } else {
                        (cell.fg, cell.bg)
                    };
                    if bg != theme.bg_terminal {
                        canvas.fill_rect(Rect::new(cx as u32, cy as u32, cw, ch), bg);
                    }
                    font.draw_cell_char(&mut canvas, cx, cy, cell.ch, fg, max_x);
                    if cell.underline {
                        canvas.fill_rect(
                            Rect::new(cx as u32, (cy as u32) + ch.saturating_sub(2), cw, 1),
                            fg,
                        );
                    }
                }
            }
        }

        // Terminal cursor (shown when at live bottom and in passthrough mode)
        if active.scroll_offset == 0 && is_pt {
            let cx = start_x + (cur_c as i32) * (cw as i32);
            let cy = start_y + (cur_r as i32) * (ch as i32);
            if cx >= 0 && cy >= 0 && (cx as u32) + cw <= layout.viewport.x + layout.viewport.w {
                canvas.fill_rect(
                    Rect::new(cx as u32, cy as u32 + ch.saturating_sub(3), cw, 3),
                    user.accent,
                );
            }
        }

        // 9. Warp-style syntax-colored input bar
        canvas.fill_rect(layout.input_bar, theme.bg_input);
        canvas.fill_rect(
            Rect::new(layout.input_bar.x, layout.input_bar.y, layout.input_bar.w, 2),
            if is_pt { theme.border } else { user.accent },
        );

        let ib_max_x = (layout.input_bar.x + layout.input_bar.w - 12) as i32;
        if is_pt {
            font.draw_text(
                &mut canvas,
                (layout.input_bar.x + 14) as i32,
                (layout.input_bar.y + 14) as i32,
                FONT_SIZE_UI,
                theme.fg_muted,
                "Passthrough mode active — keystrokes go directly to the shell (Ctrl+Shift+P to return to Warp Input)",
                ib_max_x,
            );
        } else {
            let prompt = format!("{} > ", user.username);
            let input_y = (layout.input_bar.y + 10) as i32;
            let text_start_x = font.draw_text(
                &mut canvas,
                (layout.input_bar.x + 14) as i32,
                input_y,
                FONT_SIZE_TERM,
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
                    FONT_SIZE_TERM,
                    theme.fg_muted,
                    "Type a command (flags, paths, strings & vars highlight live)...",
                    ib_max_x,
                );
            } else {
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
                        FONT_SIZE_TERM,
                        color,
                        slice,
                        ib_max_x,
                    );
                }
            }

            // Caret in input bar
            let prefix_chars = active.input.cursor_char_index();
            let prefix_text: String = line.chars().take(prefix_chars).collect();
            let caret_offset = font.measure_text(&prefix_text, FONT_SIZE_TERM) as i32;
            let caret_x = (text_start_x + caret_offset).min(ib_max_x - 2);
            if caret_x >= 0 {
                canvas.fill_rect(
                    Rect::new(caret_x as u32, (input_y + 1) as u32, 2, font.cell_h),
                    user.accent,
                );
            }

            // Subtle bottom hint bar
            font.draw_text(
                &mut canvas,
                (layout.input_bar.x + 14) as i32,
                (layout.input_bar.y + layout.input_bar.h.saturating_sub(18)) as i32,
                FONT_SIZE_SMALL,
                theme.fg_muted,
                "Enter run  |  Up/Down history  |  Ctrl+T new  |  Ctrl+W close  |  Ctrl+Tab switch  |  Ctrl+Shift+P passthrough",
                ib_max_x,
            );
        }
    } else {
        font.draw_text(
            &mut canvas,
            (layout.viewport.x + 24) as i32,
            (layout.viewport.y + 24) as i32,
            FONT_SIZE_TERM,
            theme.fg_muted,
            "No active sessions. Click '+ New Session' or press Ctrl+T to open a shell.",
            (layout.viewport.x + layout.viewport.w - 16) as i32,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_and_hit_testing_cover_sidebar_header_viewport_and_input() {
        let layout = Layout::compute(1120, 720);
        assert_eq!(layout.sidebar.w, SIDEBAR_WIDTH);
        assert_eq!(layout.viewport.x, SIDEBAR_WIDTH);
        assert_eq!(layout.input_bar.h, INPUT_BAR_HEIGHT);

        let row0 = layout.session_row_rect(0);
        assert_eq!(
            layout.hit_test(row0.x + 20, row0.y + 10, 2, 3, false),
            HitTarget::SessionRow(0)
        );

        let close0 = layout.session_close_rect(0);
        assert_eq!(
            layout.hit_test(close0.x + 2, close0.y + 2, 2, 3, false),
            HitTarget::CloseSession(0)
        );

        assert_eq!(
            layout.hit_test(
                layout.new_session_btn.x + 10,
                layout.new_session_btn.y + 10,
                2,
                3,
                false
            ),
            HitTarget::NewSessionToggle
        );

        let pick1 = layout.shell_picker_row(3, 1);
        assert_eq!(
            layout.hit_test(pick1.x + 10, pick1.y + 10, 2, 3, true),
            HitTarget::PickShell(1)
        );

        assert_eq!(
            layout.hit_test(
                layout.passthrough_pill.x + 10,
                layout.passthrough_pill.y + 5,
                2,
                3,
                false
            ),
            HitTarget::TogglePassthrough
        );

        assert_eq!(
            layout.hit_test(layout.input_bar.x + 20, layout.input_bar.y + 20, 2, 3, false),
            HitTarget::InputBar
        );
    }
}

use my_warp::session::SessionManager;
use my_warp::shell::{default_shell, discover_shells, ShellProfile};
use my_warp::theme::{Theme, UserIdentity};
use my_warp::ui::{render_app, FontRenderer, HitTarget, Layout, UiState};
use softbuffer::{Context, Surface};
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

struct App {
    proxy: EventLoopProxy<()>,
    window: Option<Rc<Window>>,
    _context: Option<Context<Rc<Window>>>,
    surface: Option<Surface<Rc<Window>, Rc<Window>>>,
    font: FontRenderer,
    user: UserIdentity,
    theme: Theme,
    shells: Vec<ShellProfile>,
    sessions: SessionManager,
    ui: UiState,
    layout: Layout,
    modifiers: ModifiersState,
    cursor_pos: (u32, u32),
}

impl App {
    fn new(proxy: EventLoopProxy<()>) -> Result<Self, String> {
        let font = FontRenderer::load_system_mono()?;
        let user = UserIdentity::detect();
        let theme = Theme::for_user(&user);
        let shells = discover_shells();
        let layout = Layout::compute(1120, 720);
        let (cols, rows) = layout.terminal_grid_size(font.cell_w, font.cell_h);

        let wake_proxy = proxy.clone();
        let wakeup = Arc::new(move || {
            let _ = wake_proxy.send_event(());
        });

        let mut sessions =
            SessionManager::new(cols, rows, theme.fg_primary, theme.bg_terminal, Some(wakeup));
        let initial_shell = default_shell(&shells);
        let _ = sessions.spawn_session(initial_shell);

        Ok(Self {
            proxy,
            window: None,
            _context: None,
            surface: None,
            font,
            user,
            theme,
            shells,
            sessions,
            ui: UiState::default(),
            layout,
            modifiers: ModifiersState::empty(),
            cursor_pos: (0, 0),
        })
    }

    fn request_redraw(&self) {
        if let Some(ref w) = self.window {
            w.request_redraw();
        }
    }

    fn handle_resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.layout = Layout::compute(width, height);
        if let Some(ref mut surface) = self.surface {
            if let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) {
                let _ = surface.resize(w, h);
            }
        }
        let (cols, rows) = self
            .layout
            .terminal_grid_size(self.font.cell_w, self.font.cell_h);
        self.sessions.resize_all(cols, rows);
        self.request_redraw();
    }

    fn handle_click(&mut self) {
        let (px, py) = self.cursor_pos;
        let hit = self.layout.hit_test(
            px,
            py,
            self.sessions.sessions().len(),
            self.shells.len(),
            self.ui.picker_open,
        );
        match hit {
            HitTarget::NewSessionToggle => {
                self.ui.picker_open = !self.ui.picker_open;
            }
            HitTarget::PickShell(idx) => {
                self.ui.picker_open = false;
                if let Some(sh) = self.shells.get(idx).cloned() {
                    let _ = self.sessions.spawn_session(sh);
                }
            }
            HitTarget::SessionRow(idx) => {
                self.ui.picker_open = false;
                self.sessions.select(idx);
            }
            HitTarget::CloseSession(idx) => {
                self.ui.picker_open = false;
                self.sessions.close(idx);
            }
            HitTarget::TogglePassthrough => {
                self.ui.picker_open = false;
                if let Some(sess) = self.sessions.active_mut() {
                    sess.input.passthrough = !sess.input.passthrough;
                }
            }
            _ => {
                self.ui.picker_open = false;
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

    fn handle_key(&mut self, event: KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }

        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();
        let primary = self.primary_modifier();

        // Global shortcuts
        if primary {
            match &event.logical_key {
                Key::Character(s) if s.eq_ignore_ascii_case("t") => {
                    self.ui.picker_open = !self.ui.picker_open;
                    self.request_redraw();
                    return;
                }
                Key::Character(s) if s.eq_ignore_ascii_case("w") => {
                    self.sessions.close_active();
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
                    self.request_redraw();
                    return;
                }
                Key::Character(s) if s.len() == 1 => {
                    if let Some(digit) = s.chars().next().and_then(|c| c.to_digit(10)) {
                        if (1..=9).contains(&digit) {
                            self.sessions.select((digit - 1) as usize);
                            self.request_redraw();
                            return;
                        }
                    }
                }
                _ => {}
            }
        }

        // Ctrl+C always interrupts running process in active PTY
        if ctrl && !shift {
            if let Key::Character(s) = &event.logical_key {
                if s.eq_ignore_ascii_case("c") {
                    if let Some(sess) = self.sessions.active_mut() {
                        sess.write_bytes(&[0x03]);
                    }
                    self.request_redraw();
                    return;
                }
                if s.eq_ignore_ascii_case("u") {
                    if let Some(sess) = self.sessions.active_mut() {
                        if !sess.effective_passthrough() {
                            sess.input.clear();
                            self.request_redraw();
                            return;
                        }
                    }
                }
            }
        }

        let Some(sess) = self.sessions.active_mut() else {
            return;
        };

        // Passthrough / TUI alternate-screen mode: send keys directly to PTY
        if sess.effective_passthrough() {
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
            match &event.logical_key {
                Key::Named(NamedKey::Enter) => sess.write_bytes(b"\r"),
                Key::Named(NamedKey::Backspace) => sess.write_bytes(&[0x7f]),
                Key::Named(NamedKey::Tab) => sess.write_bytes(b"\t"),
                Key::Named(NamedKey::Escape) => sess.write_bytes(&[0x1b]),
                Key::Named(NamedKey::ArrowUp) => sess.write_bytes(b"\x1b[A"),
                Key::Named(NamedKey::ArrowDown) => sess.write_bytes(b"\x1b[B"),
                Key::Named(NamedKey::ArrowRight) => sess.write_bytes(b"\x1b[C"),
                Key::Named(NamedKey::ArrowLeft) => sess.write_bytes(b"\x1b[D"),
                Key::Named(NamedKey::Home) => sess.write_bytes(b"\x1b[H"),
                Key::Named(NamedKey::End) => sess.write_bytes(b"\x1b[F"),
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

        // Warp-style custom input bar mode
        match &event.logical_key {
            Key::Named(NamedKey::Enter) => {
                let cmd = sess.input.submit();
                sess.submit_command(&cmd);
            }
            Key::Named(NamedKey::Backspace) => sess.input.backspace(),
            Key::Named(NamedKey::Delete) => sess.input.delete(),
            Key::Named(NamedKey::ArrowLeft) => sess.input.move_left(),
            Key::Named(NamedKey::ArrowRight) => sess.input.move_right(),
            Key::Named(NamedKey::Home) => sess.input.move_home(),
            Key::Named(NamedKey::End) => sess.input.move_end(),
            Key::Named(NamedKey::ArrowUp) => sess.input.history_prev(),
            Key::Named(NamedKey::ArrowDown) => sess.input.history_next(),
            Key::Named(NamedKey::Escape) => {
                self.ui.picker_open = false;
                sess.input.clear();
            }
            Key::Named(NamedKey::Space) => sess.input.insert_char(' '),
            _ => {
                if !ctrl && !self.modifiers.alt_key() && !self.modifiers.super_key() {
                    if let Some(text) = &event.text {
                        for ch in text.chars() {
                            if !ch.is_control() {
                                sess.input.insert_char(ch);
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
        let Some(ref mut surface) = self.surface else {
            return;
        };
        if let Ok(mut buffer) = surface.buffer_mut() {
            render_app(
                &mut buffer,
                &self.layout,
                &mut self.font,
                &self.theme,
                &self.user,
                &self.shells,
                &self.sessions,
                &self.ui,
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
        let attrs = Window::default_attributes()
            .with_title(format!("my-warp — {}@{}", self.user.username, self.user.hostname))
            .with_inner_size(LogicalSize::new(1120.0, 720.0))
            .with_min_inner_size(LogicalSize::new(640.0, 400.0));

        let window = Rc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );
        let context = Context::new(window.clone()).expect("failed to create softbuffer context");
        let mut surface =
            Surface::new(&context, window.clone()).expect("failed to create softbuffer surface");

        let size = window.inner_size();
        if let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) {
            let _ = surface.resize(w, h);
            self.layout = Layout::compute(size.width, size.height);
            let (cols, rows) = self
                .layout
                .terminal_grid_size(self.font.cell_w, self.font.cell_h);
            self.sessions.resize_all(cols, rows);
        }

        window.request_redraw();
        self.window = Some(window);
        self._context = Some(context);
        self.surface = Some(surface);
        let _ = self.proxy.send_event(());
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        if self.sessions.poll_all() {
            self.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => self.handle_resize(size.width, size.height),
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x.max(0.0) as u32, position.y.max(0.0) as u32);
                let hit = self.layout.hit_test(
                    self.cursor_pos.0,
                    self.cursor_pos.1,
                    self.sessions.sessions().len(),
                    self.shells.len(),
                    self.ui.picker_open,
                );
                if hit != self.ui.hovered {
                    self.ui.hovered = hit;
                    self.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.handle_click(),
            WindowEvent::MouseWheel { delta, .. } => {
                let lines: i32 = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y * 3.0).round() as i32,
                    MouseScrollDelta::PixelDelta(pos) => (pos.y / 16.0).round() as i32,
                };
                if let Some(sess) = self.sessions.active_mut() {
                    if lines > 0 {
                        sess.scroll_up(lines as usize);
                    } else if lines < 0 {
                        sess.scroll_down((-lines) as usize);
                    }
                    self.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => self.handle_key(event),
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
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

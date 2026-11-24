//! Zero-dependency native clipboard access (Win32 CF_UNICODETEXT on Windows, pbcopy/pbpaste on macOS).

/// Copies UTF-8 text to the OS clipboard.
pub fn copy_text(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        win32_set_clipboard(text)
    }
    #[cfg(target_os = "macos")]
    {
        run_pipe_in("pbcopy", &[], text)
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        run_pipe_in("wl-copy", &[], text)
            || run_pipe_in("xclip", &["-selection", "clipboard"], text)
    }
}

/// Reads UTF-8 text from the OS clipboard.
pub fn paste_text() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        win32_get_clipboard()
    }
    #[cfg(target_os = "macos")]
    {
        run_stdout("pbpaste", &[])
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        run_stdout("wl-paste", &["--no-newline"])
            .or_else(|| run_stdout("xclip", &["-selection", "clipboard", "-o"]))
    }
}

#[cfg(not(target_os = "windows"))]
fn run_pipe_in(cmd: &str, args: &[&str], input: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let Ok(mut child) = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

#[cfg(not(target_os = "windows"))]
fn run_stdout(cmd: &str, args: &[&str]) -> Option<String> {
    use std::process::Command;
    let out = Command::new(cmd).args(args).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        None
    }
}

#[cfg(target_os = "windows")]
mod win32 {
    use std::ffi::c_void;

    pub const CF_UNICODETEXT: u32 = 13;
    pub const GMEM_MOVEABLE: u32 = 0x0002;

    #[link(name = "user32")]
    extern "system" {
        pub fn OpenClipboard(hwnd: *mut c_void) -> i32;
        pub fn CloseClipboard() -> i32;
        pub fn EmptyClipboard() -> i32;
        pub fn GetClipboardData(format: u32) -> *mut c_void;
        pub fn SetClipboardData(format: u32, hmem: *mut c_void) -> *mut c_void;
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
        pub fn GlobalLock(hmem: *mut c_void) -> *mut c_void;
        pub fn GlobalSize(hmem: *mut c_void) -> usize;
        pub fn GlobalUnlock(hmem: *mut c_void) -> i32;
        pub fn GlobalFree(hmem: *mut c_void) -> *mut c_void;
    }
}

#[cfg(target_os = "windows")]
fn win32_set_clipboard(text: &str) -> bool {
    use std::ptr;
    let clean: String = text.chars().filter(|&c| c != '\0').collect();
    let utf16: Vec<u16> = clean.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_len = utf16.len() * std::mem::size_of::<u16>();

    unsafe {
        if win32::OpenClipboard(ptr::null_mut()) == 0 {
            return false;
        }
        win32::EmptyClipboard();
        let hmem = win32::GlobalAlloc(win32::GMEM_MOVEABLE, byte_len);
        if hmem.is_null() {
            win32::CloseClipboard();
            return false;
        }
        let dst = win32::GlobalLock(hmem) as *mut u16;
        if dst.is_null() {
            win32::GlobalFree(hmem);
            win32::CloseClipboard();
            return false;
        }
        ptr::copy_nonoverlapping(utf16.as_ptr(), dst, utf16.len());
        win32::GlobalUnlock(hmem);
        let res = win32::SetClipboardData(win32::CF_UNICODETEXT, hmem);
        if res.is_null() {
            win32::GlobalFree(hmem);
            win32::CloseClipboard();
            return false;
        }
        win32::CloseClipboard();
        true
    }
}

#[cfg(target_os = "windows")]
fn win32_get_clipboard() -> Option<String> {
    use std::ptr;
    unsafe {
        if win32::OpenClipboard(ptr::null_mut()) == 0 {
            return None;
        }
        let hmem = win32::GetClipboardData(win32::CF_UNICODETEXT);
        if hmem.is_null() {
            win32::CloseClipboard();
            return None;
        }
        let src = win32::GlobalLock(hmem) as *const u16;
        if src.is_null() {
            win32::CloseClipboard();
            return None;
        }
        let max_units = win32::GlobalSize(hmem) / std::mem::size_of::<u16>();
        let mut len = 0usize;
        while len < max_units && *src.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(src, len);
        let text = String::from_utf16_lossy(slice);
        win32::GlobalUnlock(hmem);
        win32::CloseClipboard();
        Some(text)
    }
}

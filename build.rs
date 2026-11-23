use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let Some(rc_exe) = find_rc_exe() else {
        return;
    };

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let ico_path = out_dir.join("my-warp.ico");
    let rc_path = out_dir.join("my-warp.rc");
    let res_path = out_dir.join("my-warp.res");

    if fs::write(&ico_path, build_ico_bytes()).is_err() {
        return;
    }
    let rc_body = format!(
        "1 ICON \"{}\"\n",
        ico_path.to_string_lossy().replace('\\', "\\\\")
    );
    if fs::write(&rc_path, rc_body).is_err() {
        return;
    }

    let status = Command::new(rc_exe)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_path)
        .arg(&rc_path)
        .status();

    if let Ok(s) = status {
        if s.success() && res_path.is_file() {
            println!("cargo:rustc-link-arg-bin=roby-term={}", res_path.display());
        }
    }
}

fn find_rc_exe() -> Option<PathBuf> {
    if let Some(path_var) = env::var_os("PATH") {
        for dir in env::split_paths(&path_var) {
            let cand = dir.join("rc.exe");
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    let kits_bin = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut versions: Vec<PathBuf> = fs::read_dir(kits_bin)
        .ok()?
        .flatten()
        .map(|e| e.path().join("x64").join("rc.exe"))
        .filter(|p| p.is_file())
        .collect();
    versions.sort();
    versions.pop()
}

/// Builds a valid 32x32 32-bit BGRA Windows `.ico` file (`>_` inside a rounded dark frame).
fn build_ico_bytes() -> Vec<u8> {
    const W: usize = 32;
    const H: usize = 32;
    let bg = (18u8, 21u8, 31u8);
    let accent = (0xA8u8, 0x8Bu8, 0xD6u8);
    let chevron = (137u8, 220u8, 235u8);

    let mut rgba = vec![0u8; W * H * 4];
    let set_px = |buf: &mut [u8], x: usize, y: usize, c: (u8, u8, u8), a: u8| {
        if x < W && y < H {
            let i = (y * W + x) * 4;
            buf[i] = c.0;
            buf[i + 1] = c.1;
            buf[i + 2] = c.2;
            buf[i + 3] = a;
        }
    };

    for y in 0..H {
        for x in 0..W {
            let dx = if x < 6 { 6 - x } else if x >= W - 6 { x - (W - 7) } else { 0 };
            let dy = if y < 6 { 6 - y } else if y >= H - 6 { y - (H - 7) } else { 0 };
            let dist_sq = dx * dx + dy * dy;
            if dist_sq <= 36 {
                let is_border = x < 2 || y < 2 || x >= W - 2 || y >= H - 2 || dist_sq >= 22;
                set_px(&mut rgba, x, y, if is_border { accent } else { bg }, 255);
            }
        }
    }
    for step in 0..6 {
        for thick in 0..3 {
            set_px(&mut rgba, 7 + step + thick, 10 + step, chevron, 255);
            set_px(&mut rgba, 7 + step + thick, 21 - step, chevron, 255);
        }
    }
    for y in 19..22 {
        for x in 17..25 {
            set_px(&mut rgba, x, y, accent, 255);
        }
    }

    // ICO header (6 bytes) + 1 ICONDIRENTRY (16 bytes) + BITMAPINFOHEADER (40 bytes)
    // + XOR BGRA bottom-up pixels (32*32*4 = 4096) + AND mask (32*4 = 128)
    let dib_size: u32 = 40 + (W * H * 4) as u32 + (H * 4) as u32;
    let mut ico = Vec::with_capacity(22 + dib_size as usize);

    // ICONDIR
    ico.extend_from_slice(&0u16.to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes()); // Type 1 = ICO
    ico.extend_from_slice(&1u16.to_le_bytes()); // Count = 1

    // ICONDIRENTRY
    ico.push(W as u8);
    ico.push(H as u8);
    ico.push(0); // Color palette
    ico.push(0); // Reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // Color planes
    ico.extend_from_slice(&32u16.to_le_bytes()); // Bits per pixel
    ico.extend_from_slice(&dib_size.to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes()); // Offset to DIB

    // BITMAPINFOHEADER (40 bytes)
    ico.extend_from_slice(&40u32.to_le_bytes());
    ico.extend_from_slice(&(W as i32).to_le_bytes());
    ico.extend_from_slice(&((H * 2) as i32).to_le_bytes()); // Height * 2 for XOR + AND
    ico.extend_from_slice(&1u16.to_le_bytes()); // Planes
    ico.extend_from_slice(&32u16.to_le_bytes()); // BitCount
    ico.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    ico.extend_from_slice(&((W * H * 4) as u32).to_le_bytes());
    ico.extend_from_slice(&0i32.to_le_bytes());
    ico.extend_from_slice(&0i32.to_le_bytes());
    ico.extend_from_slice(&0u32.to_le_bytes());
    ico.extend_from_slice(&0u32.to_le_bytes());

    // Bottom-up BGRA pixels
    for y in (0..H).rev() {
        for x in 0..W {
            let i = (y * W + x) * 4;
            ico.push(rgba[i + 2]); // B
            ico.push(rgba[i + 1]); // G
            ico.push(rgba[i]);     // R
            ico.push(rgba[i + 3]); // A
        }
    }

    // 1bpp AND mask (32 bits = 4 bytes per row, 0 = use alpha)
    ico.resize(ico.len() + H * 4, 0);
    ico
}

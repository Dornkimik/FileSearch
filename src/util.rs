//! Small Windows helpers and formatting.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;

use chrono::{Local, TimeZone};
use eframe::egui::Color32;

use crate::search::{CATEGORIES, extension};

/// All fixed (internal) drives, e.g. `C:\`, `D:\`.
pub fn fixed_drives() -> Vec<PathBuf> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    const DRIVE_FIXED: u32 = 3;
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .filter_map(|i| {
            let root = format!("{}:\\", (b'A' + i) as char);
            let wide: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
            (unsafe { GetDriveTypeW(wide.as_ptr()) } == DRIVE_FIXED).then(|| PathBuf::from(root))
        })
        .collect()
}

pub fn open(path: &str) {
    let _ = open::that_detached(path);
}

/// Opens Explorer with the item selected.
pub fn reveal(path: &str) {
    let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{path}\"")).spawn();
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if v < 10.0 { format!("{v:.1} {}", UNITS[unit]) } else { format!("{v:.0} {}", UNITS[unit]) }
}

pub fn format_time(unix: u32) -> String {
    match Local.timestamp_opt(unix as i64, 0).single() {
        Some(t) => t.format("%Y-%m-%d  %H:%M").to_string(),
        None => String::new(),
    }
}

pub fn format_count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub const FOLDER_COLOR: Color32 = Color32::from_rgb(0xE8, 0xB3, 0x4B);

/// Icon tint for a file, based on its category.
pub fn file_color(name_lower: &str) -> Color32 {
    const COLORS: [Color32; 7] = [
        Color32::from_rgb(0x5B, 0x9B, 0xF0), // Documents
        Color32::from_rgb(0x4C, 0xC2, 0x8A), // Images
        Color32::from_rgb(0xE0, 0x6C, 0xB0), // Audio
        Color32::from_rgb(0xA7, 0x7B, 0xF3), // Video
        Color32::from_rgb(0xE8, 0x8A, 0x4B), // Archives
        Color32::from_rgb(0x3F, 0xC1, 0xC9), // Code
        Color32::from_rgb(0xEE, 0x6A, 0x6A), // Programs
    ];
    let ext = extension(name_lower);
    CATEGORIES
        .iter()
        .position(|cat| cat.exts.contains(&ext))
        .map_or(Color32::from_rgb(0x8C, 0x93, 0xA0), |i| COLORS[i])
}

/// A simple procedurally drawn app icon: a magnifier on an accent tile.
pub fn app_icon() -> eframe::egui::IconData {
    const N: usize = 64;
    let mut rgba = vec![0u8; N * N * 4];
    let (cx, cy, r) = (27.0f32, 27.0f32, 13.0f32);
    for y in 0..N {
        for x in 0..N {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // Rounded square tile.
            let (dx, dy) = ((fx - 32.0).abs() - 22.0, (fy - 32.0).abs() - 22.0);
            let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() - 10.0;
            if outside > 0.0 {
                continue;
            }
            let mut c = [0x5B, 0x6B, 0xF0, 255];
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let ring = (d - r).abs() < 3.2;
            // Handle along the diagonal from the ring towards the bottom right.
            let t = ((fx - cx) + (fy - cy)) / 2.0;
            let off = ((fx - cx) - (fy - cy)).abs() / std::f32::consts::SQRT_2;
            let handle = t > r * 0.7 && t < r + 12.0 && off < 3.6;
            if ring || handle {
                c = [255, 255, 255, 255];
            }
            rgba[(y * N + x) * 4..][..4].copy_from_slice(&c);
        }
    }
    eframe::egui::IconData { rgba, width: N as u32, height: N as u32 }
}

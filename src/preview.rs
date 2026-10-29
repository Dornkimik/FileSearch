//! Background loading of previews: images and GIFs are decoded directly,
//! videos (and formats we cannot decode) use the Windows shell thumbnail,
//! the same one Explorer shows.

use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use eframe::egui::ColorImage;
use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageReader, imageops};
use parking_lot::Mutex;

use crate::search::{CATEGORIES, extension};

/// Largest edge of a still image texture.
const MAX_STILL: u32 = 1600;
/// Largest edge of a GIF frame texture.
const MAX_GIF: u32 = 480;
const MAX_GIF_FRAMES: usize = 400;
/// Stop adding GIF frames once they use this much memory.
const GIF_BUDGET: usize = 160 * 1024 * 1024;
/// Bigger files skip full decoding and use the shell thumbnail instead.
const MAX_DECODE_BYTES: u64 = 256 * 1024 * 1024;
const THUMB_SIZE: i32 = 768;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Image,
    Gif,
    Video,
}

pub fn kind(name_lower: &str) -> Option<Kind> {
    let ext = extension(name_lower);
    if ext == "gif" {
        return Some(Kind::Gif);
    }
    let in_cat = |name: &str| CATEGORIES.iter().any(|c| c.name == name && c.exts.contains(&ext));
    if in_cat("Images") {
        Some(Kind::Image)
    } else if in_cat("Video") {
        Some(Kind::Video)
    } else {
        None
    }
}

pub struct Frame {
    pub image: ColorImage,
    pub delay: Duration,
}

pub struct Decoded {
    pub frames: Vec<Frame>,
    /// Original pixel size, when known.
    pub dims: Option<[u32; 2]>,
}

pub struct Loader {
    tx: crossbeam_channel::Sender<(u64, String, Kind)>,
    latest: Arc<AtomicU64>,
    pub result: Arc<Mutex<Option<(u64, Result<Decoded, String>)>>>,
}

impl Loader {
    pub fn new(notify: impl Fn() + Send + 'static) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<(u64, String, Kind)>();
        let latest = Arc::new(AtomicU64::new(0));
        let result = Arc::new(Mutex::new(None));
        let (latest2, result2) = (latest.clone(), result.clone());
        std::thread::Builder::new()
            .name("preview".into())
            .spawn(move || {
                // The shell thumbnail API is COM based.
                unsafe {
                    let _ = windows::Win32::System::Com::CoInitializeEx(
                        None,
                        windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
                    );
                }
                while let Ok(mut req) = rx.recv() {
                    // Skip previews the user has already moved past.
                    while let Ok(newer) = rx.try_recv() {
                        req = newer;
                    }
                    let (id, path, kind) = req;
                    let out = load(&path, kind);
                    if latest2.load(Ordering::SeqCst) == id {
                        *result2.lock() = Some((id, out));
                        notify();
                    }
                }
            })
            .expect("spawn preview loader");
        Self { tx, latest, result }
    }

    pub fn request(&self, id: u64, path: String, kind: Kind) {
        self.latest.store(id, Ordering::SeqCst);
        let _ = self.tx.send((id, path, kind));
    }
}

fn load(path: &str, kind: Kind) -> Result<Decoded, String> {
    let small = std::fs::metadata(path).map(|m| m.len() <= MAX_DECODE_BYTES).unwrap_or(false);
    let decoded = match kind {
        Kind::Gif if small => decode_gif(path).or_else(|_| decode_still(path)),
        Kind::Image if small => decode_still(path),
        _ => Err(String::new()),
    };
    decoded.or_else(|_| shell_thumbnail(path).map(|image| Decoded { frames: vec![Frame { image, delay: Duration::ZERO }], dims: None }))
}

fn to_color_image(img: &image::RgbaImage) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw())
}

fn decode_still(path: &str) -> Result<Decoded, String> {
    let err = |e: image::ImageError| e.to_string();
    let reader = ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
    let mut decoder = reader.into_decoder().map_err(err)?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).map_err(err)?;
    // Photos from phones and cameras are often stored sideways with a rotation tag.
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    let dims = [img.width(), img.height()];
    if img.width() > MAX_STILL || img.height() > MAX_STILL {
        img = img.thumbnail(MAX_STILL, MAX_STILL);
    }
    let image = to_color_image(&img.to_rgba8());
    Ok(Decoded { frames: vec![Frame { image, delay: Duration::ZERO }], dims: Some(dims) })
}

fn decode_gif(path: &str) -> Result<Decoded, String> {
    let file = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    let decoder = GifDecoder::new(file).map_err(|e| e.to_string())?;
    let (w, h) = decoder.dimensions();
    let scale = (MAX_GIF as f32 / w.max(h).max(1) as f32).min(1.0);
    let (tw, th) = (((w as f32 * scale) as u32).max(1), ((h as f32 * scale) as u32).max(1));

    let mut frames = Vec::new();
    let mut bytes = 0usize;
    for frame in decoder.into_frames().take(MAX_GIF_FRAMES) {
        let frame = frame.map_err(|e| e.to_string())?;
        let (num, den) = frame.delay().numer_denom_ms();
        let ms = num / den.max(1);
        // Browsers treat very short delays as 100 ms; do the same so GIFs play at the familiar speed.
        let delay = Duration::from_millis(if ms <= 10 { 100 } else { ms as u64 });
        let mut buf = frame.into_buffer();
        if scale < 1.0 {
            buf = imageops::thumbnail(&buf, tw, th);
        }
        bytes += buf.as_raw().len();
        frames.push(Frame { image: to_color_image(&buf), delay });
        if bytes > GIF_BUDGET {
            break;
        }
    }
    if frames.is_empty() {
        return Err("empty GIF".into());
    }
    Ok(Decoded { frames, dims: Some([w, h]) })
}

/// Asks Windows for the thumbnail Explorer would show (works for videos and HEIC, RAW, … when codecs exist).
fn shell_thumbnail(path: &str) -> Result<ColorImage, String> {
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject,
        GetDIBits, GetObjectW,
    };
    use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF, SIIGBF_BIGGERSIZEOK, SIIGBF_THUMBNAILONLY};
    use windows::core::HSTRING;

    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(&HSTRING::from(path), None).map_err(|e| e.message())?;
        let flags = SIIGBF(SIIGBF_BIGGERSIZEOK.0 | SIIGBF_THUMBNAILONLY.0);
        let hbmp = factory.GetImage(SIZE { cx: THUMB_SIZE, cy: THUMB_SIZE }, flags).map_err(|e| e.message())?;

        let mut bm = BITMAP::default();
        GetObjectW(hbmp.into(), size_of::<BITMAP>() as i32, Some(&mut bm as *mut BITMAP as *mut _));
        let (w, h) = (bm.bmWidth.max(0) as usize, bm.bmHeight.unsigned_abs() as usize);
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32), // top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; w * h * 4];
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(dc, hbmp, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
        let _ = DeleteDC(dc);
        let _ = DeleteObject(hbmp.into());
        if lines == 0 || w == 0 || h == 0 {
            return Err("No thumbnail available".into());
        }

        // BGRA → RGBA. Some thumbnails leave alpha at zero, meaning "opaque".
        let opaque = px.chunks_exact(4).all(|p| p[3] == 0);
        for p in px.chunks_exact_mut(4) {
            p.swap(0, 2);
            if opaque {
                p[3] = 255;
            }
        }
        Ok(if opaque {
            ColorImage::from_rgba_unmultiplied([w, h], &px)
        } else {
            ColorImage::from_rgba_premultiplied([w, h], &px)
        })
    }
}

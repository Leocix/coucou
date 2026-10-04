// "Ask about my screen": one capture of the monitor under the cursor, taken
// only when the user clicks the screen button in the chat, never in the
// background. The PNG goes straight to the chat provider and is not kept.

/// Longest side sent to the model. Bigger captures cost more tokens and the
/// providers downscale them anyway.
const MAX_SIDE: u32 = 1568;

pub struct Capture {
    pub png: Vec<u8>,
}

pub fn screen() -> Result<Capture, String> {
    let (width, height, rgb) = grab()?;
    from_rgb(width, height, rgb)
}

/// Downscales and encodes any RGB frame the same way as a screen capture.
pub fn from_rgb(width: u32, height: u32, rgb: Vec<u8>) -> Result<Capture, String> {
    let (width, height, rgb) = downscale(width, height, rgb, MAX_SIDE);
    Ok(Capture { png: encode_png(width, height, &rgb)? })
}

/// The image a chat message carries, if its context asks for one: a fresh
/// screen capture, or the area / clipboard image waiting in `clip`.
pub fn image_for(context: Option<&crate::claude::ChatContext>) -> Result<Option<Vec<u8>>, String> {
    use crate::claude::ChatContext;
    match context {
        Some(ChatContext::Screen) => Ok(Some(screen()?.png)),
        Some(ChatContext::Image) => crate::clip::take_pending()
            .map(Some)
            .ok_or_else(|| "The image is gone. Capture it again.".to_string()),
        _ => Ok(None),
    }
}

fn encode_png(width: u32, height: u32, rgb: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(rgb).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Box filter: averages every source pixel that falls into a target pixel, so
/// text stays readable instead of the aliasing a nearest-neighbour pick gives.
fn downscale(width: u32, height: u32, rgb: Vec<u8>, max_side: u32) -> (u32, u32, Vec<u8>) {
    let longest = width.max(height);
    if longest <= max_side || width == 0 || height == 0 {
        return (width, height, rgb);
    }
    let new_w = ((width as u64 * max_side as u64) / longest as u64).max(1) as u32;
    let new_h = ((height as u64 * max_side as u64) / longest as u64).max(1) as u32;
    let mut out = vec![0u8; (new_w * new_h * 3) as usize];
    for ty in 0..new_h {
        let y0 = (ty as u64 * height as u64 / new_h as u64) as u32;
        let y1 = (((ty + 1) as u64 * height as u64 / new_h as u64) as u32).max(y0 + 1);
        for tx in 0..new_w {
            let x0 = (tx as u64 * width as u64 / new_w as u64) as u32;
            let x1 = (((tx + 1) as u64 * width as u64 / new_w as u64) as u32).max(x0 + 1);
            let mut sum = [0u32; 3];
            for y in y0..y1 {
                let row = (y * width) as usize * 3;
                for x in x0..x1 {
                    let i = row + x as usize * 3;
                    sum[0] += rgb[i] as u32;
                    sum[1] += rgb[i + 1] as u32;
                    sum[2] += rgb[i + 2] as u32;
                }
            }
            let n = (y1 - y0) * (x1 - x0);
            let o = ((ty * new_w + tx) * 3) as usize;
            out[o] = (sum[0] / n) as u8;
            out[o + 1] = (sum[1] / n) as u8;
            out[o + 2] = (sum[2] / n) as u8;
        }
    }
    (new_w, new_h, out)
}

#[cfg(windows)]
fn grab() -> Result<(u32, u32, Vec<u8>), String> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, GetMonitorInfoW, MonitorFromPoint, ReleaseDC, SelectObject, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, MONITORINFO,
        MONITOR_DEFAULTTOPRIMARY, SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    unsafe {
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return Err("Could not find the screen to capture.".into());
        }
        let rect = info.rcMonitor;
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        if w <= 0 || h <= 0 {
            return Err("Could not find the screen to capture.".into());
        }

        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, w, h);
        let previous = SelectObject(mem, bitmap.into());
        let blit = BitBlt(mem, 0, 0, w, h, Some(screen), rect.left, rect.top, SRCCOPY | CAPTUREBLT);

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        let lines = if blit.is_ok() {
            GetDIBits(
                mem,
                bitmap,
                0,
                h as u32,
                Some(bgra.as_mut_ptr().cast()),
                &mut bmi,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };

        SelectObject(mem, previous);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);

        if lines == 0 {
            return Err("Screen capture failed.".into());
        }
        let rgb = bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect();
        Ok((w as u32, h as u32, rgb))
    }
}

#[cfg(not(windows))]
fn grab() -> Result<(u32, u32, Vec<u8>), String> {
    Err("Screen capture is only available on Windows for now.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_keeps_small_images() {
        let (w, h, px) = downscale(4, 2, vec![7; 24], 10);
        assert_eq!((w, h, px.len()), (4, 2, 24));
    }

    #[test]
    fn downscale_averages_and_keeps_aspect() {
        // 4×2 image: left half black, right half white → 2×1 black, white.
        let mut px = Vec::new();
        for _ in 0..2 {
            px.extend_from_slice(&[0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 255]);
        }
        let (w, h, out) = downscale(4, 2, px, 2);
        assert_eq!((w, h), (2, 1));
        assert_eq!(out, vec![0, 0, 0, 255, 255, 255]);
    }

    #[test]
    fn png_has_signature() {
        let png = encode_png(1, 1, &[1, 2, 3]).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }
}

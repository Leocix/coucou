// Clipboard for the chat: "Use clipboard" reads text or an image, and the area
// capture opens the Windows snipping overlay (Win+Shift+S) and waits for the
// snip to land on the clipboard. Only ever read on the user's click.
//
// Images wait here, already encoded, until the next message takes them, so the
// pixels never cross into the webview.

use std::sync::Mutex;

use serde::Serialize;

/// Text longer than this is cut: it all goes into one message.
const MAX_TEXT_CHARS: usize = 50_000;

static PENDING: Mutex<Option<Vec<u8>>> = Mutex::new(None);

pub fn take_pending() -> Option<Vec<u8>> {
    PENDING.lock().unwrap().take()
}

fn set_pending(png: Vec<u8>) {
    *PENDING.lock().unwrap() = Some(png);
}

pub fn clear_pending() {
    PENDING.lock().unwrap().take();
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ClipContent {
    Text { text: String, truncated: bool },
    Image { width: u32, height: u32 },
}

/// Whatever is on the clipboard now: an image wins over text.
pub fn read() -> Result<ClipContent, String> {
    let mut board = arboard::Clipboard::new().map_err(|e| format!("Clipboard unavailable: {e}"))?;
    if let Ok(image) = board.get_image() {
        return store_image(image);
    }
    let text = board.get_text().map_err(|_| "The clipboard is empty.".to_string())?;
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("The clipboard is empty.".into());
    }
    let truncated = text.chars().count() > MAX_TEXT_CHARS;
    let text = if truncated { text.chars().take(MAX_TEXT_CHARS).collect() } else { text };
    Ok(ClipContent::Text { text, truncated })
}

fn store_image(image: arboard::ImageData) -> Result<ClipContent, String> {
    let (width, height) = (image.width as u32, image.height as u32);
    let rgb: Vec<u8> = image.bytes.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let shot = crate::capture::from_rgb(width, height, rgb)?;
    set_pending(shot.png);
    Ok(ClipContent::Image { width, height })
}

/// Opens the snipping overlay and waits for the snip. Gives up after a minute,
/// or as soon as something that is not an image is copied instead.
#[cfg(windows)]
pub async fn capture_area() -> Result<ClipContent, String> {
    use windows::Win32::System::DataExchange::GetClipboardSequenceNumber;

    let before = unsafe { GetClipboardSequenceNumber() };
    crate::platform::open_url("ms-screenclip:");

    for _ in 0..240 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if unsafe { GetClipboardSequenceNumber() } == before {
            continue;
        }
        // The snip tool may announce the change a moment before the bitmap
        // is readable: give it a few tries.
        for _ in 0..8 {
            if let Ok(image) = arboard::Clipboard::new().and_then(|mut b| b.get_image()) {
                return store_image(image);
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        return Err("No image was captured.".into());
    }
    Err("No area was captured.".into())
}

#[cfg(not(windows))]
pub async fn capture_area() -> Result<ClipContent, String> {
    Err("Area capture is only available on Windows for now.".into())
}

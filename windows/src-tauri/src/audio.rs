// Dictation: records the default microphone here in Rust (waveIn, 16 kHz
// mono) rather than in the webview, so there is no browser permission prompt
// hidden inside the island. The recording only lives in memory until Gemini
// has transcribed it; nothing is written to disk.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

const RATE: u32 = 16_000;
/// A dictation that runs this long stops by itself.
#[cfg_attr(not(windows), allow(dead_code))]
const MAX_SECS: usize = 120;

struct Recording {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Result<Vec<i16>, String>>,
}

static CURRENT: Mutex<Option<Recording>> = Mutex::new(None);

/// Starts recording. Returns once the microphone is open, so a missing or
/// blocked one is reported straight away.
pub fn start() -> Result<(), String> {
    let mut current = CURRENT.lock().unwrap();
    if current.is_some() {
        return Ok(());
    }
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let flag = stop.clone();
    let thread = std::thread::spawn(move || record(flag, ready_tx));
    match ready_rx.recv() {
        Ok(Ok(())) => {
            *current = Some(Recording { stop, thread });
            Ok(())
        }
        Ok(Err(err)) => {
            let _ = thread.join();
            Err(err)
        }
        Err(_) => Err("The microphone could not be opened.".into()),
    }
}

/// Stops recording and returns the audio as a WAV file.
pub fn stop() -> Result<Vec<u8>, String> {
    let recording = CURRENT.lock().unwrap().take().ok_or("Not recording.")?;
    recording.stop.store(true, Ordering::Relaxed);
    let samples = recording
        .thread
        .join()
        .map_err(|_| "The recording failed.".to_string())??;
    if samples.len() < RATE as usize / 2 {
        return Err("That was too short. Hold on a little longer.".into());
    }
    Ok(wav(&samples))
}

/// Drops a recording nobody wants any more.
pub fn cancel() {
    if let Some(recording) = CURRENT.lock().unwrap().take() {
        recording.stop.store(true, Ordering::Relaxed);
        let _ = recording.thread.join();
    }
}

/// 16-bit mono PCM in a RIFF/WAVE container, which Gemini takes as is.
fn wav(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(windows)]
fn record(stop: Arc<AtomicBool>, ready: Sender<Result<(), String>>) -> Result<Vec<i16>, String> {
    use std::ptr::addr_of;
    use std::time::Duration;
    use windows::core::PSTR;
    use windows::Win32::Media::Audio::{
        waveInAddBuffer, waveInClose, waveInOpen, waveInPrepareHeader, waveInReset, waveInStart,
        waveInUnprepareHeader, CALLBACK_NULL, HWAVEIN, WAVEFORMATEX, WAVEHDR, WAVE_FORMAT_PCM,
        WAVE_MAPPER, WHDR_DONE,
    };

    const BUFFERS: usize = 4;
    const BUF_BYTES: usize = RATE as usize / 10 * 2; // 100 ms each

    let format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: 1,
        nSamplesPerSec: RATE,
        nAvgBytesPerSec: RATE * 2,
        nBlockAlign: 2,
        wBitsPerSample: 16,
        cbSize: 0,
    };
    let mut device = HWAVEIN::default();
    let rc = unsafe { waveInOpen(Some(&mut device), WAVE_MAPPER, &format, None, None, CALLBACK_NULL) };
    if rc != 0 {
        let err = "No microphone available. Check Windows Settings > Privacy > Microphone, \
                   and that desktop apps may use it."
            .to_string();
        let _ = ready.send(Err(err.clone()));
        return Err(err);
    }

    // The driver writes into these behind our back, so after handing them over
    // they are only touched through raw pointers.
    let mut data = vec![vec![0u8; BUF_BYTES]; BUFFERS];
    let mut headers: Vec<WAVEHDR> = data
        .iter_mut()
        .map(|d| WAVEHDR {
            lpData: PSTR(d.as_mut_ptr()),
            dwBufferLength: BUF_BYTES as u32,
            ..Default::default()
        })
        .collect();
    let headers = headers.as_mut_ptr();
    let size = std::mem::size_of::<WAVEHDR>() as u32;
    unsafe {
        for i in 0..BUFFERS {
            waveInPrepareHeader(device, headers.add(i), size);
            waveInAddBuffer(device, headers.add(i), size);
        }
        waveInStart(device);
    }
    let _ = ready.send(Ok(()));

    let mut samples: Vec<i16> = Vec::new();
    let max = RATE as usize * MAX_SECS;
    let mut next = 0;
    // Buffers come back in the order they were queued.
    let mut drain = |samples: &mut Vec<i16>, requeue: bool| loop {
        let header = unsafe { headers.add(next) };
        let flags = unsafe { addr_of!((*header).dwFlags).read_unaligned() };
        if flags & WHDR_DONE == 0 {
            break;
        }
        let bytes = unsafe { addr_of!((*header).dwBytesRecorded).read_unaligned() } as usize;
        let start = unsafe { addr_of!((*header).lpData).read_unaligned() }.0;
        let chunk = unsafe { std::slice::from_raw_parts(start, bytes.min(BUF_BYTES)) };
        samples.extend(chunk.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
        unsafe {
            if requeue {
                waveInAddBuffer(device, header, size);
            } else {
                // Clear "done" so the drain stops once every buffer is read.
                (*header).dwFlags = flags & !WHDR_DONE;
            }
        }
        next = (next + 1) % BUFFERS;
        if !requeue && next == 0 {
            break;
        }
    };

    while !stop.load(Ordering::Relaxed) && samples.len() < max {
        std::thread::sleep(Duration::from_millis(30));
        drain(&mut samples, true);
    }
    unsafe {
        // Reset hands back every queued buffer, marked done.
        waveInReset(device);
    }
    drain(&mut samples, false);
    unsafe {
        for i in 0..BUFFERS {
            waveInUnprepareHeader(device, headers.add(i), size);
        }
        waveInClose(device);
    }
    drop(data); // only now may the buffers go
    Ok(samples)
}

#[cfg(not(windows))]
fn record(_stop: Arc<AtomicBool>, ready: Sender<Result<(), String>>) -> Result<Vec<i16>, String> {
    let err = "Dictation is Windows only for now.".to_string();
    let _ = ready.send(Err(err.clone()));
    Err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_matches_the_samples() {
        let out = wav(&[0, 1, -1]);
        assert_eq!(&out[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()), 36 + 6);
        assert_eq!(&out[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(out[24..28].try_into().unwrap()), RATE);
        assert_eq!(u32::from_le_bytes(out[40..44].try_into().unwrap()), 6);
        assert_eq!(&out[44..], &[0, 0, 1, 0, 0xff, 0xff]);
    }
}

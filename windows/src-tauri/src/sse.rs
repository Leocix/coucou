// Server-sent events, read off a streamed HTTP body. Both chat providers
// stream their answers this way so the island can show the text as it comes.

use std::time::Duration;

/// Splits a byte stream into `(event, data)` pairs. Chunks can end anywhere,
/// even inside a UTF-8 character, so bytes wait in `buf` until a line is whole.
#[derive(Default)]
pub struct Reader {
    buf: Vec<u8>,
    event: String,
    data: String,
}

impl Reader {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<(String, String)> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(end) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                // A blank line ends the event.
                if !self.data.is_empty() {
                    out.push((std::mem::take(&mut self.event), std::mem::take(&mut self.data)));
                }
                self.event.clear();
            } else if let Some(value) = line.strip_prefix("event:") {
                self.event = value.trim_start().to_string();
            } else if let Some(value) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(value.strip_prefix(' ').unwrap_or(value));
            }
            // Comments (":") and other fields are ignored.
        }
        out
    }
}

/// No overall timeout: a long answer can take minutes. A stream that goes
/// silent for a minute is dead, though.
pub fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_events_across_chunks() {
        let mut r = Reader::default();
        assert!(r.feed(b"event: ping\nda").is_empty());
        let got = r.feed(b"ta: {\"a\":1}\r\n\r\ndata: x\n\n");
        assert_eq!(
            got,
            vec![("ping".into(), "{\"a\":1}".into()), (String::new(), "x".into())]
        );
    }

    #[test]
    fn keeps_utf8_split_between_chunks() {
        let mut r = Reader::default();
        let bytes = "data: ¡hola!\n\n".as_bytes();
        assert!(r.feed(&bytes[..7]).is_empty()); // ends inside "¡"
        assert_eq!(r.feed(&bytes[7..]), vec![(String::new(), "¡hola!".into())]);
    }
}

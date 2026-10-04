// Claude API client — the same integration as ClaudeService.swift: multi-turn
// chat with web search, and files sent as document/image/text blocks.
//
// Everything happens here rather than in the island: the API key never leaves
// the Credential Manager, and file bytes never cross the IPC boundary.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::secrets;

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Server-side fallback: on a policy decline the API retries the same request on
/// a fallback model inside the same call, so the island never shows a dead end.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const MAX_TOKENS: u32 = 4096;
/// Text and code files are inlined; anything larger is skipped, as on macOS.
const MAX_INLINE_TEXT: u64 = 200_000;

pub const DEFAULT_MODEL: &str = "claude-opus-5";

pub(crate) const SYSTEM_PROMPT: &str = "You are Mochi, a personal AI assistant living at the top of the user's screen. \
You have web search access and can help with absolutely anything — research, coding, finding places, recommendations, tasks, questions. \
Respond in the user's language. Be thorough and complete — use as much detail as the task requires. \
No markdown formatting (no **, no ##, no bullet dashes). Use plain text with line breaks.";

#[derive(Default)]
pub struct Chat {
    /// Full multi-turn history, including tool_use / tool_result blocks.
    messages: Mutex<Vec<Value>>,
}

impl Chat {
    pub fn reset(&self) {
        self.messages.lock().unwrap().clear();
    }

    fn is_empty(&self) -> bool {
        self.messages.lock().unwrap().is_empty()
    }

    fn push(&self, message: Value) {
        self.messages.lock().unwrap().push(message);
    }

    fn pop(&self) {
        self.messages.lock().unwrap().pop();
    }

    fn snapshot(&self) -> Vec<Value> {
        self.messages.lock().unwrap().clone()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChatContext {
    File { name: String, path: String },
    Window { app_name: String, title: String, url: Option<String> },
    /// A capture of the screen, taken when the message is sent.
    Screen,
    /// The area capture or clipboard image waiting in `clip`.
    Image,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReply {
    pub text: String,
}

/// One chat turn, streamed: `on_text` gets each piece of the answer as it
/// arrives. Returns the whole text, or a message the island shows in the note view.
pub async fn send(
    chat: &Chat,
    model: &str,
    query: String,
    context: Option<ChatContext>,
    on_text: impl Fn(&str) + Send + Sync,
) -> Result<ChatReply, String> {
    let key = secrets::get("anthropic-api-key")
        .ok_or_else(|| "API key missing. Open settings.".to_string())?;

    let mut content: Vec<Value> = Vec::new();

    // File / window context rides along with the first message only, exactly
    // like ClaudeService.chat().
    if chat.is_empty() {
        match &context {
            Some(ChatContext::File { name, path }) => {
                if let Some(block) = file_block(path) {
                    content.push(block);
                }
                content.push(json!({ "type": "text", "text": format!("File: {name}") }));
            }
            Some(ChatContext::Window { app_name, title, url }) => {
                let mut text = format!("Context — App: {app_name}, Window: {title}");
                if let Some(url) = url {
                    text.push_str(&format!(", URL: {url}"));
                }
                content.push(json!({ "type": "text", "text": text }));
            }
            Some(ChatContext::Screen | ChatContext::Image) | None => {}
        }
    }
    // Unlike files, images can ride along with any turn: the user asks for one
    // each time with the screen, area or clipboard button.
    if let Some(png) = crate::capture::image_for(context.as_ref())? {
        content.push(json!({
            "type": "image",
            "source": { "type": "base64", "media_type": "image/png", "data": base64(&png) },
        }));
        let note = if matches!(context, Some(ChatContext::Screen)) {
            "Screenshot of my screen, taken just now."
        } else {
            "Image I captured or copied for this question."
        };
        content.push(json!({ "type": "text", "text": note }));
    }
    content.push(json!({ "type": "text", "text": query }));

    chat.push(json!({ "role": "user", "content": content }));

    let body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": SYSTEM_PROMPT,
        "tools": [{ "type": "web_search_20260209", "name": "web_search", "max_uses": 5 }],
        "fallbacks": "default",
        "stream": true,
        "messages": chat.snapshot(),
    });

    let message = match stream(&key, &body, &on_text).await {
        Ok(m) => m,
        Err(err) => {
            chat.pop(); // keep the history consistent with what the model saw
            return Err(err);
        }
    };

    // A policy decline ends the stream with stop_reason "refusal". Whatever
    // text came before it is dropped by the island.
    if message.stop_reason.as_deref() == Some("refusal") {
        chat.pop();
        let why = message
            .stop_details
            .as_ref()
            .and_then(|d| d.get("explanation"))
            .and_then(Value::as_str)
            .unwrap_or("Claude declined this one.");
        return Err(why.to_string());
    }

    let blocks = message.blocks;
    let text = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    if text.is_empty() {
        chat.pop();
        return Err("No response text.".into());
    }

    // Store the content — server tool blocks included — so the next turn has
    // the right context.
    chat.push(json!({ "role": "assistant", "content": history_blocks(blocks) }));
    Ok(ChatReply { text })
}

/// The message as it is rebuilt from the stream.
#[derive(Default)]
struct Message {
    blocks: Vec<Value>,
    /// Partial JSON of tool inputs, by block index, parsed when the block ends.
    inputs: Vec<(usize, String)>,
    stop_reason: Option<String>,
    stop_details: Option<Value>,
}

impl Message {
    /// Applies one event. Returns the text to show, if the event carries some.
    fn apply(&mut self, event: &str, data: &str) -> Result<Option<String>, String> {
        let Ok(v) = serde_json::from_str::<Value>(data) else { return Ok(None) };
        let index = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        match event {
            "content_block_start" => {
                let block = v.get("content_block").cloned().unwrap_or(Value::Null);
                if self.blocks.len() <= index {
                    self.blocks.resize(index + 1, Value::Null);
                }
                self.blocks[index] = block;
            }
            "content_block_delta" => {
                let delta = v.get("delta").cloned().unwrap_or(Value::Null);
                let Some(block) = self.blocks.get_mut(index) else { return Ok(None) };
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let piece = delta.get("text").and_then(Value::as_str).unwrap_or("");
                        let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                        block["text"] = Value::String(format!("{text}{piece}"));
                        return Ok(Some(piece.to_string()));
                    }
                    Some("citations_delta") => {
                        if let Some(citation) = delta.get("citation") {
                            if !block.get("citations").is_some_and(Value::is_array) {
                                block["citations"] = json!([]);
                            }
                            block["citations"].as_array_mut().unwrap().push(citation.clone());
                        }
                    }
                    Some("input_json_delta") => {
                        let piece = delta.get("partial_json").and_then(Value::as_str).unwrap_or("");
                        match self.inputs.iter_mut().find(|(i, _)| *i == index) {
                            Some((_, json)) => json.push_str(piece),
                            None => self.inputs.push((index, piece.to_string())),
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some(pos) = self.inputs.iter().position(|(i, _)| *i == index) {
                    let (_, json) = self.inputs.remove(pos);
                    if let (Some(block), Ok(input)) =
                        (self.blocks.get_mut(index), serde_json::from_str::<Value>(&json))
                    {
                        block["input"] = input;
                    }
                }
            }
            "message_delta" => {
                let delta = v.get("delta");
                if let Some(reason) = delta.and_then(|d| d.get("stop_reason")).and_then(Value::as_str) {
                    self.stop_reason = Some(reason.to_string());
                }
                if let Some(details) = delta.and_then(|d| d.get("stop_details")) {
                    if !details.is_null() {
                        self.stop_details = Some(details.clone());
                    }
                }
            }
            "error" => {
                let detail = v
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("The answer stopped.");
                return Err(format!("Claude API: {detail}"));
            }
            _ => {}
        }
        Ok(None)
    }
}

/// After a fallback mid-answer, only text and paired server tool blocks from
/// before the switch may be sent back; the fallback marker itself is dropped.
fn history_blocks(blocks: Vec<Value>) -> Vec<Value> {
    let kind = |b: &Value| b.get("type").and_then(Value::as_str).unwrap_or("").to_string();
    let last_fallback = blocks.iter().rposition(|b| kind(b) == "fallback");
    let result_ids: Vec<String> = blocks
        .iter()
        .filter(|b| kind(b).ends_with("_tool_result"))
        .filter_map(|b| b.get("tool_use_id").and_then(Value::as_str).map(str::to_string))
        .collect();
    blocks
        .into_iter()
        .enumerate()
        .filter(|(i, b)| {
            let k = kind(b);
            if b.is_null() || k == "fallback" {
                return false;
            }
            if last_fallback.is_some_and(|f| *i < f) {
                return match k.as_str() {
                    "text" => true,
                    "server_tool_use" => b
                        .get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| result_ids.iter().any(|r| r == id)),
                    _ => k.ends_with("_tool_result"),
                };
            }
            true
        })
        .map(|(_, b)| b)
        .collect()
}

async fn stream(key: &str, body: &Value, on_text: &(impl Fn(&str) + Sync)) -> Result<Message, String> {
    let mut response = crate::sse::client()?
        .post(ENDPOINT)
        .header("x-api-key", key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("anthropic-beta", FALLBACK_BETA)
        .header("content-type", "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.map_err(|e| e.to_string())?;
        // Surface the API's own message, which is what makes a bad key obvious.
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| text.chars().take(200).collect());
        return Err(format!("Claude API {status}: {detail}"));
    }

    let mut reader = crate::sse::Reader::default();
    let mut message = Message::default();
    let mut done = false;
    while let Some(chunk) = response.chunk().await.map_err(|e| format!("Network error: {e}"))? {
        for (event, data) in reader.feed(&chunk) {
            if event == "message_stop" {
                done = true;
            }
            if let Some(text) = message.apply(&event, &data)? {
                on_text(&text);
            }
        }
    }
    if !done {
        return Err("The answer was cut off. Try again.".into());
    }
    Ok(message)
}

/// PDF → document block, image → image block, text/code → inline text.
/// Mirrors readFileAsBlock() in ClaudeService.swift.
fn file_block(path: &str) -> Option<Value> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let media_type = match ext.as_str() {
        "pdf" => Some(("document", "application/pdf")),
        "jpg" | "jpeg" => Some(("image", "image/jpeg")),
        "png" => Some(("image", "image/png")),
        "gif" => Some(("image", "image/gif")),
        "webp" => Some(("image", "image/webp")),
        _ => None,
    };

    if let Some((block_type, media)) = media_type {
        let bytes = std::fs::read(path).ok()?;
        return Some(json!({
            "type": block_type,
            "source": { "type": "base64", "media_type": media, "data": base64(&bytes) },
        }));
    }

    let len = std::fs::metadata(path).ok()?.len();
    if len > MAX_INLINE_TEXT {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    Some(json!({ "type": "text", "text": format!("File contents:\n{text}") }))
}

/// Small standalone base64 encoder — not worth another dependency.
/// Also used for Stripe's basic auth.
pub(crate) fn base64_for(bytes: &[u8]) -> String {
    base64(bytes)
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(events: &[(&str, &str)]) -> (Message, String) {
        let mut m = Message::default();
        let mut shown = String::new();
        for (e, d) in events {
            if let Some(t) = m.apply(e, d).unwrap() {
                shown.push_str(&t);
            }
        }
        (m, shown)
    }

    #[test]
    fn rebuilds_text_and_tool_blocks_from_the_stream() {
        let (m, shown) = run(&[
            ("message_start", r#"{"message":{}}"#),
            ("content_block_start", r#"{"index":0,"content_block":{"type":"server_tool_use","id":"s1","name":"web_search","input":{}}}"#),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"query\":"}}"#),
            ("content_block_delta", r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"\"tacos\"}"}}"#),
            ("content_block_stop", r#"{"index":0}"#),
            ("content_block_start", r#"{"index":1,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"index":1,"delta":{"type":"text_delta","text":"Ho"}}"#),
            ("content_block_delta", r#"{"index":1,"delta":{"type":"citations_delta","citation":{"url":"u"}}}"#),
            ("content_block_delta", r#"{"index":1,"delta":{"type":"text_delta","text":"la"}}"#),
            ("content_block_stop", r#"{"index":1}"#),
            ("message_delta", r#"{"delta":{"stop_reason":"end_turn","stop_details":null}}"#),
        ]);
        assert_eq!(shown, "Hola");
        assert_eq!(m.blocks[0]["input"]["query"], "tacos");
        assert_eq!(m.blocks[1]["text"], "Hola");
        assert_eq!(m.blocks[1]["citations"][0]["url"], "u");
        assert_eq!(m.stop_reason.as_deref(), Some("end_turn"));
        assert!(m.stop_details.is_none());
    }

    #[test]
    fn stream_error_event_fails_the_turn() {
        let mut m = Message::default();
        let err = m.apply("error", r#"{"error":{"type":"overloaded_error","message":"Overloaded"}}"#);
        assert_eq!(err.unwrap_err(), "Claude API: Overloaded");
    }

    #[test]
    fn history_drops_unpaired_blocks_before_a_fallback() {
        let blocks = vec![
            json!({"type":"text","text":"a"}),
            json!({"type":"thinking","thinking":"x"}),
            json!({"type":"server_tool_use","id":"s1"}),
            json!({"type":"server_tool_use","id":"s2"}),
            json!({"type":"web_search_tool_result","tool_use_id":"s1"}),
            json!({"type":"fallback"}),
            json!({"type":"thinking","thinking":"y"}),
            json!({"type":"text","text":"b"}),
        ];
        let kept: Vec<String> = history_blocks(blocks)
            .iter()
            .map(|b| format!("{}{}", b["type"].as_str().unwrap(), b["id"].as_str().unwrap_or("")))
            .collect();
        assert_eq!(
            kept,
            ["text", "server_tool_uses1", "web_search_tool_result", "thinking", "text"]
        );
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}

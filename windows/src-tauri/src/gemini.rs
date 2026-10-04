// Gemini chat through Google's OpenAI-compatible endpoint — the same route
// ClaudeService.chatOpenAICompatible() takes on macOS. Chosen whenever the
// model in Settings starts with "gemini". The key stays in the Credential
// Manager under "google-api-key", like on the Mac.

use std::sync::Mutex;

use serde::Serialize;
use serde_json::{json, Value};

use crate::claude::{base64_for, ChatContext, ChatReply, SYSTEM_PROMPT};
use crate::secrets;

const BASE: &str = "https://generativelanguage.googleapis.com/v1beta/openai";
const MAX_TOKENS: u32 = 4096;
const MAX_INLINE_TEXT: u64 = 200_000;
/// Shown in Settings when the model list can't be fetched (no key yet, offline).
pub const FALLBACK_MODELS: &[&str] = &["gemini-2.5-flash", "gemini-2.5-pro"];

pub fn is_gemini(model: &str) -> bool {
    model.starts_with("gemini")
}

/// Kept apart from the Claude history: the two APIs shape messages differently.
#[derive(Default)]
pub struct GeminiChat {
    /// Text-only history. Images and file bytes are sent with their own turn
    /// and not re-sent on every later one.
    messages: Mutex<Vec<Value>>,
}

impl GeminiChat {
    pub fn reset(&self) {
        self.messages.lock().unwrap().clear();
    }
}

pub async fn send(
    chat: &GeminiChat,
    model: &str,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    let key = secrets::get("google-api-key")
        .ok_or_else(|| "Gemini API key missing. Open settings.".to_string())?;

    let first = chat.messages.lock().unwrap().is_empty();
    let mut parts: Vec<Value> = Vec::new();
    let mut text_prefix = String::new();

    match &context {
        Some(ChatContext::File { name, path }) if first => {
            match file_part(path) {
                Some(FilePart::Image(part)) => parts.push(part),
                Some(FilePart::Text(text)) => text_prefix.push_str(&format!("File contents:\n{text}\n\n")),
                None => {}
            }
            text_prefix.push_str(&format!("File: {name}\n\n"));
        }
        Some(ChatContext::Window { app_name, title, url }) if first => {
            text_prefix.push_str(&format!("Context — App: {app_name}, Window: {title}"));
            if let Some(url) = url {
                text_prefix.push_str(&format!(", URL: {url}"));
            }
            text_prefix.push_str("\n\n");
        }
        Some(ChatContext::Screen) => {
            let shot = crate::capture::screen()?;
            parts.push(image_part("image/png", &shot.png));
            text_prefix.push_str("Screenshot of my screen, taken just now.\n\n");
        }
        _ => {}
    }

    let user_text = format!("{text_prefix}{query}");
    parts.push(json!({ "type": "text", "text": user_text }));

    let mut messages = vec![json!({ "role": "system", "content": SYSTEM_PROMPT })];
    messages.extend(chat.messages.lock().unwrap().iter().cloned());
    messages.push(json!({ "role": "user", "content": parts }));

    let body = json!({ "model": model, "max_tokens": MAX_TOKENS, "messages": messages });
    let response = post(&key, "/chat/completions", &body).await?;

    let text = response
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        return Err("No response text.".into());
    }

    let mut history = chat.messages.lock().unwrap();
    history.push(json!({ "role": "user", "content": user_text }));
    history.push(json!({ "role": "assistant", "content": text }));
    Ok(ChatReply { text })
}

#[derive(Serialize)]
pub struct ModelOption {
    pub id: String,
    pub label: String,
}

/// Chat models the key can use, for the Settings picker. Same filter as
/// ClaudeService.fetchGoogleModels().
pub async fn models() -> Vec<ModelOption> {
    let fallback = || {
        FALLBACK_MODELS
            .iter()
            .map(|id| ModelOption { id: id.to_string(), label: id.to_string() })
            .collect()
    };
    let Some(key) = secrets::get("google-api-key") else { return fallback() };
    let Ok(response) = get(&key, "/models").await else { return fallback() };
    let excluded = ["embed", "imagen", "veo", "aqa", "tts", "audio", "live", "image"];
    let list: Vec<ModelOption> = response
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .map(|id| id.strip_prefix("models/").unwrap_or(id).to_string())
        .filter(|id| is_gemini(id) && !excluded.iter().any(|x| id.contains(x)))
        .map(|id| ModelOption { label: id.clone(), id })
        .collect();
    if list.is_empty() { fallback() } else { list }
}

enum FilePart {
    Image(Value),
    Text(String),
}

fn file_part(path: &str) -> Option<FilePart> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let media = match ext.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        // The OpenAI-compatible endpoint takes no PDFs: the name is all it gets.
        "pdf" => return None,
        _ => None,
    };
    if let Some(media) = media {
        let bytes = std::fs::read(path).ok()?;
        return Some(FilePart::Image(image_part(media, &bytes)));
    }
    if std::fs::metadata(path).ok()?.len() > MAX_INLINE_TEXT {
        return None;
    }
    std::fs::read_to_string(path).ok().map(FilePart::Text)
}

fn image_part(media: &str, bytes: &[u8]) -> Value {
    json!({
        "type": "image_url",
        "image_url": { "url": format!("data:{media};base64,{}", base64_for(bytes)) },
    })
}

fn client(timeout: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout))
        .build()
        .map_err(|e| e.to_string())
}

async fn post(key: &str, path: &str, body: &Value) -> Result<Value, String> {
    let response = client(90)?
        .post(format!("{BASE}{path}"))
        .bearer_auth(key)
        .json(body)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    read(response).await
}

async fn get(key: &str, path: &str) -> Result<Value, String> {
    let response = client(10)?
        .get(format!("{BASE}{path}"))
        .bearer_auth(key)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    read(response).await
}

async fn read(response: reqwest::Response) -> Result<Value, String> {
    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("Gemini API {status}: {}", error_detail(&text)));
    }
    serde_json::from_str(&text).map_err(|e| format!("Bad API response: {e}"))
}

/// Google answers errors either as `{"error": {...}}` or as a one-element array
/// of that; surface its message, which is what makes a bad key obvious.
fn error_detail(text: &str) -> String {
    let parsed = serde_json::from_str::<Value>(text).ok();
    let error = parsed.as_ref().and_then(|v| match v {
        Value::Array(items) => items.first().and_then(|i| i.get("error")),
        other => other.get("error"),
    });
    error
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| text.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_only_gemini_models() {
        assert!(is_gemini("gemini-2.5-flash"));
        assert!(!is_gemini("claude-opus-5"));
    }

    #[test]
    fn error_detail_reads_both_shapes() {
        assert_eq!(error_detail(r#"{"error":{"message":"bad key"}}"#), "bad key");
        assert_eq!(error_detail(r#"[{"error":{"message":"bad key"}}]"#), "bad key");
        assert_eq!(error_detail("plain"), "plain");
    }
}

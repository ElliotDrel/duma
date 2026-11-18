//! Fallback transcription of a finished recording through `POST /v1/audio/transcriptions`.
//! Used when the realtime socket fails, so a dictation is never lost.

use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::API_BASE;
use crate::responses::ResponsesClient;

#[derive(Clone, Debug, Default, Serialize)]
pub struct FileTranscript {
    pub text: String,
    pub error: Option<String>,
    pub model: String,
    pub audio_bytes: usize,
    pub latency_ms: u64,
    pub http_status: Option<u16>,
    pub response: Value,
}

impl ResponsesClient {
    pub async fn transcribe_wav(
        &self,
        api_key: &str,
        model: &str,
        wav: Vec<u8>,
        prompt: Option<String>,
    ) -> FileTranscript {
        let started = Instant::now();
        let mut out = FileTranscript {
            model: model.to_string(),
            audio_bytes: wav.len(),
            ..Default::default()
        };
        // ~3 MB per minute at 24 kHz PCM16: give big uploads more time.
        let timeout = Duration::from_secs(15 + (wav.len() / 200_000) as u64);
        for attempt in 1..=2 {
            let file = match reqwest::multipart::Part::bytes(wav.clone())
                .file_name("audio.wav")
                .mime_str("audio/wav")
            {
                Ok(file) => file,
                Err(error) => {
                    out.error = Some(error.to_string());
                    break;
                }
            };
            let mut form = reqwest::multipart::Form::new()
                .text("model", model.to_string())
                .part("file", file);
            if let Some(prompt) = prompt.clone() {
                form = form.text("prompt", prompt);
            }
            let sent = self
                .http()
                .post(format!("{API_BASE}/audio/transcriptions"))
                .bearer_auth(api_key)
                .timeout(timeout)
                .multipart(form)
                .send()
                .await;
            match sent {
                Ok(response) => {
                    let status = response.status().as_u16();
                    out.http_status = Some(status);
                    let body = response.text().await.unwrap_or_default();
                    out.response =
                        serde_json::from_str(&body).unwrap_or_else(|_| json!({ "raw": body }));
                    match out.response["text"].as_str() {
                        Some(text) if (200..300).contains(&status) => {
                            out.text = text.to_string();
                            out.error = None;
                            break;
                        }
                        _ => {
                            out.error = Some(format!(
                                "HTTP {status}: {}",
                                out.response["error"]["message"]
                            ))
                        }
                    }
                    if !(status == 429 || status >= 500) {
                        break;
                    }
                }
                Err(error) => out.error = Some(error.to_string()),
            }
            if attempt == 1 {
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        }
        out.latency_ms = started.elapsed().as_millis() as u64;
        out
    }
}

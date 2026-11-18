//! Streaming transcription through `wss://api.openai.com/v1/realtime?intent=transcription`.
//!
//! Audio is mono PCM16 at 24 kHz. The socket is opened and configured (waiting for
//! `session.updated`) while the user starts speaking; audio is streamed as it arrives and committed
//! once at the end. The final transcript is `conversation.item.input_audio_transcription.completed`.
//! Docs: https://developers.openai.com/api/docs/guides/realtime-transcription
//!
//! Every outcome, including failures, returns a [`Transcript`] record so nothing is lost from the log.
//! The caller keeps its own copy of the audio and uses `/v1/audio/transcriptions` when `error` is set.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub const SAMPLE_RATE: u32 = 24_000;
const URL: &str = "wss://api.openai.com/v1/realtime?intent=transcription";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const SEND_TIMEOUT: Duration = Duration::from_secs(2);
/// After commit, how long to wait for the final transcript before the caller falls back to file transcription.
const COMPLETION_TIMEOUT: Duration = Duration::from_secs(3);

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TranscribeConfig {
    pub model: String,
    /// `minimal`, `low`, `medium`, `high`, `xhigh`. `None` keeps the model default.
    pub delay: Option<String>,
    pub languages: Vec<String>,
    /// Literal terms the speaker is likely to say (names, jargon).
    pub keywords: Vec<String>,
    pub prompt: Option<String>,
}

impl Default for TranscribeConfig {
    fn default() -> Self {
        Self {
            model: "gpt-live-transcribe".into(),
            delay: Some("low".into()),
            languages: Vec::new(),
            keywords: Vec::new(),
            prompt: None,
        }
    }
}

impl TranscribeConfig {
    pub fn session_update(&self) -> Value {
        let mut transcription = json!({ "model": self.model });
        if let Some(delay) = self.delay.as_deref().filter(|d| !d.is_empty()) {
            transcription["delay"] = json!(delay);
        }
        if !self.languages.is_empty() {
            transcription["languages"] = json!(self.languages);
        }
        // Keywords must be single-line and free of `<` / `>` or the update is rejected.
        let keywords: Vec<&str> = self
            .keywords
            .iter()
            .map(|k| k.trim())
            .filter(|k| !k.is_empty() && !k.contains(['<', '>', '\r', '\n']))
            .collect();
        if !keywords.is_empty() {
            transcription["keywords"] = json!(keywords);
        }
        if let Some(prompt) = self.prompt.as_deref().filter(|p| !p.trim().is_empty()) {
            transcription["prompt"] = json!(prompt);
        }
        json!({
            "type": "session.update",
            "session": {
                "type": "transcription",
                "audio": { "input": {
                    "format": { "type": "audio/pcm", "rate": SAMPLE_RATE },
                    "transcription": transcription,
                    "turn_detection": Value::Null
                }}
            }
        })
    }
}

/// An open socket whose transcription session has been configured and acknowledged.
pub struct Connection {
    ws: Ws,
    record: Transcript,
    started: Instant,
}

impl Connection {
    /// Connects, sends `session.update` and waits for `session.updated`, so a bad configuration fails here
    /// rather than mid-dictation.
    pub async fn open(api_key: &str, config: &TranscribeConfig) -> Result<Self, Transcript> {
        let started = Instant::now();
        let mut record = Transcript::default();
        match timeout(
            CONNECT_TIMEOUT,
            open_inner(api_key, config, &mut record, started),
        )
        .await
        {
            Ok(Ok(ws)) => Ok(Self {
                ws,
                record,
                started,
            }),
            Ok(Err(error)) => Err(record.failed(error.to_string())),
            Err(_) => Err(record.failed("timed out opening the transcription session".into())),
        }
    }

    /// Starts streaming. `on_delta` receives partial text as it arrives.
    pub fn start(self, on_delta: Box<dyn Fn(&str) + Send>) -> LiveSession {
        let (tx, rx) = mpsc::unbounded_channel();
        let (done_tx, done_rx) = oneshot::channel();
        tokio::spawn(async move {
            let mut record = self.record;
            if let Err(error) = run(self.ws, rx, on_delta, &mut record, self.started).await {
                record.error.get_or_insert(error.to_string());
            }
            if record.text.is_empty() && record.error.is_some() {
                record.text = record.deltas.iter().map(|(_, d)| d.as_str()).collect();
                record.text_from_deltas = !record.text.is_empty();
            }
            let _ = done_tx.send(record);
        });
        LiveSession { tx, done: done_rx }
    }
}

async fn open_inner(
    api_key: &str,
    config: &TranscribeConfig,
    record: &mut Transcript,
    started: Instant,
) -> Result<Ws> {
    let mut request = URL.into_client_request()?;
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", api_key.trim()).parse()?,
    );
    let (mut ws, _response) = tokio_tungstenite::connect_async_with_config(request, None, true)
        .await
        .context("connecting to the realtime transcription socket")?;
    record.connect_ms = started.elapsed().as_millis() as u64;
    let update = config.session_update();
    ws.send(Message::text(update.to_string())).await?;
    record.client_events.push((ms(started), update));
    while let Some(message) = ws.next().await {
        let event = parse(message?, record, started)?;
        match event["type"].as_str() {
            Some("session.updated") => {
                record.session_ready_ms = Some(ms(started));
                record.server_events.push((ms(started), event));
                return Ok(ws);
            }
            Some("error") => {
                let message = error_text(&event);
                record.server_events.push((ms(started), event));
                bail!("transcription session rejected: {message}");
            }
            _ => record.server_events.push((ms(started), event)),
        }
    }
    bail!("socket closed before the session was ready")
}

fn ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

fn error_text(event: &Value) -> String {
    let error = &event["error"];
    format!(
        "{} ({})",
        error["message"].as_str().unwrap_or("unknown error"),
        error["code"]
            .as_str()
            .or(error["type"].as_str())
            .unwrap_or("no code")
    )
}

/// Turns a socket message into a JSON event. Close frames and unparseable text are recorded and become errors or `Null`.
fn parse(message: Message, record: &mut Transcript, started: Instant) -> Result<Value> {
    match message {
        Message::Text(text) => Ok(serde_json::from_str(&text).unwrap_or_else(|_| {
            record.server_events.push((
                ms(started),
                json!({ "type": "duma.unparseable", "raw": text.as_str() }),
            ));
            Value::Null
        })),
        Message::Close(frame) => {
            let detail = frame
                .map(|f| format!("{} {}", u16::from(f.code), f.reason))
                .unwrap_or_default();
            record.close = Some(detail.clone());
            Err(anyhow!("server closed the socket: {detail}"))
        }
        _ => Ok(Value::Null),
    }
}

enum Command {
    Audio(Vec<u8>),
    Commit,
    Cancel,
}

pub struct LiveSession {
    tx: mpsc::UnboundedSender<Command>,
    done: oneshot::Receiver<Transcript>,
}

impl LiveSession {
    /// Queues mono PCM16 samples at 24 kHz. Returns false once the session has failed.
    pub fn send_audio(&self, samples: &[i16]) -> bool {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        self.tx.send(Command::Audio(bytes)).is_ok()
    }

    /// Commits the audio and waits for the final transcript (bounded). Check `error` on the result.
    pub async fn finish(self) -> Transcript {
        let _ = self.tx.send(Command::Commit);
        match timeout(COMPLETION_TIMEOUT + SEND_TIMEOUT * 2, self.done).await {
            Ok(Ok(record)) => record,
            Ok(Err(_)) => {
                Transcript::default().failed("transcription task ended unexpectedly".into())
            }
            Err(_) => {
                Transcript::default().failed("timed out waiting for the transcription task".into())
            }
        }
    }

    pub fn cancel(self) {
        let _ = self.tx.send(Command::Cancel);
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Transcript {
    pub text: String,
    /// The final event never arrived; `text` is the concatenated partial transcripts.
    pub text_from_deltas: bool,
    pub error: Option<String>,
    pub item_id: Option<String>,
    pub connect_ms: u64,
    pub session_ready_ms: Option<u64>,
    pub first_delta_ms: Option<u64>,
    pub commit_sent_ms: Option<u64>,
    pub committed_ms: Option<u64>,
    pub final_ms: Option<u64>,
    /// From commit to the final transcript: what the user waits for after releasing the key.
    pub commit_to_final_ms: Option<u64>,
    pub audio_chunks_sent: usize,
    pub audio_bytes_sent: usize,
    /// The audio was too short to commit (< 100 ms), so no request was made.
    pub too_short: bool,
    pub close: Option<String>,
    /// Partial transcripts with milliseconds since the socket opened.
    pub deltas: Vec<(u64, String)>,
    /// Client events except audio appends.
    pub client_events: Vec<(u64, Value)>,
    /// Every non-delta server event, verbatim.
    pub server_events: Vec<(u64, Value)>,
    pub usage: Option<Value>,
}

impl Transcript {
    fn failed(mut self, error: String) -> Self {
        self.error = Some(error);
        self
    }
}

async fn send(
    sink: &mut futures_util::stream::SplitSink<Ws, Message>,
    event: &Value,
) -> Result<()> {
    timeout(SEND_TIMEOUT, sink.send(Message::text(event.to_string())))
        .await
        .map_err(|_| anyhow!("timed out sending to the transcription socket"))??;
    Ok(())
}

async fn run(
    ws: Ws,
    mut rx: mpsc::UnboundedReceiver<Command>,
    on_delta: Box<dyn Fn(&str) + Send>,
    out: &mut Transcript,
    started: Instant,
) -> Result<()> {
    let (mut sink, mut stream) = ws.split();
    let mut committed_at: Option<Instant> = None;
    let mut input_open = true;
    let deadline = tokio::time::sleep(Duration::from_secs(60 * 30));
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            command = rx.recv(), if input_open => match command {
                Some(Command::Audio(bytes)) => {
                    out.audio_chunks_sent += 1;
                    out.audio_bytes_sent += bytes.len();
                    let audio = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    send(&mut sink, &json!({ "type": "input_audio_buffer.append", "audio": audio })).await?;
                }
                Some(Command::Commit) => {
                    input_open = false;
                    // The API rejects commits under ~100 ms of audio; treat that as silence.
                    if out.audio_bytes_sent < (SAMPLE_RATE as usize / 10) * 2 {
                        out.too_short = true;
                        let _ = sink.close().await;
                        return Ok(());
                    }
                    let commit = json!({ "type": "input_audio_buffer.commit" });
                    committed_at = Some(Instant::now());
                    out.commit_sent_ms = Some(ms(started));
                    deadline.as_mut().reset(tokio::time::Instant::now() + COMPLETION_TIMEOUT);
                    send(&mut sink, &commit).await?;
                    out.client_events.push((ms(started), commit));
                }
                Some(Command::Cancel) | None => {
                    let _ = sink.close().await;
                    bail!("transcription cancelled");
                }
            },
            message = stream.next() => {
                let message = match message {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => return Err(e).context("realtime socket error"),
                    None => bail!("realtime socket closed before the final transcript"),
                };
                let event = parse(message, out, started)?;
                match event["type"].as_str().unwrap_or_default() {
                    "conversation.item.input_audio_transcription.delta" => {
                        let delta = event["delta"].as_str().unwrap_or_default();
                        out.first_delta_ms.get_or_insert(ms(started));
                        out.deltas.push((ms(started), delta.to_string()));
                        on_delta(delta);
                        continue;
                    }
                    "input_audio_buffer.committed" => {
                        out.committed_ms = Some(ms(started));
                        out.item_id = event["item_id"].as_str().map(str::to_string);
                    }
                    "conversation.item.input_audio_transcription.completed" => {
                        let item = event["item_id"].as_str();
                        if committed_at.is_some() && (out.item_id.is_none() || item == out.item_id.as_deref()) {
                            out.text = event["transcript"].as_str().unwrap_or_default().to_string();
                            out.usage = event.get("usage").cloned();
                            out.final_ms = Some(ms(started));
                            out.commit_to_final_ms = committed_at.map(|t| t.elapsed().as_millis() as u64);
                            out.server_events.push((ms(started), event));
                            let _ = sink.close().await;
                            return Ok(());
                        }
                    }
                    "conversation.item.input_audio_transcription.failed" => {
                        let message = error_text(&event);
                        out.server_events.push((ms(started), event));
                        bail!("transcription failed: {message}");
                    }
                    "error" => {
                        let message = error_text(&event);
                        out.server_events.push((ms(started), event));
                        bail!("realtime API error: {message}");
                    }
                    _ => {}
                }
                if !event.is_null() {
                    out.server_events.push((ms(started), event));
                }
            }
            _ = &mut deadline => bail!("timed out waiting for the final transcript"),
        }
    }
}

/// Converts f32 samples in [-1, 1] to PCM16.
pub fn to_pcm16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_update_matches_documented_shape() {
        let config = TranscribeConfig {
            keywords: vec!["Duma".into(), "bad<kw>".into(), " ".into()],
            ..Default::default()
        };
        let update = config.session_update();
        assert_eq!(update["session"]["type"], "transcription");
        let input = &update["session"]["audio"]["input"];
        assert_eq!(input["format"]["rate"], 24_000);
        assert_eq!(input["transcription"]["model"], "gpt-live-transcribe");
        assert_eq!(input["transcription"]["keywords"], json!(["Duma"]));
        assert!(input["turn_detection"].is_null());
        assert!(input["transcription"].get("prompt").is_none());
    }

    #[test]
    fn pcm16_clamps() {
        assert_eq!(to_pcm16(&[2.0, -2.0, 0.0]), vec![i16::MAX, -i16::MAX, 0]);
    }
}

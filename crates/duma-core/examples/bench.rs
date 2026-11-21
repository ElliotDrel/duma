//! Latency bench: streams a 24 kHz mono 16-bit WAV through the live transcription socket in real time
//! (like a held key), then runs dictation cleanup, and prints where the time went.
//!
//! `cargo run -p duma-core --example bench -- <file.wav> [runs]`
//! Uses `OPENAI_API_KEY` or the key saved in the app's keychain entry, and the default settings.

use std::time::{Duration, Instant};

use duma_core::prompts::{self, PromptContext};
use duma_core::settings::Settings;
use duma_openai::responses::{ResponseRequest, ResponsesClient};
use duma_openai::transcribe::{Connection, SAMPLE_RATE};

const CHUNK_MS: usize = 50;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    duma_openai::install_crypto_provider();
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: bench <file.wav> [runs]"))?;
    let runs: usize = args.next().map(|r| r.parse()).transpose()?.unwrap_or(1);
    let key = duma_core::secrets::api_key().ok_or_else(|| anyhow::anyhow!("no OpenAI API key"))?;

    let mut reader = hound::WavReader::open(&path)?;
    let spec = reader.spec();
    anyhow::ensure!(
        spec.sample_rate == SAMPLE_RATE && spec.channels == 1 && spec.bits_per_sample == 16,
        "need {SAMPLE_RATE} Hz mono 16-bit, got {spec:?}"
    );
    let samples: Vec<i16> = reader.samples::<i16>().collect::<Result<_, _>>()?;
    let settings = Settings::default();
    let responses = ResponsesClient::new();
    responses.warm(&key, &settings.cleanup.model).await;

    for run in 1..=runs {
        // The app opens the socket on key-down, so connecting overlaps speech; measure it separately.
        let t = Instant::now();
        let connection = Connection::open(&key, &settings.transcribe_config())
            .await
            .map_err(|t| anyhow::anyhow!("open failed: {:?}", t.error))?;
        let open_ms = t.elapsed().as_millis();

        let session = connection.start(Box::new(|_| {}));
        let chunk = SAMPLE_RATE as usize * CHUNK_MS / 1000;
        for part in samples.chunks(chunk) {
            session.send_audio(part);
            tokio::time::sleep(Duration::from_millis(CHUNK_MS as u64)).await;
        }
        let released = Instant::now();
        let transcript = session.finish().await;
        let stt_ms = released.elapsed().as_millis();
        if let Some(error) = &transcript.error {
            anyhow::bail!(
                "transcription failed: {error}\n{:#?}",
                transcript.server_events
            );
        }

        let (instructions, input) =
            prompts::dictation(&settings, &transcript.text, &PromptContext::default());
        let request = ResponseRequest {
            model: settings.cleanup.model.clone(),
            instructions,
            input,
            reasoning_effort: settings.cleanup.reasoning_effort.clone(),
            verbosity: settings.cleanup.verbosity.clone(),
            service_tier: None,
            store: false,
            timeout_ms: settings.cleanup.timeout_ms,
        };
        let cleanup = responses.create(&key, &request).await;
        let total_ms = released.elapsed().as_millis();

        println!("--- run {run}");
        println!(
            "open {open_ms} ms | first partial {:?} ms after open",
            transcript.first_delta_ms
        );
        println!(
            "release -> transcript {stt_ms} ms | cleanup {} ms | release -> text {total_ms} ms",
            cleanup.latency_ms
        );
        println!("transcript: {}", transcript.text);
        match &cleanup.error {
            Some(error) => println!("cleanup error: {error}"),
            None => println!("cleaned:    {}", cleanup.text),
        }
    }
    Ok(())
}

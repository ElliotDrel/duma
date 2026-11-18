//! OpenAI clients used by Duma.
//!
//! - [`transcribe`]: streaming speech-to-text over the Realtime transcription WebSocket.
//! - [`responses`]: text generation through the Responses API over a kept-alive HTTP/2 client.
//!
//! Both return everything they sent and received so the caller can log it in full.

pub mod file_transcribe;
pub mod responses;
pub mod transcribe;

pub const API_BASE: &str = "https://api.openai.com/v1";

/// Pins rustls to the `ring` crypto provider. Call once at startup: if another dependency ever enables
/// `aws-lc-rs` too, rustls would otherwise panic on the first TLS connection.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

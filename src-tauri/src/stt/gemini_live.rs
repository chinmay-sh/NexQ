// Gemini Live (gemini-3.5-transcribe-live) streaming STT over WebSocket
//
// Connects to the Live API's BidiGenerateContent socket, sends a setup message
// asking for input transcription only, then streams 16 kHz mono PCM as base64
// JSON in 100 ms frames. Gemini's own speech detection splits speech into
// turns (measured 2026-11-23: ~0.5 s behind speech, long speech split every
// ~14 s):
// - `interimInputTranscription` carries the turn so far and becomes an interim
// - `inputTranscription` carries the finished turn and becomes the final
// No turn signals: a final can end mid-sentence when Gemini splits long speech.
//
// - Several API keys: "You" starts on the first key, "Them" on the second, so
//   a meeting's two sessions spread over two keys. A key that hits a rate limit
//   or quota is swapped for the next; a key Gemini rejects is dropped.
// - Google ends a session at about 10 minutes (goAway at 9:00). A new session
//   is opened after 8 minutes, at a pause in speech, and swapped in while the
//   old one finishes its last turn; a goAway or 9 minutes forces the swap.
// - Language: the app's language as the BCP-47 code ("en-IN" gives romanized
//   Hinglish, "hi-IN" Devanagari with English kept in Latin), none for Auto.
// - Custom vocabulary and Verbatim/Clean style from GeminiLiveConfig.
// - The rest follows Cartesia: API key in a header, gaps filled with silence,
//   muting closes the stream, background reconnect that never blocks the audio
//   loop, status events on `stt_connection_status`.

use async_trait::async_trait;
use futures::stream::SplitSink;
use futures::{FutureExt, SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::audio::AudioChunk;
use crate::stt::deepgram::ConnectionStatusEvent;
use crate::stt::provider::{reported_language, STTProvider, STTProviderType, TranscriptResult};

type GeminiSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Provider id, also the credential holding the keys (one per line).
pub const ID: &str = "gemini_live";
/// Credential used when no Gemini Live key is saved: the Gemini LLM key.
pub const FALLBACK_KEY_CREDENTIAL: &str = "gemini";

const MODEL: &str = "gemini-3.5-transcribe-live";
const DEFAULT_WS_URL: &str =
    "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";

const SAMPLE_RATE: u32 = 16_000;
/// 100 ms of s16le audio, the frame size the Live API recommends.
const FRAME_BYTES: usize = SAMPLE_RATE as usize / 10 * 2;
const MAX_API_KEYS: usize = 10;
pub const MAX_VOCABULARY_TERMS: usize = 1000;

/// Handshake plus setupComplete.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// After the end of audio, wait at least this long for the last words (the
/// final came ~0.9 s after the end of audio in tests)...
const FINISH_WAIT_MIN: Duration = Duration::from_millis(1500);
/// ...and at most this long for an open turn to finish.
const FINISH_WAIT_MAX: Duration = Duration::from_secs(5);
/// Open a replacement session after this long, at a pause in speech.
const ROTATE_AFTER: Duration = Duration::from_secs(8 * 60);
/// Swap even mid-turn after this long (Google sends goAway at 9:00).
const FORCE_ROTATE_AFTER: Duration = Duration::from_secs(9 * 60);
/// Wait before retrying a failed replacement session.
const ROTATE_RETRY: Duration = Duration::from_secs(15);

const AUDIO_QUEUE_CHUNKS: usize = 512;
const GAP_BEFORE_SILENCE: Duration = Duration::from_millis(150);
const GAP_CHECK_INTERVAL: Duration = Duration::from_millis(100);
const MAX_SILENCE_FILL: Duration = Duration::from_secs(1);
const STALLED_SAMPLES: usize = 5 * SAMPLE_RATE as usize;
const RESUME_HOLD_SAMPLES: usize = SAMPLE_RATE as usize;

/// Vocabulary and transcription style (Settings → STT → Gemini Live).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GeminiLiveConfig {
    /// Names and jargon Gemini should spell correctly.
    pub vocabulary: Vec<String>,
    /// "VERBATIM" (every word as spoken) or "SMART" (drops fillers).
    pub mode: String,
}

impl Default for GeminiLiveConfig {
    fn default() -> Self {
        Self { vocabulary: Vec::new(), mode: "VERBATIM".into() }
    }
}

impl GeminiLiveConfig {
    /// Unknown modes become VERBATIM; vocabulary is trimmed, de-duplicated
    /// (ignoring case) and capped.
    pub fn normalized(mut self) -> Self {
        if !matches!(self.mode.as_str(), "VERBATIM" | "SMART") {
            self.mode = "VERBATIM".into();
        }
        let mut seen = std::collections::HashSet::new();
        self.vocabulary = self
            .vocabulary
            .iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty() && seen.insert(t.to_lowercase()))
            .take(MAX_VOCABULARY_TERMS)
            .collect();
        self
    }
}

/// Keys from a credential value: one per line (commas also work), trimmed,
/// without duplicates, at most 10.
pub fn parse_keys(raw: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    raw.split(['\n', '\r', ','])
        .map(str::trim)
        .filter(|k| !k.is_empty() && seen.insert(k.to_string()))
        .take(MAX_API_KEYS)
        .map(str::to_string)
        .collect()
}

/// The Gemini Live keys, or else the Gemini LLM key. `get` reads a credential.
pub fn keys_from_store(get: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let own = get(ID).map(|raw| parse_keys(&raw)).unwrap_or_default();
    if !own.is_empty() {
        return own;
    }
    get(FALLBACK_KEY_CREDENTIAL).map(|raw| parse_keys(&raw)).unwrap_or_default()
}

/// BCP-47 codes for the setup message: none for Auto-detect.
fn language_codes(language: &str) -> Vec<String> {
    let language = language.trim();
    if language.is_empty() || language.eq_ignore_ascii_case("auto") {
        Vec::new()
    } else {
        vec![language.to_string()]
    }
}

fn setup_message(language: &str, config: &GeminiLiveConfig) -> String {
    let mut transcription = json!({ "languageCodes": language_codes(language), "mode": config.mode });
    if !config.vocabulary.is_empty() {
        transcription["customVocabulary"] = json!(config.vocabulary);
    }
    // No realtimeInputConfig: Gemini's own speech detection splits turns.
    json!({
        "setup": {
            "model": format!("models/{}", MODEL),
            "generationConfig": { "responseModalities": ["TEXT"] },
            "inputAudioTranscription": transcription,
        }
    })
    .to_string()
}

const BASE64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let n = group.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= group.len() {
                out.push(BASE64_CHARS[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn audio_message(pcm_s16le: &[u8]) -> String {
    json!({
        "realtimeInput": {
            "audio": { "data": base64_encode(pcm_s16le), "mimeType": "audio/pcm;rate=16000" }
        }
    })
    .to_string()
}

const AUDIO_STREAM_END: &str = r#"{"realtimeInput":{"audioStreamEnd":true}}"#;

fn pcm_to_bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

#[derive(Debug, Clone, PartialEq)]
enum ServerEvent {
    SetupComplete,
    Interim(String),
    Final(String),
    GoAway,
}

/// One server frame; a frame can carry several events.
fn parse_server_message(raw: &str) -> Vec<ServerEvent> {
    let mut events = Vec::new();
    let Ok(msg) = serde_json::from_str::<Value>(raw) else { return events };
    if msg.get("setupComplete").is_some() {
        events.push(ServerEvent::SetupComplete);
    }
    if let Some(sc) = msg.get("serverContent") {
        if let Some(text) = sc.pointer("/interimInputTranscription/text").and_then(Value::as_str) {
            events.push(ServerEvent::Interim(text.to_string()));
        }
        if let Some(text) = sc.pointer("/inputTranscription/text").and_then(Value::as_str) {
            events.push(ServerEvent::Final(text.to_string()));
        }
    }
    if msg.get("goAway").is_some() {
        events.push(ServerEvent::GoAway);
    }
    events
}

/// Why a connection attempt failed.
#[derive(Debug)]
enum ConnectError {
    /// Gemini rejected the key or the setup: retrying with this key won't help.
    Auth(String),
    /// The key hit a rate limit or quota; it works again later.
    RateLimited(String),
    Other(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Auth(m) | ConnectError::RateLimited(m) | ConnectError::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ConnectError {}

/// Google reports rate limits and quotas as RESOURCE_EXHAUSTED / 429.
fn is_rate_limit(text: &str) -> bool {
    let text = text.to_lowercase();
    ["resource_exhausted", "resource exhausted", "quota", "rate limit", "too many requests"]
        .iter()
        .any(|needle| text.contains(needle))
}

/// A close during setup with 1007 (bad key) or 1008 (missing key) is an auth
/// failure. Mid-session Google also closes with 1008 after goAway, so later
/// closes are never treated as a bad key.
fn classify_close(code: Option<u16>, reason: &str, during_setup: bool) -> ConnectError {
    let msg = format!("Gemini closed the connection ({}): {}", code.map_or("no code".into(), |c| c.to_string()), reason);
    if is_rate_limit(reason) {
        ConnectError::RateLimited(msg)
    } else if during_setup && matches!(code, Some(1007) | Some(1008)) {
        ConnectError::Auth(msg)
    } else {
        ConnectError::Other(msg)
    }
}

fn reconnect_backoff(attempt: u32) -> Duration {
    match attempt {
        0..=4 => Duration::from_secs(1 << attempt.saturating_sub(1)),
        _ => Duration::from_secs(15),
    }
}

fn silence_samples_for(idle: Duration) -> usize {
    if idle < GAP_BEFORE_SILENCE {
        return 0;
    }
    idle.min(MAX_SILENCE_FILL).as_millis() as usize * SAMPLE_RATE as usize / 1000
}

fn hold_for_resume(held: &mut Vec<i16>, samples: &[i16]) {
    held.extend_from_slice(samples);
    if held.len() > RESUME_HOLD_SAMPLES {
        let excess = held.len() - RESUME_HOLD_SAMPLES;
        held.drain(..excess);
    }
}

fn epoch_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Turns Gemini's transcription events into results for one party.
struct TurnTracker {
    party: String,
    language: Option<String>,
    turn_start_ms: Option<u64>,
    turn_text: String,
}

impl TurnTracker {
    fn new(party: &str, language: &str) -> Self {
        Self {
            party: party.to_string(),
            language: reported_language(language),
            turn_start_ms: None,
            turn_text: String::new(),
        }
    }

    fn is_open(&self) -> bool {
        self.turn_start_ms.is_some()
    }

    fn on_event(&mut self, event: &ServerEvent, now_ms: u64) -> Option<TranscriptResult> {
        match event {
            ServerEvent::Interim(text) => {
                let text = text.trim();
                if text.is_empty() {
                    return None;
                }
                let start = *self.turn_start_ms.get_or_insert(now_ms);
                self.turn_text = text.to_string();
                Some(self.result(self.turn_text.clone(), false, start, None))
            }
            ServerEvent::Final(text) => {
                let start = self.turn_start_ms.take().unwrap_or(now_ms);
                self.turn_text.clear();
                let text = text.trim();
                if text.is_empty() {
                    return None;
                }
                Some(self.result(text.to_string(), true, start, Some(now_ms.saturating_sub(start))))
            }
            _ => None,
        }
    }

    /// The open turn's latest text as a final, once, when its session ends first.
    fn finish_open_turn(&mut self, now_ms: u64) -> Option<TranscriptResult> {
        let start = self.turn_start_ms.take()?;
        let text = std::mem::take(&mut self.turn_text);
        if text.is_empty() {
            return None;
        }
        Some(self.result(text, true, start, Some(now_ms.saturating_sub(start))))
    }

    fn result(&self, text: String, is_final: bool, start_ms: u64, duration_ms: Option<u64>) -> TranscriptResult {
        TranscriptResult {
            text,
            is_final,
            confidence: 1.0,
            timestamp_ms: start_ms,
            speaker: Some(self.party.clone()),
            language: self.language.clone(),
            segment_id: None,
            duration_ms,
            speaker_tag: None,
            turn: None,
        }
    }
}

fn lock(turns: &Mutex<TurnTracker>) -> std::sync::MutexGuard<'_, TurnTracker> {
    turns.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the reader learned about its session.
#[derive(Default)]
struct SessionFlags {
    go_away: AtomicBool,
    rate_limited: AtomicBool,
}

/// One open Gemini session.
struct Session {
    audio_tx: mpsc::Sender<Vec<u8>>,
    reader: tokio::task::JoinHandle<()>,
    turns: Arc<Mutex<TurnTracker>>,
    flags: Arc<SessionFlags>,
    opened_at: Instant,
}

impl Session {
    fn ended(&self) -> bool {
        self.reader.is_finished() || self.audio_tx.is_closed()
    }

    /// End the audio, give Gemini time to finish the last turn, then close.
    /// An unfinished turn is kept as a final.
    async fn finish(self, result_tx: Option<mpsc::Sender<TranscriptResult>>) {
        let Session { audio_tx, reader, turns, .. } = self;
        drop(audio_tx); // the writer sends audioStreamEnd
        let started = Instant::now();
        loop {
            let waited = started.elapsed();
            if reader.is_finished() || waited >= FINISH_WAIT_MAX || (waited >= FINISH_WAIT_MIN && !lock(&turns).is_open()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        reader.abort();
        let last = lock(&turns).finish_open_turn(epoch_now_ms());
        if let (Some(last), Some(tx)) = (last, result_tx) {
            let _ = tx.send(last).await;
        }
    }
}

type ConnectTask = tokio::task::JoinHandle<(usize, Result<GeminiSocket, ConnectError>)>;

/// Gemini Live streaming STT for one party.
pub struct GeminiLiveSTT {
    keys: Vec<String>,
    /// Key used for the next connection.
    key_index: usize,
    /// Keys found rate limited since the last successful connection.
    rate_limited_in_a_row: usize,
    language: String,
    config: GeminiLiveConfig,
    party: String,
    app_handle: Option<tauri::AppHandle>,
    base_url: String,
    session: Option<Session>,
    result_tx: Option<mpsc::Sender<TranscriptResult>>,
    /// Every key was rejected: no reconnects until the next start.
    reconnect_blocked: bool,
    reconnect_task: Option<ConnectTask>,
    reconnect_attempt: u32,
    next_reconnect_at: Option<Instant>,
    /// Replacement session being opened before Google ends the current one.
    rotate_task: Option<ConnectTask>,
    next_rotate_at: Option<Instant>,
    dropped_chunks: u64,
    dropped_samples_streak: usize,
    paused: bool,
    resuming_since: Option<Instant>,
    resume_hold: Vec<i16>,
}

impl GeminiLiveSTT {
    pub fn new() -> Self {
        Self {
            keys: Vec::new(),
            key_index: 0,
            rate_limited_in_a_row: 0,
            language: "auto".to_string(),
            config: GeminiLiveConfig::default(),
            party: "Them".to_string(),
            app_handle: None,
            base_url: DEFAULT_WS_URL.to_string(),
            session: None,
            result_tx: None,
            reconnect_blocked: false,
            reconnect_task: None,
            reconnect_attempt: 0,
            next_reconnect_at: None,
            rotate_task: None,
            next_rotate_at: None,
            dropped_chunks: 0,
            dropped_samples_streak: 0,
            paused: false,
            resuming_since: None,
            resume_hold: Vec::new(),
        }
    }

    pub fn with_api_keys(keys: Vec<String>) -> Self {
        let mut stt = Self::new();
        stt.keys = keys.iter().flat_map(|k| parse_keys(k)).collect();
        stt
    }

    pub fn set_party(&mut self, party: &str) {
        self.party = party.to_string();
    }

    pub fn set_app_handle(&mut self, handle: tauri::AppHandle) {
        self.app_handle = Some(handle);
    }

    pub fn set_config(&mut self, config: GeminiLiveConfig) {
        self.config = config.normalized();
    }

    #[cfg(test)]
    fn for_endpoint(keys: &[&str], base_url: &str) -> Self {
        let mut stt = Self::with_api_keys(keys.iter().map(|k| k.to_string()).collect());
        stt.base_url = base_url.to_string();
        stt
    }

    fn is_streaming(&self) -> bool {
        self.session.is_some()
    }

    fn emit_status(&self, status: &str, message: Option<String>) {
        if let Some(ref handle) = self.app_handle {
            let event = ConnectionStatusEvent {
                provider: ID.to_string(),
                party: self.party.clone(),
                status: status.to_string(),
                message,
            };
            if let Err(e) = tauri::Emitter::emit(handle, "stt_connection_status", &event) {
                log::warn!("GeminiLiveSTT: Failed to emit status event: {}", e);
            }
        }
    }

    /// Open the socket (key in the `x-goog-api-key` header) and finish setup.
    async fn connect(api_key: &str, url: &str, setup: String) -> Result<GeminiSocket, ConnectError> {
        let mut request = url
            .into_client_request()
            .map_err(|e| ConnectError::Other(format!("Invalid Gemini URL: {}", e)))?;
        let key = HeaderValue::from_str(api_key)
            .map_err(|_| ConnectError::Auth("A Gemini API key contains invalid characters".to_string()))?;
        request.headers_mut().insert("x-goog-api-key", key);

        let attempt = async {
            let mut socket = match tokio_tungstenite::connect_async(request).await {
                Ok((socket, _)) => socket,
                Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                    let code = response.status().as_u16();
                    let body = response
                        .body()
                        .as_ref()
                        .map(|b| String::from_utf8_lossy(b).chars().take(200).collect::<String>())
                        .unwrap_or_default();
                    let msg = format!("Gemini refused the connection (HTTP {}) {}", code, body);
                    return Err(if code == 429 || is_rate_limit(&body) {
                        ConnectError::RateLimited(msg)
                    } else if matches!(code, 400 | 401 | 403) {
                        ConnectError::Auth(msg)
                    } else {
                        ConnectError::Other(msg)
                    });
                }
                Err(e) => return Err(ConnectError::Other(format!("Couldn't connect to Gemini: {}", e))),
            };
            socket
                .send(Message::Text(setup.into()))
                .await
                .map_err(|e| ConnectError::Other(format!("Couldn't send the Gemini setup: {}", e)))?;
            while let Some(message) = socket.next().await {
                let text = match message {
                    Ok(Message::Text(t)) => t.to_string(),
                    // The Live API sends JSON in binary frames too.
                    Ok(Message::Binary(b)) => String::from_utf8_lossy(&b).into_owned(),
                    Ok(Message::Close(frame)) => {
                        let (code, reason) = frame
                            .map(|f| (Some(u16::from(f.code)), f.reason.to_string()))
                            .unwrap_or((None, String::new()));
                        return Err(classify_close(code, &reason, true));
                    }
                    Ok(_) => continue,
                    Err(e) => return Err(ConnectError::Other(format!("Gemini connection failed: {}", e))),
                };
                if parse_server_message(&text).contains(&ServerEvent::SetupComplete) {
                    return Ok(socket);
                }
            }
            Err(ConnectError::Other("Gemini closed the connection during setup".to_string()))
        };
        tokio::time::timeout(CONNECT_TIMEOUT, attempt)
            .await
            .unwrap_or_else(|_| Err(ConnectError::Other("Gemini didn't finish setup within 15 s".to_string())))
    }

    /// Start connecting with the current key in the background.
    fn spawn_connect(&self) -> ConnectTask {
        let index = self.key_index;
        let key = self.keys[index].clone();
        let url = self.base_url.clone();
        let setup = setup_message(&self.language, &self.config);
        tokio::spawn(async move { (index, Self::connect(&key, &url, setup).await) })
    }

    /// Start the reader and writer for a connected socket. A session already
    /// open (a rotation) finishes its last turn in the background.
    fn install_session(&mut self, socket: GeminiSocket, key_index: usize) {
        let Some(result_tx) = self.result_tx.clone() else { return };
        let (write_half, read_half) = socket.split();
        let (audio_tx, audio_rx) = mpsc::channel::<Vec<u8>>(AUDIO_QUEUE_CHUNKS);
        let turns = Arc::new(Mutex::new(TurnTracker::new(&self.party, &self.language)));
        let flags = Arc::new(SessionFlags::default());
        let reader = spawn_reader_task(read_half, result_tx.clone(), Arc::clone(&turns), Arc::clone(&flags), self.party.clone());
        spawn_writer_task(write_half, audio_rx, self.party.clone());
        let new = Session { audio_tx, reader, turns, flags, opened_at: Instant::now() };
        if let Some(old) = self.session.replace(new) {
            log::info!("GeminiLiveSTT: swapped in a new session ({}) after {} s", self.party, old.opened_at.elapsed().as_secs());
            tokio::spawn(old.finish(Some(result_tx)));
        } else {
            self.emit_status("connected", None);
        }
        self.key_index = key_index;
        self.rate_limited_in_a_row = 0;
        self.reconnect_attempt = 0;
        self.next_reconnect_at = None;
        self.next_rotate_at = None;
        self.dropped_chunks = 0;
        self.dropped_samples_streak = 0;
    }

    fn next_key(&mut self) {
        if self.keys.len() > 1 {
            self.key_index = (self.key_index + 1) % self.keys.len();
            log::info!("GeminiLiveSTT: moving to key {} of {} ({})", self.key_index + 1, self.keys.len(), self.party);
        }
    }

    /// Handle a failed attempt with `index`'s key. Returns true to retry at
    /// once with another key, false to back off.
    fn on_connect_error(&mut self, index: usize, error: ConnectError) -> bool {
        match error {
            ConnectError::RateLimited(msg) => {
                log::warn!("GeminiLiveSTT: key {} rate limited ({}): {}", index + 1, self.party, msg);
                self.key_index = index;
                self.next_key();
                self.rate_limited_in_a_row += 1;
                if self.rate_limited_in_a_row < self.keys.len() {
                    return true;
                }
                self.rate_limited_in_a_row = 0;
                self.emit_status("reconnecting", Some("Every Gemini key is rate limited — retrying".to_string()));
                false
            }
            ConnectError::Auth(msg) => {
                log::warn!("GeminiLiveSTT: key {} rejected ({}): {}", index + 1, self.party, msg);
                if index < self.keys.len() {
                    self.keys.remove(index);
                }
                if self.keys.is_empty() {
                    self.reconnect_blocked = true;
                    self.emit_status("error", Some("Gemini rejected the API key — check it in Settings → STT".to_string()));
                    return false;
                }
                self.key_index = index % self.keys.len();
                true
            }
            ConnectError::Other(msg) => {
                log::warn!("GeminiLiveSTT: connection failed ({}): {}", self.party, msg);
                false
            }
        }
    }

    fn mark_disconnected(&mut self) {
        let Some(session) = self.session.take() else { return };
        let rate_limited = session.flags.rate_limited.load(Ordering::SeqCst);
        session.reader.abort();
        let last = lock(&session.turns).finish_open_turn(epoch_now_ms());
        if let (Some(last), Some(tx)) = (last, self.result_tx.as_ref()) {
            let _ = tx.try_send(last);
        }
        if let Some(task) = self.rotate_task.take() {
            task.abort();
        }
        if rate_limited {
            self.next_key();
        }
        log::warn!("GeminiLiveSTT: connection lost ({}), reconnecting in the background", self.party);
        self.reconnect_attempt = 0;
        self.next_reconnect_at = None;
        self.emit_status("reconnecting", Some("Connection lost — reconnecting".to_string()));
    }

    /// Finished connect task's result, without waiting.
    fn take_finished(task: &mut Option<ConnectTask>) -> Option<Option<(usize, Result<GeminiSocket, ConnectError>)>> {
        if !task.as_ref()?.is_finished() {
            return None;
        }
        // Unconstrained: with the coop budget used up, polling a finished
        // JoinHandle can still return Pending (and drop the socket).
        Some(task.take().and_then(|t| tokio::task::unconstrained(t).now_or_never()).and_then(Result::ok))
    }

    /// Advance the background reconnect without waiting.
    fn drive_reconnect(&mut self) {
        if self.reconnect_blocked || self.result_tx.is_none() || self.keys.is_empty() {
            return;
        }
        if self.reconnect_task.is_some() {
            let Some(done) = Self::take_finished(&mut self.reconnect_task) else { return };
            match done {
                Some((index, Ok(socket))) => {
                    log::info!("GeminiLiveSTT: connected ({}) with key {}", self.party, index + 1);
                    self.flush_after_connect(socket, index);
                    return;
                }
                Some((index, Err(e))) => {
                    if self.on_connect_error(index, e) {
                        self.next_reconnect_at = None;
                    } else if !self.reconnect_blocked {
                        self.reconnect_attempt += 1;
                        let wait = reconnect_backoff(self.reconnect_attempt);
                        self.next_reconnect_at = Some(Instant::now() + wait);
                        self.emit_status("reconnecting", Some(format!("Retrying in {} s", wait.as_secs())));
                    }
                }
                None => {
                    self.reconnect_attempt += 1;
                    self.next_reconnect_at = Some(Instant::now() + reconnect_backoff(self.reconnect_attempt));
                }
            }
            if self.reconnect_blocked {
                return;
            }
        }
        if self.next_reconnect_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        self.reconnect_task = Some(self.spawn_connect());
    }

    fn flush_after_connect(&mut self, socket: GeminiSocket, index: usize) {
        self.install_session(socket, index);
        let Some(since) = self.resuming_since.take() else { return };
        let held = std::mem::take(&mut self.resume_hold);
        if let Some(session) = self.session.as_ref() {
            for frame in held.chunks(SAMPLE_RATE as usize / 10) {
                let _ = session.audio_tx.try_send(pcm_to_bytes(frame));
            }
        }
        log::info!(
            "GeminiLiveSTT: resumed ({}): connected in {} ms, sent {} ms of held audio",
            self.party,
            since.elapsed().as_millis(),
            held.len() * 1000 / SAMPLE_RATE as usize
        );
    }

    /// Open a replacement before Google ends the session, and swap it in.
    fn drive_rotation(&mut self) {
        if self.rotate_task.is_some() {
            let Some(done) = Self::take_finished(&mut self.rotate_task) else { return };
            match done {
                Some((index, Ok(socket))) => self.install_session(socket, index),
                Some((index, Err(e))) => {
                    self.on_connect_error(index, e);
                    self.next_rotate_at = Some(Instant::now() + ROTATE_RETRY);
                }
                None => self.next_rotate_at = Some(Instant::now() + ROTATE_RETRY),
            }
            return;
        }
        let Some(session) = self.session.as_ref() else { return };
        let age = session.opened_at.elapsed();
        let due = session.flags.go_away.load(Ordering::SeqCst)
            || age >= FORCE_ROTATE_AFTER
            || (age >= ROTATE_AFTER && !lock(&session.turns).is_open());
        if due && !self.reconnect_blocked && !self.next_rotate_at.is_some_and(|at| Instant::now() < at) {
            log::info!("GeminiLiveSTT: opening a replacement session ({}) after {} s", self.party, age.as_secs());
            self.rotate_task = Some(self.spawn_connect());
        }
    }

    fn abort_tasks(&mut self) {
        for task in [self.reconnect_task.take(), self.rotate_task.take()].into_iter().flatten() {
            task.abort();
        }
        self.next_reconnect_at = None;
        self.next_rotate_at = None;
    }

    fn pause(&mut self) {
        self.paused = true;
        self.resuming_since = None;
        self.resume_hold.clear();
        self.abort_tasks();
        if let Some(session) = self.session.take() {
            tokio::spawn(session.finish(self.result_tx.clone()));
            log::info!("GeminiLiveSTT: paused ({}): stream closed while muted", self.party);
        }
        self.emit_status("paused", Some("Muted: not streaming to Gemini".to_string()));
    }

    fn resume(&mut self) {
        self.paused = false;
        if self.result_tx.is_none() || self.reconnect_blocked {
            return;
        }
        self.resuming_since = Some(Instant::now());
        self.reconnect_attempt = 0;
        self.next_reconnect_at = None;
        self.drive_reconnect();
    }
}

impl Default for GeminiLiveSTT {
    fn default() -> Self {
        Self::new()
    }
}

fn spawn_reader_task(
    mut read_half: futures::stream::SplitStream<GeminiSocket>,
    result_tx: mpsc::Sender<TranscriptResult>,
    turns: Arc<Mutex<TurnTracker>>,
    flags: Arc<SessionFlags>,
    party: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(message) = read_half.next().await {
            let text = match message {
                Ok(Message::Text(t)) => t.to_string(),
                Ok(Message::Binary(b)) => String::from_utf8_lossy(&b).into_owned(),
                Ok(Message::Close(frame)) => {
                    let (code, reason) = frame.map(|f| (Some(u16::from(f.code)), f.reason.to_string())).unwrap_or((None, String::new()));
                    log::info!("GeminiLiveSTT: closed by Gemini ({}): {:?} {}", party, code, reason);
                    if is_rate_limit(&reason) {
                        flags.rate_limited.store(true, Ordering::SeqCst);
                    }
                    break;
                }
                Ok(_) => continue,
                Err(e) => {
                    log::warn!("GeminiLiveSTT: read error ({}): {}", party, e);
                    break;
                }
            };
            for event in parse_server_message(&text) {
                if event == ServerEvent::GoAway {
                    log::info!("GeminiLiveSTT: goAway ({}); swapping sessions", party);
                    flags.go_away.store(true, Ordering::SeqCst);
                }
                let result = lock(&turns).on_event(&event, epoch_now_ms());
                if let Some(result) = result {
                    if result_tx.send(result).await.is_err() {
                        return;
                    }
                }
            }
        }
        let last = lock(&turns).finish_open_turn(epoch_now_ms());
        if let Some(result) = last {
            log::info!("GeminiLiveSTT: connection ended mid-turn ({}); kept the turn's latest text", party);
            let _ = result_tx.send(result).await;
        }
    })
}

/// Send audio in 100 ms frames, filling gaps with silence so Gemini's speech
/// detection can end a turn while no audio arrives. When the audio channel
/// closes, send audioStreamEnd so Gemini finishes the open turn.
fn spawn_writer_task(
    mut write_half: SplitSink<GeminiSocket, Message>,
    mut audio_rx: mpsc::Receiver<Vec<u8>>,
    party: String,
) {
    tokio::spawn(async move {
        let mut gap_check = tokio::time::interval(GAP_CHECK_INTERVAL);
        gap_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_sent = Instant::now();
        let mut buffer: Vec<u8> = Vec::with_capacity(FRAME_BYTES * 2);
        loop {
            tokio::select! {
                audio = audio_rx.recv() => match audio {
                    Some(bytes) => {
                        buffer.extend_from_slice(&bytes);
                        if buffer.len() < FRAME_BYTES {
                            continue;
                        }
                    }
                    None => break,
                },
                _ = gap_check.tick() => {
                    if buffer.is_empty() {
                        let samples = silence_samples_for(last_sent.elapsed());
                        if samples == 0 {
                            continue;
                        }
                        buffer.resize(samples * 2, 0);
                    } else if last_sent.elapsed() < GAP_CHECK_INTERVAL {
                        continue;
                    }
                }
            }
            let frame = audio_message(&buffer);
            buffer.clear();
            if let Err(e) = write_half.send(Message::Text(frame.into())).await {
                log::warn!("GeminiLiveSTT: write error ({}): {}", party, e);
                return;
            }
            last_sent = Instant::now();
        }
        if !buffer.is_empty() {
            let _ = write_half.send(Message::Text(audio_message(&buffer).into())).await;
        }
        if let Err(e) = write_half.send(Message::Text(AUDIO_STREAM_END.into())).await {
            log::debug!("GeminiLiveSTT: couldn't send audioStreamEnd ({}): {}", party, e);
        }
    });
}

#[async_trait]
impl STTProvider for GeminiLiveSTT {
    fn provider_name(&self) -> &str {
        "Gemini Live"
    }

    fn provider_type(&self) -> STTProviderType {
        STTProviderType::GeminiLive
    }

    async fn start_stream(
        &mut self,
        result_tx: mpsc::Sender<TranscriptResult>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.is_streaming() {
            return Err("Stream already active".into());
        }
        if self.keys.is_empty() {
            return Err("Gemini Live needs an API key. Add one in Settings → STT.".into());
        }
        self.abort_tasks();
        self.reconnect_blocked = false;
        self.result_tx = Some(result_tx);
        // "You" on the first key, everyone else on the second, when there are two.
        self.key_index = usize::from(self.party != "You") % self.keys.len();
        if self.paused {
            log::info!("GeminiLiveSTT: muted at start ({}); connecting on unmute", self.party);
            self.emit_status("paused", Some("Muted: not streaming to Gemini".to_string()));
            return Ok(());
        }
        log::info!(
            "GeminiLiveSTT: connecting ({}, {} key(s), language {}, {} vocabulary terms, {})",
            self.party, self.keys.len(), self.language, self.config.vocabulary.len(), self.config.mode
        );
        self.emit_status("connecting", None);
        // Try each key once; a rejected or rate-limited key moves to the next.
        loop {
            let index = self.key_index;
            let setup = setup_message(&self.language, &self.config);
            match Self::connect(&self.keys[index], &self.base_url, setup).await {
                Ok(socket) => {
                    self.install_session(socket, index);
                    return Ok(());
                }
                Err(e) => {
                    let auth = matches!(e, ConnectError::Auth(_));
                    let msg = e.to_string();
                    if self.on_connect_error(index, e) {
                        continue;
                    }
                    if auth || self.reconnect_blocked {
                        self.result_tx = None;
                        return Err(msg.into());
                    }
                    // Network trouble or every key rate limited: keep the party
                    // and connect in the background.
                    self.reconnect_attempt = 1;
                    self.next_reconnect_at = Some(Instant::now() + reconnect_backoff(1));
                    self.emit_status("reconnecting", Some(msg));
                    return Ok(());
                }
            }
        }
    }

    async fn feed_audio(&mut self, chunk: AudioChunk) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.result_tx.is_none() || chunk.pcm_data.is_empty() || self.paused {
            return Ok(());
        }
        if self.session.as_ref().is_some_and(Session::ended) {
            self.mark_disconnected();
        }
        if self.session.is_none() {
            self.drive_reconnect();
            if self.session.is_none() {
                if self.resuming_since.is_some() {
                    hold_for_resume(&mut self.resume_hold, &chunk.pcm_data);
                }
                return Ok(());
            }
        }
        self.drive_rotation();
        let Some(session) = self.session.as_ref() else { return Ok(()) };
        let samples = chunk.pcm_data.len();
        match session.audio_tx.try_send(pcm_to_bytes(&chunk.pcm_data)) {
            Ok(()) => self.dropped_samples_streak = 0,
            Err(TrySendError::Full(_)) => {
                self.dropped_chunks += 1;
                self.dropped_samples_streak += samples;
                if self.dropped_chunks.is_power_of_two() {
                    log::warn!("GeminiLiveSTT: sending is behind ({}); {} audio chunks dropped", self.party, self.dropped_chunks);
                }
                if self.dropped_samples_streak > STALLED_SAMPLES {
                    log::warn!("GeminiLiveSTT: writer stalled for 5 s of audio ({})", self.party);
                    self.mark_disconnected();
                }
            }
            Err(TrySendError::Closed(_)) => self.mark_disconnected(),
        }
        Ok(())
    }

    async fn stop_stream(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.abort_tasks();
        self.resuming_since = None;
        self.resume_hold.clear();
        if let Some(session) = self.session.take() {
            session.finish(self.result_tx.clone()).await;
        }
        self.result_tx = None;
        self.emit_status("disconnected", None);
        log::info!("GeminiLiveSTT: stream stopped ({})", self.party);
        Ok(())
    }

    /// Open a session with every key (no audio is sent, so nothing is billed)
    /// and name the keys that fail.
    async fn test_connection(&self) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        if self.keys.is_empty() {
            return Err("No Gemini API key configured".into());
        }
        let mut failures = Vec::new();
        for (i, key) in self.keys.iter().enumerate() {
            match Self::connect(key, &self.base_url, setup_message(&self.language, &self.config)).await {
                Ok(mut socket) => {
                    let _ = socket.close(None).await;
                }
                // The key works; it is only out of quota for now.
                Err(ConnectError::RateLimited(_)) => {}
                Err(e) => failures.push(format!("key {}: {}", i + 1, e)),
            }
        }
        if failures.is_empty() {
            Ok(true)
        } else {
            Err(format!("Gemini Live: {}", failures.join("; ")).into())
        }
    }

    fn set_language(&mut self, language: &str) {
        self.language = language.to_string();
    }

    fn set_muted(&mut self, muted: bool) {
        if muted == self.paused {
            return;
        }
        if muted {
            self.pause();
        } else {
            self.resume();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioSource;
    use tokio::net::TcpListener;

    fn chunk(samples: Vec<i16>) -> AudioChunk {
        AudioChunk { pcm_data: samples, source: AudioSource::System, timestamp_ms: 0, is_speech: true }
    }

    fn text(json: &str) -> Message {
        Message::Text(json.to_string().into())
    }

    async fn local_server() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/ws", listener.local_addr().unwrap());
        (listener, url)
    }

    async fn next_result(rx: &mut mpsc::Receiver<TranscriptResult>) -> TranscriptResult {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("a result within 5 s")
            .expect("result channel open")
    }

    /// Accept one connection, record the key header, expect the setup message
    /// and answer setupComplete.
    async fn accept_session(
        listener: &TcpListener,
        keys_seen: Option<mpsc::Sender<String>>,
    ) -> WebSocketStream<TcpStream> {
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_hdr_async(socket, move |req: &Request, resp: Response| {
            let key = req.headers().get("x-goog-api-key").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            if let Some(tx) = keys_seen {
                let _ = tx.try_send(key);
            }
            Ok(resp)
        })
        .await
        .unwrap();
        let Some(Ok(Message::Text(setup))) = ws.next().await else { panic!("no setup message") };
        assert!(setup.contains("\"setup\""));
        ws.send(text(r#"{"setupComplete":{}}"#)).await.unwrap();
        ws
    }

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(&[0xff, 0xfe, 0x00, 0x01]), "//4AAQ==");
    }

    #[test]
    fn setup_carries_language_vocabulary_and_mode() {
        let config = GeminiLiveConfig { vocabulary: vec!["NexQ".into()], mode: "SMART".into() };
        let v: Value = serde_json::from_str(&setup_message("en-IN", &config)).unwrap();
        assert_eq!(v["setup"]["model"], "models/gemini-3.5-transcribe-live");
        assert_eq!(v["setup"]["inputAudioTranscription"]["languageCodes"], json!(["en-IN"]));
        assert_eq!(v["setup"]["inputAudioTranscription"]["customVocabulary"], json!(["NexQ"]));
        assert_eq!(v["setup"]["inputAudioTranscription"]["mode"], "SMART");
        assert!(v["setup"].get("realtimeInputConfig").is_none(), "Gemini's own speech detection stays on");

        let v: Value = serde_json::from_str(&setup_message("auto", &GeminiLiveConfig::default())).unwrap();
        assert_eq!(v["setup"]["inputAudioTranscription"]["languageCodes"], json!([]));
        assert!(v["setup"]["inputAudioTranscription"].get("customVocabulary").is_none());
    }

    #[test]
    fn config_is_normalized() {
        let c = GeminiLiveConfig { vocabulary: vec![" NexQ ".into(), "".into(), "nexq".into(), "BM25".into()], mode: "loud".into() }
            .normalized();
        assert_eq!(c.vocabulary, ["NexQ", "BM25"]);
        assert_eq!(c.mode, "VERBATIM");
        let many = GeminiLiveConfig { vocabulary: (0..1500).map(|i| format!("t{i}")).collect(), ..Default::default() };
        assert_eq!(many.normalized().vocabulary.len(), MAX_VOCABULARY_TERMS);
        let partial: GeminiLiveConfig = serde_json::from_str(r#"{"vocabulary":["a"]}"#).unwrap();
        assert_eq!(partial.mode, "VERBATIM");
    }

    #[test]
    fn keys_come_one_per_line_with_the_llm_key_as_fallback() {
        assert_eq!(parse_keys(" a \r\nb\n\na, c"), ["a", "b", "c"]);
        let store = |own: Option<&'static str>| {
            move |name: &str| match name {
                ID => own.map(str::to_string),
                FALLBACK_KEY_CREDENTIAL => Some("llm-key".to_string()),
                _ => None,
            }
        };
        assert_eq!(keys_from_store(store(Some("k1\nk2"))), ["k1", "k2"]);
        assert_eq!(keys_from_store(store(Some("  "))), ["llm-key"]);
        assert_eq!(keys_from_store(store(None)), ["llm-key"]);
    }

    #[test]
    fn parses_server_frames() {
        assert_eq!(parse_server_message(r#"{"setupComplete":{}}"#), [ServerEvent::SetupComplete]);
        assert_eq!(
            parse_server_message(r#"{"serverContent":{"interimInputTranscription":{"text":"Okay te"}}}"#),
            [ServerEvent::Interim("Okay te".into())]
        );
        assert_eq!(
            parse_server_message(r#"{"serverContent":{"inputTranscription":{"text":"Okay team."},"generationComplete":true}}"#),
            [ServerEvent::Final("Okay team.".into())]
        );
        assert_eq!(parse_server_message(r#"{"goAway":{"timeLeft":"50s"}}"#), [ServerEvent::GoAway]);
        assert!(parse_server_message("not json").is_empty());
    }

    #[test]
    fn closes_are_classified() {
        assert!(matches!(classify_close(Some(1007), "API key not valid", true), ConnectError::Auth(_)));
        assert!(matches!(classify_close(Some(1008), "after GoAway", false), ConnectError::Other(_)));
        assert!(matches!(classify_close(Some(1011), "RESOURCE_EXHAUSTED: Quota exceeded", false), ConnectError::RateLimited(_)));
        assert!(matches!(classify_close(Some(1008), "You exceeded your current quota", true), ConnectError::RateLimited(_)));
    }

    #[test]
    fn interims_grow_into_one_final() {
        let mut t = TurnTracker::new("Them", "en-IN");
        let a = t.on_event(&ServerEvent::Interim("Kya aap".into()), 100).unwrap();
        let b = t.on_event(&ServerEvent::Interim("Kya aap Friday tak".into()), 600).unwrap();
        assert!(!a.is_final && !b.is_final);
        assert_eq!((b.text.as_str(), b.timestamp_ms), ("Kya aap Friday tak", 100));
        assert!(t.is_open());
        let f = t.on_event(&ServerEvent::Final("Kya aap Friday tak fix kar sakte ho?".into()), 2_100).unwrap();
        assert!(f.is_final);
        assert_eq!((f.timestamp_ms, f.duration_ms), (100, Some(2_000)));
        assert_eq!(f.language.as_deref(), Some("en-IN"));
        assert!(!t.is_open());
        assert!(t.finish_open_turn(3_000).is_none());
    }

    #[test]
    fn an_unfinished_turn_is_kept_once() {
        let mut t = TurnTracker::new("You", "auto");
        t.on_event(&ServerEvent::Interim("half a sente".into()), 5);
        let kept = t.finish_open_turn(50).unwrap();
        assert_eq!((kept.text.as_str(), kept.is_final, kept.language), ("half a sente", true, None));
        assert!(t.finish_open_turn(60).is_none());
    }

    /// Audio arrives as base64 JSON frames, results come back, and stop sends
    /// audioStreamEnd and waits for the last final.
    #[tokio::test]
    async fn streams_audio_and_returns_the_final_on_stop() {
        let (listener, url) = local_server().await;
        let (seen_tx, mut seen_rx) = mpsc::channel::<String>(64);
        let (keys_tx, mut keys_rx) = mpsc::channel::<String>(4);
        tokio::spawn(async move {
            let mut ws = accept_session(&listener, Some(keys_tx)).await;
            while let Some(Ok(Message::Text(t))) = ws.next().await {
                let v: Value = serde_json::from_str(&t).unwrap();
                if let Some(data) = v.pointer("/realtimeInput/audio/data").and_then(Value::as_str) {
                    if data.chars().all(|c| c == 'A' || c == '=') {
                        continue; // gap-filling silence
                    }
                    seen_tx.send(format!("audio={data}")).await.unwrap();
                    ws.send(text(r#"{"serverContent":{"interimInputTranscription":{"text":"Hello"}}}"#)).await.unwrap();
                } else if v.pointer("/realtimeInput/audioStreamEnd").is_some() {
                    seen_tx.send("end".into()).await.unwrap();
                    ws.send(text(r#"{"serverContent":{"inputTranscription":{"text":"Hello there."}}}"#)).await.unwrap();
                }
            }
        });

        let mut stt = GeminiLiveSTT::for_endpoint(&["key-1"], &url);
        stt.set_party("You");
        let (tx, mut rx) = mpsc::channel(16);
        stt.start_stream(tx).await.unwrap();
        stt.feed_audio(chunk(vec![1, 2])).await.unwrap();
        stt.stop_stream().await.unwrap();

        assert_eq!(keys_rx.recv().await.unwrap(), "key-1");
        assert_eq!(seen_rx.recv().await.unwrap(), format!("audio={}", base64_encode(&[1, 0, 2, 0])));
        assert_eq!(seen_rx.recv().await.unwrap(), "end");
        let interim = next_result(&mut rx).await;
        assert_eq!((interim.text.as_str(), interim.is_final), ("Hello", false));
        let last = next_result(&mut rx).await;
        assert_eq!((last.text.as_str(), last.is_final, last.speaker.as_deref()), ("Hello there.", true, Some("You")));
    }

    /// "Them" starts on the second key; a rate-limited key moves on to the next.
    #[tokio::test]
    async fn rate_limited_key_moves_to_the_next() {
        let (listener, url) = local_server().await;
        let (keys_tx, mut keys_rx) = mpsc::channel::<String>(8);
        tokio::spawn(async move {
            let ws = accept_session_closing(&listener, &keys_tx, 1011, "RESOURCE_EXHAUSTED: quota").await;
            drop(ws);
            let _ws = accept_session(&listener, Some(keys_tx)).await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let mut stt = GeminiLiveSTT::for_endpoint(&["k1", "k2", "k3"], &url);
        stt.set_party("Them");
        let (tx, _rx) = mpsc::channel(16);
        stt.start_stream(tx).await.unwrap();
        assert_eq!(keys_rx.recv().await.unwrap(), "k2");
        assert_eq!(keys_rx.recv().await.unwrap(), "k3");
        assert!(stt.is_streaming());
        assert_eq!(stt.key_index, 2);
    }

    /// Accept, read setup, then close with `code` instead of setupComplete.
    async fn accept_session_closing(
        listener: &TcpListener,
        keys_seen: &mpsc::Sender<String>,
        code: u16,
        reason: &'static str,
    ) -> WebSocketStream<TcpStream> {
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
        use tokio_tungstenite::tungstenite::protocol::frame::{coding::CloseCode, CloseFrame};
        let (socket, _) = listener.accept().await.unwrap();
        let keys = keys_seen.clone();
        let mut ws = tokio_tungstenite::accept_hdr_async(socket, move |req: &Request, resp: Response| {
            let key = req.headers().get("x-goog-api-key").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            let _ = keys.try_send(key);
            Ok(resp)
        })
        .await
        .unwrap();
        let _ = ws.next().await;
        let _ = ws.close(Some(CloseFrame { code: CloseCode::from(code), reason: reason.into() })).await;
        ws
    }

    /// A rejected key is dropped; with no keys left start fails without retrying.
    #[tokio::test]
    async fn a_rejected_only_key_fails_the_start() {
        let (listener, url) = local_server().await;
        let (keys_tx, _keys_rx) = mpsc::channel::<String>(8);
        tokio::spawn(async move {
            let _ws = accept_session_closing(&listener, &keys_tx, 1007, "API key not valid").await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        let mut stt = GeminiLiveSTT::for_endpoint(&["bad"], &url);
        let (tx, _rx) = mpsc::channel(16);
        let err = stt.start_stream(tx).await.unwrap_err().to_string();
        assert!(err.contains("1007"), "{err}");
        assert!(stt.keys.is_empty() && stt.reconnect_blocked && !stt.is_streaming());
    }

    #[tokio::test]
    async fn no_key_means_no_connection_attempt() {
        let mut stt = GeminiLiveSTT::new();
        let (tx, _rx) = mpsc::channel(16);
        assert!(stt.start_stream(tx).await.unwrap_err().to_string().contains("needs an API key"));
        assert!(stt.test_connection().await.is_err());
    }

    /// After goAway a replacement session is opened and swapped in without
    /// blocking feed_audio; the old session's turn still arrives as a final.
    #[tokio::test]
    async fn go_away_swaps_in_a_new_session() {
        let (listener, url) = local_server().await;
        tokio::spawn(async move {
            let mut old = accept_session(&listener, None).await;
            old.send(text(r#"{"serverContent":{"interimInputTranscription":{"text":"old turn"}}}"#)).await.unwrap();
            old.send(text(r#"{"goAway":{"timeLeft":"50s"}}"#)).await.unwrap();
            let mut new = accept_session(&listener, None).await;
            // The old session gets audioStreamEnd and finishes its turn.
            tokio::spawn(async move {
                while let Some(Ok(Message::Text(t))) = old.next().await {
                    if t.contains("audioStreamEnd") {
                        old.send(text(r#"{"serverContent":{"inputTranscription":{"text":"old turn done."}}}"#)).await.unwrap();
                    }
                }
            });
            let mut answered = false;
            while let Some(Ok(Message::Text(t))) = new.next().await {
                if !answered && t.contains("\"audio\"") {
                    answered = true;
                    new.send(text(r#"{"serverContent":{"inputTranscription":{"text":"new session."}}}"#)).await.unwrap();
                }
            }
        });

        let mut stt = GeminiLiveSTT::for_endpoint(&["k"], &url);
        let (tx, mut rx) = mpsc::channel(64);
        stt.start_stream(tx).await.unwrap();
        let mut finals = Vec::new();
        let mut slowest_feed = Duration::ZERO;
        let deadline = Instant::now() + Duration::from_secs(8);
        while finals.len() < 2 && Instant::now() < deadline {
            let started = Instant::now();
            stt.feed_audio(chunk(vec![7; 160])).await.unwrap();
            slowest_feed = slowest_feed.max(started.elapsed());
            while let Ok(r) = rx.try_recv() {
                if r.is_final {
                    finals.push(r.text);
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        finals.sort();
        assert_eq!(finals, ["new session.", "old turn done."]);
        assert!(slowest_feed < Duration::from_millis(50), "feed_audio blocked for {:?}", slowest_feed);
        stt.stop_stream().await.unwrap();
    }

    /// A dropped connection keeps its turn and reconnects in the background.
    #[tokio::test]
    async fn reconnects_after_the_server_drops() {
        let (listener, url) = local_server().await;
        tokio::spawn(async move {
            let mut ws = accept_session(&listener, None).await;
            ws.send(text(r#"{"serverContent":{"interimInputTranscription":{"text":"lost words"}}}"#)).await.unwrap();
            let _ = ws.next().await;
            drop(ws);
            let mut ws = accept_session(&listener, None).await;
            ws.send(text(r#"{"serverContent":{"inputTranscription":{"text":"after reconnect"}}}"#)).await.unwrap();
            while ws.next().await.is_some() {}
        });
        let mut stt = GeminiLiveSTT::for_endpoint(&["k"], &url);
        let (tx, mut rx) = mpsc::channel(64);
        stt.start_stream(tx).await.unwrap();
        let mut finals = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(8);
        while !finals.iter().any(|t: &String| t == "after reconnect") && Instant::now() < deadline {
            stt.feed_audio(chunk(vec![7; 160])).await.unwrap();
            while let Ok(r) = rx.try_recv() {
                if r.is_final {
                    finals.push(r.text);
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(finals, ["lost words", "after reconnect"]);
        stt.stop_stream().await.unwrap();
    }

    /// Live check against Google with a real key: GEMINI_LIVE_KEY and a 16 kHz
    /// mono WAV in GEMINI_LIVE_WAV. Run with `--ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_transcribes_a_clip() {
        let (Ok(key), Ok(wav)) = (std::env::var("GEMINI_LIVE_KEY"), std::env::var("GEMINI_LIVE_WAV")) else {
            panic!("set GEMINI_LIVE_KEY and GEMINI_LIVE_WAV");
        };
        let bytes = std::fs::read(wav).unwrap();
        let samples: Vec<i16> = bytes[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        let mut stt = GeminiLiveSTT::with_api_keys(vec![key]);
        stt.set_language(&std::env::var("GEMINI_LIVE_LANG").unwrap_or_else(|_| "en-IN".into()));
        let (tx, mut rx) = mpsc::channel(1024);
        stt.start_stream(tx).await.unwrap();
        let started = Instant::now();
        let mut interims = 0;
        for (i, piece) in samples.chunks(160).enumerate() {
            stt.feed_audio(chunk(piece.to_vec())).await.unwrap();
            tokio::time::sleep_until((started + Duration::from_millis(10 * (i as u64 + 1))).into()).await;
            while let Ok(r) = rx.try_recv() {
                if r.is_final {
                    println!("[{:5.1}s] FINAL   {}", started.elapsed().as_secs_f32(), r.text);
                } else {
                    interims += 1;
                }
            }
        }
        stt.stop_stream().await.unwrap();
        while let Ok(r) = rx.try_recv() {
            println!("[{:5.1}s] {} {}", started.elapsed().as_secs_f32(), if r.is_final { "FINAL  " } else { "interim" }, r.text);
        }
        println!("{interims} interims");
        assert!(interims > 0);
    }
}

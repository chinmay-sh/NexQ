// Automatic meeting copilot (see `crate::copilot`): one check of the latest
// transcript lines, or an immediate answer to a question.

use std::sync::{Arc, Mutex};

use tauri::{command, AppHandle, Manager};
use tokio::sync::Mutex as TokioMutex;

use crate::copilot::{self, CopilotRequest, CopilotResponse, Passage};
use crate::llm::provider::{GenerationParams, LLMMessage, LLMProvider, TokenSink};
use crate::state::AppState;

/// Most knowledge-base passages offered per check.
const MAX_PASSAGES: usize = 6;
/// Transcript text used as the knowledge-base query.
const QUERY_CHARS: usize = 600;

/// Check the new transcript lines (or answer `request.question`) and return
/// new notes and cards. Uses the active LLM provider and model, or
/// `request.model`, through its own client so it never waits on (or delays)
/// an answer the user asked for.
#[command]
pub async fn copilot_tick(app: AppHandle, request: CopilotRequest) -> Result<CopilotResponse, String> {
    if request.new_lines.trim().is_empty() && request.question().is_none() {
        return Ok(CopilotResponse::default());
    }
    let state = app.state::<AppState>();

    // The user's files: BM25 (or hybrid) passages matching the question or what was just said.
    let query = request.question().map(str::to_string);
    let excerpt = tail(&request.new_lines, QUERY_CHARS);
    let top_k = state
        .rag
        .as_ref()
        .and_then(|r| r.lock().ok().map(|r| r.config().top_k))
        .unwrap_or(5)
        .min(MAX_PASSAGES);
    let passages: Vec<Passage> = match crate::commands::intelligence_commands::retrieve_rag(&state, query, excerpt, top_k).await {
        Ok(found) => found.chunks.into_iter().map(|c| Passage { source: c.source, text: c.text }).collect(),
        Err(e) => {
            log::warn!("Copilot: knowledge-base search failed: {}", e);
            Vec::new()
        }
    };

    let (system, user) = copilot::build_messages(&request, &passages);
    let messages = vec![
        LLMMessage { role: "system".into(), content: system },
        LLMMessage { role: "user".into(), content: user },
    ];
    let (provider, model) = copilot_provider(&state, request.model.as_deref())?;
    let reply = complete_silently(&app, provider, &model, messages, request.web_search).await?;
    let response = copilot::parse_response(&reply, &request, &passages)?;
    log::info!(
        "Copilot: {} note(s), {} card(s) from {} passage(s){}",
        response.notes.len(),
        response.cards.len(),
        passages.len(),
        if request.question().is_some() { " (question)" } else { "" }
    );
    Ok(response)
}

fn tail(text: &str, n: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(n)).collect()
}

/// A provider for the copilot and the model to ask. A fresh client built from
/// the active provider's settings, so copilot checks don't hold the shared
/// provider while the user waits on an answer; CLI agents (which manage their
/// own sessions) use the shared one.
fn copilot_provider(
    state: &AppState,
    model_override: Option<&str>,
) -> Result<(Arc<TokioMutex<Box<dyn LLMProvider>>>, String), String> {
    let llm = state.llm.as_ref().ok_or("LLM router not initialized")?;
    let router = llm.lock().map_err(|_| "LLM lock poisoned")?;
    let model = model_override
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or(router.active_model())
        .to_string();
    if model.is_empty() {
        return Err("Choose a model in Settings → AI Providers for the copilot".to_string());
    }
    let own = router
        .active_config()
        .filter(|c| !crate::llm::ProviderType::from_str(&c.provider_type).is_ok_and(|t| t.is_cli_agent()))
        .and_then(|c| crate::llm::create_provider(c).ok());
    let provider = match own {
        Some(p) => Arc::new(TokioMutex::new(p)),
        None => router.get_provider().map_err(|e| format!("LLM not configured: {}", e))?,
    };
    Ok((provider, model))
}

/// Run `messages` without any `llm_stream_*` events and return the full reply.
async fn complete_silently(
    app: &AppHandle,
    provider: Arc<TokioMutex<Box<dyn LLMProvider>>>,
    model: &str,
    messages: Vec<LLMMessage>,
    web_search: bool,
) -> Result<String, String> {
    let buffer = Arc::new(Mutex::new(String::new()));
    let target = buffer.clone();
    let sink: TokenSink = Arc::new(move |token: &str| {
        if let Ok(mut b) = target.lock() {
            b.push_str(token);
        }
    });
    let params = GenerationParams {
        request_id: Some(uuid::Uuid::new_v4().to_string()),
        silent: true,
        enable_web_search: web_search,
        // A background JSON task: no thinking unless the user chose some.
        thinking_auto_off: true,
        ..Default::default()
    };
    provider
        .lock()
        .await
        .stream_completion(messages, model, params, app.clone(), Some(sink))
        .await
        .map_err(|e| format!("Copilot model call failed: {}", e))?;
    let reply = buffer.lock().map_err(|_| "Copilot buffer poisoned")?.clone();
    if reply.trim().is_empty() {
        return Err("The model returned nothing".to_string());
    }
    Ok(reply)
}

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
/// Longest knowledge-base passage (a chunk is ~512 tokens).
const MAX_PASSAGE_CHARS: usize = 2_000;
/// Attached files go in whole, up to this much text in all (~15k tokens).
// shortcut: files past the budget are cut off, not searched; index them (folders) when attachments grow that large.
const MAX_ATTACHED_CHARS: usize = 60_000;
/// Web results added for a question or a requested search.
const MAX_WEB_RESULTS: usize = 5;
/// Credential holding the Tavily API key.
pub const TAVILY_KEY: &str = "tavily";
/// Transcript text used as the knowledge-base query.
const QUERY_CHARS: usize = 600;

/// Check the new transcript lines (or answer `request.question`) and return
/// new notes and cards. Uses the active LLM provider and model, or
/// `request.model`, through its own client so it never waits on (or delays)
/// an answer the user asked for.
#[command]
pub async fn copilot_tick(app: AppHandle, mut request: CopilotRequest) -> Result<CopilotResponse, String> {
    if request.new_lines.trim().is_empty() && request.question().is_none() {
        return Ok(CopilotResponse::default());
    }
    let state = app.state::<AppState>();
    let scenario = state.active_scenario.read().map(|s| s.system_prompt.clone()).unwrap_or_default();
    // What NexQ remembers about the user; its vocabulary fixes misheard names
    // before the knowledge-base search and the prompt see the transcript.
    let memory = crate::memory::load_app(&app);
    let vocabulary = crate::memory::vocabulary(&memory);
    request.new_lines = crate::memory::apply_vocabulary(&request.new_lines, &vocabulary);
    request.recent_context = crate::memory::apply_vocabulary(&request.recent_context, &vocabulary);
    request.question = request.question.map(|q| crate::memory::apply_vocabulary(&q, &vocabulary));

    // Attached files go in whole, like Meetily: a few documents fit, and the
    // model sees all of them instead of a few matching snippets.
    let attached = state
        .context
        .as_ref()
        .and_then(|c| c.lock().ok().map(|c| c.resource_texts()))
        .unwrap_or_default();
    let attached = copilot::attached_passages(attached, MAX_ATTACHED_CHARS);

    // Folder files: BM25 (or hybrid) passages matching the question or what was just said.
    let query = request.question().map(str::to_string);
    let excerpt = tail(&request.new_lines, QUERY_CHARS);
    let top_k = state
        .rag
        .as_ref()
        .and_then(|r| r.lock().ok().map(|r| r.config().top_k))
        .unwrap_or(5)
        .min(MAX_PASSAGES);
    let passages: Vec<Passage> = match crate::commands::intelligence_commands::retrieve_rag(&state, query, excerpt, top_k).await {
        Ok(found) => found
            .chunks
            .into_iter()
            // An attached file that is also indexed is already in whole.
            .filter(|c| !attached.iter().any(|a| a.source == c.source))
            .map(|c| Passage { text: copilot::first_chars(&c.text, MAX_PASSAGE_CHARS).to_string(), source: c.source })
            .collect(),
        Err(e) => {
            log::warn!("Copilot: knowledge-base search failed: {}", e);
            Vec::new()
        }
    };

    let (provider, model, native_search) = copilot_provider(&state, request.model.as_deref())?;
    // Web: the provider's own search (Gemini, OpenRouter), else Tavily when a key is saved.
    let tavily = (request.web_search && !native_search).then(|| credential(&state, TAVILY_KEY)).flatten();
    let mut passages: Vec<Passage> = attached.into_iter().chain(passages).collect();
    if let (Some(key), Some(q)) = (tavily.as_deref(), request.question()) {
        passages.extend(web_passages(key, q).await);
    }

    let ask = |request: &CopilotRequest, passages: &[Passage], offer_search: bool| {
        let (system, user) = copilot::build_messages(request, passages, offer_search, &scenario, &memory);
        vec![LLMMessage { role: "system".into(), content: system }, LLMMessage { role: "user".into(), content: user }]
    };
    let native = request.web_search && native_search;
    let reply = complete_silently(&app, provider.clone(), &model, ask(&request, &passages, tavily.is_some()), native).await?;
    let mut response = copilot::parse_response(&reply, &request, &passages)?;

    // The model asked for a web search: answer it as one more card.
    if let (Some(key), Some(search)) = (tavily.as_deref(), response.search.take()) {
        let web = web_passages(key, &search).await;
        if !web.is_empty() {
            let follow = CopilotRequest { question: Some(search.clone()), ..request.clone() };
            let all: Vec<Passage> = passages.iter().cloned().chain(web).collect();
            match complete_silently(&app, provider, &model, ask(&follow, &all, false), false).await {
                Ok(reply) => match copilot::parse_response(&reply, &follow, &all) {
                    Ok(found) => response.cards.extend(found.cards.into_iter().map(|c| copilot::CopilotCard { kind: "answer".into(), ..c })),
                    Err(e) => log::warn!("Copilot: web follow-up unreadable: {}", e),
                },
                Err(e) => log::warn!("Copilot: web follow-up failed: {}", e),
            }
        }
    }
    log::info!(
        "Copilot: {} note(s), {} card(s) from {} passage(s){}",
        response.notes.len(),
        response.cards.len(),
        passages.len(),
        if request.question().is_some() { " (question)" } else { "" }
    );
    Ok(response)
}

fn credential(state: &AppState, name: &str) -> Option<String> {
    let creds = state.credentials.as_ref()?.lock().ok()?;
    creds.get_key(name).ok().flatten().map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

/// Tavily results for `query` as passages (none on any failure, which is logged).
async fn web_passages(key: &str, query: &str) -> Vec<Passage> {
    match tavily_search(key, query).await {
        Ok(found) => found,
        Err(e) => {
            log::warn!("Copilot: Tavily search failed: {}", e);
            Vec::new()
        }
    }
}

async fn tavily_search(key: &str, query: &str) -> Result<Vec<Passage>, String> {
    let response = reqwest::Client::new()
        .post("https://api.tavily.com/search")
        .bearer_auth(key)
        .json(&serde_json::json!({ "query": query, "max_results": MAX_WEB_RESULTS, "search_depth": "basic" }))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let body: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    if status != 200 {
        return Err(format!("HTTP {}: {}", status, body));
    }
    Ok(copilot::tavily_passages(&body))
}

fn tail(text: &str, n: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(n)).collect()
}

/// A provider for the copilot and the model to ask. A fresh client built from
/// the active provider's settings, so copilot checks don't hold the shared
/// provider while the user waits on an answer; CLI agents (which manage their
/// own sessions) use the shared one.
/// Also says whether the provider searches the web itself (Gemini, OpenRouter).
pub(crate) fn copilot_provider(
    state: &AppState,
    model_override: Option<&str>,
) -> Result<(Arc<TokioMutex<Box<dyn LLMProvider>>>, String, bool), String> {
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
    let native_search = matches!(
        router.active_provider_type(),
        Some(crate::llm::ProviderType::Gemini | crate::llm::ProviderType::Openrouter)
    );
    Ok((provider, model, native_search))
}

/// Run `messages` without any `llm_stream_*` events and return the full reply.
pub(crate) async fn complete_silently(
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

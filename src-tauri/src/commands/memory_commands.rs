// memory.md (see `crate::memory`): read, edit, and update after a meeting.

use tauri::{command, AppHandle, Manager};

use crate::commands::copilot_commands::{complete_silently, copilot_provider};
use crate::llm::provider::LLMMessage;
use crate::state::AppState;

fn memory_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| crate::memory::path(&dir))
        .map_err(|e| format!("App data folder unavailable: {}", e))
}

/// memory.md, or a starting template when there is none yet.
#[command]
pub async fn get_memory(app: AppHandle) -> Result<String, String> {
    let text = crate::memory::load(&memory_path(&app)?);
    Ok(if text.trim().is_empty() { crate::memory::TEMPLATE.to_string() } else { text })
}

/// Save the user's edit of memory.md (the previous version goes to memory.md.bak).
#[command]
pub async fn set_memory(app: AppHandle, text: String) -> Result<(), String> {
    crate::memory::save(&memory_path(&app)?, &text)
}

/// Merge what a finished meeting taught about the user into memory.md and
/// return the new memory. `transcript` is the meeting's lines ("Speaker: text"),
/// `notes` the copilot's notes. Uses the copilot's model (`model`, or the active one).
#[command]
pub async fn update_memory_from_meeting(
    app: AppHandle,
    transcript: String,
    notes: Vec<String>,
    model: Option<String>,
) -> Result<String, String> {
    if transcript.trim().is_empty() {
        return Err("The meeting has no transcript to learn from".to_string());
    }
    let path = memory_path(&app)?;
    let old = crate::memory::load(&path);
    let state = app.state::<AppState>();
    // File names hint at project names that speech recognition mishears.
    let files: Vec<String> = state
        .database
        .as_ref()
        .and_then(|db| db.lock().ok())
        .and_then(|db| {
            let mut stmt = db.connection().prepare("SELECT DISTINCT file_id FROM rag_chunks WHERE source_type = 'file' LIMIT 200").ok()?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).ok()?;
            Some(rows.filter_map(Result::ok).collect::<Vec<_>>())
        })
        .unwrap_or_default()
        .into_iter()
        .map(|id| std::path::Path::new(&id).file_name().map_or(id.clone(), |n| n.to_string_lossy().to_string()))
        .collect();
    let mut names: Vec<String> = state
        .context
        .as_ref()
        .and_then(|c| c.lock().ok().map(|c| c.resource_texts().into_iter().map(|(name, _)| name).collect::<Vec<_>>()))
        .unwrap_or_default();
    names.extend(files.into_iter().filter(|f| f.contains('.')));
    names.dedup();

    let (system, user) = crate::memory::update_messages(&old, &transcript, &notes, &names);
    let (provider, model, _) = copilot_provider(&state, model.as_deref())?;
    let messages = vec![LLMMessage { role: "system".into(), content: system }, LLMMessage { role: "user".into(), content: user }];
    let reply = complete_silently(&app, provider, &model, messages, false).await?;
    let new = crate::memory::accept_update(&old, &reply)?;
    crate::memory::save(&path, &new)?;
    log::info!("Memory updated after a meeting ({} -> {} characters)", old.chars().count(), new.chars().count());
    Ok(new)
}

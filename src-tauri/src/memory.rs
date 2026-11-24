//! memory.md: what NexQ knows about the user, kept in the app data folder.
//!
//! The copilot reads it on every check. When a meeting ends, one model call
//! merges what that meeting taught about the user into it (the previous
//! version is kept as memory.md.bak). The user can edit it in Settings.
//!
//! Its "## Vocabulary" section lists the user's names and terms, one per
//! line, optionally with what speech recognition mishears them as:
//!   - KASH — heard as: cash, cache
//! Those spellings fix the copilot's transcript and knowledge-base search, and
//! the terms are given to Deepgram and Gemini Live as words to listen for.

use std::path::{Path, PathBuf};

const FILE: &str = "memory.md";
/// Most memory text sent to the copilot.
pub const MAX_PROMPT_CHARS: usize = 8_000;
/// Most transcript text sent when updating the memory.
const MAX_TRANSCRIPT_CHARS: usize = 15_000;
/// A reply longer than this isn't a memory file.
const MAX_MEMORY_CHARS: usize = 20_000;
const MAX_TERMS: usize = 100;

pub const TEMPLATE: &str = "# About me\n\n## Vocabulary\n";

pub fn path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(FILE)
}

/// The memory, or "" when there is none yet.
pub fn load(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Save `text`, keeping the previous version as memory.md.bak.
pub fn save(path: &Path, text: &str) -> Result<(), String> {
    if path.exists() {
        let _ = std::fs::copy(path, path.with_extension("md.bak"));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't create {}: {}", dir.display(), e))?;
    }
    std::fs::write(path, text).map_err(|e| format!("Couldn't save the memory: {}", e))
}

/// The memory in the app data folder of `app`.
pub fn load_app(app: &tauri::AppHandle) -> String {
    use tauri::Manager;
    app.path().app_data_dir().map(|dir| load(&path(&dir))).unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Term {
    pub term: String,
    /// What speech recognition turns it into.
    pub heard_as: Vec<String>,
}

/// The entries of the memory's Vocabulary section.
pub fn vocabulary(memory: &str) -> Vec<Term> {
    let mut in_vocabulary = false;
    let mut out = Vec::new();
    for line in memory.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            in_vocabulary = line.to_lowercase().contains("vocabulary");
            continue;
        }
        let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")).filter(|_| in_vocabulary) else {
            continue;
        };
        // ASCII lowercase keeps byte offsets, so the index works on `item`.
        let (term, heard) = match item.to_ascii_lowercase().find("heard as") {
            Some(i) => (&item[..i], &item[i + "heard as".len()..]),
            None => (item, ""),
        };
        let term = term.trim_matches(|c: char| c.is_whitespace() || "—–-:(,".contains(c)).to_string();
        if term.is_empty() || term.chars().count() > 60 {
            continue;
        }
        let heard_as = heard
            .split(',')
            .map(|a| a.trim_matches(|c: char| c.is_whitespace() || ":)\"'.".contains(c)).to_string())
            .filter(|a| !a.is_empty() && !a.eq_ignore_ascii_case(&term))
            .collect();
        out.push(Term { term, heard_as });
        if out.len() >= MAX_TERMS {
            break;
        }
    }
    out
}

/// The vocabulary's terms, for speech recognition (Deepgram keyterms, Gemini Live vocabulary).
pub fn terms(memory: &str) -> Vec<String> {
    vocabulary(memory).into_iter().map(|t| t.term).collect()
}

/// `existing` plus the `extra` words not already in it (ignoring case), at most `max`.
pub fn merge_words(existing: Vec<String>, extra: Vec<String>, max: usize) -> Vec<String> {
    let mut all = existing;
    for word in extra {
        if !all.iter().any(|w| w.eq_ignore_ascii_case(&word)) {
            all.push(word);
        }
    }
    all.truncate(max);
    all
}

/// `text` with every misheard form in `vocabulary` replaced by its term
/// (whole words, ignoring case).
pub fn apply_vocabulary(text: &str, vocabulary: &[Term]) -> String {
    let mut out = text.to_string();
    for t in vocabulary {
        for heard in &t.heard_as {
            out = replace_phrase(&out, heard, &t.term);
        }
    }
    out
}

fn replace_phrase(text: &str, phrase: &str, with: &str) -> String {
    let n = phrase.chars().count();
    if n == 0 {
        return text.to_string();
    }
    let phrase = phrase.to_lowercase();
    let boundary = |c: Option<char>| c.map_or(true, |c| !c.is_alphanumeric());
    let mut out = String::with_capacity(text.len());
    let mut prev: Option<char> = None;
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let candidate: String = rest.chars().take(n).collect();
        let end = i + candidate.len();
        if candidate.to_lowercase() == phrase && boundary(prev) && boundary(text[end..].chars().next()) {
            out.push_str(with);
            prev = with.chars().last();
            i = end;
        } else {
            let c = rest.chars().next().unwrap_or_default();
            out.push(c);
            prev = Some(c);
            i += c.len_utf8().max(1);
        }
    }
    out
}

const UPDATE_SYSTEM: &str = r#"You keep memory.md, NexQ's long-term memory about its user ("You" in meeting transcripts). NexQ's meeting copilot reads it in every meeting to help the user.

Return the complete updated memory.md as Markdown, with no code fence and no other text.

- Keep everything that is still true. Add what this meeting taught about the user that will matter in future meetings: their projects, work, skills and experience, the people and organisations they deal with, their goals and preferences, recurring topics.
- Leave out details that only matter for this one meeting (those go in its notes) and anything said only by others about themselves.
- Merge and tighten instead of growing without end: stay under 1,500 words.
- Keep a "## Vocabulary" section: the user's proper names, project names and jargon, one per line, as "- TERM" or, when speech recognition got it wrong in the transcript, "- TERM — heard as: misheard, forms" (for example a project from the user's FILES transcribed as a common word). Keep the existing entries.
- If nothing new was learned, return the memory unchanged."#;

/// The system prompt and user message for updating the memory after a meeting.
pub fn update_messages(memory: &str, transcript: &str, notes: &[String], files: &[String]) -> (String, String) {
    let memory = if memory.trim().is_empty() { TEMPLATE } else { memory };
    let count = transcript.chars().count();
    let skip = transcript.char_indices().nth(count.saturating_sub(MAX_TRANSCRIPT_CHARS)).map_or(0, |(i, _)| i);
    let mut user = format!("## CURRENT MEMORY\n{}\n\n## TRANSCRIPT\n{}\n", memory.trim(), transcript[skip..].trim());
    if !notes.is_empty() {
        user.push_str(&format!("\n## MEETING NOTES\n{}\n", notes.iter().map(|n| format!("- {}", n)).collect::<Vec<_>>().join("\n")));
    }
    if !files.is_empty() {
        user.push_str(&format!("\n## FILES (the user's documents)\n{}\n", files.join("\n")));
    }
    (UPDATE_SYSTEM.to_string(), user)
}

/// The new memory from the model's reply, or why it can't be used. Refuses
/// a reply that lost most of a substantial memory, so a bad answer can't
/// wipe it.
pub fn accept_update(old: &str, reply: &str) -> Result<String, String> {
    let mut text = reply.trim();
    if let Some(inner) = text.strip_prefix("```") {
        text = inner.trim_start_matches(|c: char| c.is_alphanumeric()).trim();
        text = text.strip_suffix("```").unwrap_or(text).trim();
    }
    if text.is_empty() || text.chars().count() > MAX_MEMORY_CHARS {
        return Err("The model's memory update was empty or far too long".to_string());
    }
    let old_len = old.trim().chars().count();
    if old_len > 500 && text.chars().count() < old_len / 2 {
        return Err("The model's memory update dropped most of the memory; kept the old one".to_string());
    }
    Ok(format!("{}\n", text))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMORY: &str = "# About me\nI build RAG tools.\n\n## Vocabulary\n- KASH — heard as: cash, Cache\n- NexQ (heard as next queue)\n- Priya\n- \n\n## Projects\n- not a term\n";

    #[test]
    fn reads_the_vocabulary_section_only() {
        assert_eq!(vocabulary(MEMORY), [
            Term { term: "KASH".into(), heard_as: vec!["cash".into(), "Cache".into()] },
            Term { term: "NexQ".into(), heard_as: vec!["next queue".into()] },
            Term { term: "Priya".into(), heard_as: vec![] },
        ]);
        assert_eq!(terms(MEMORY), ["KASH", "NexQ", "Priya"]);
        assert!(vocabulary("# About me\n- KASH").is_empty());
    }

    #[test]
    fn misheard_words_become_the_term() {
        let v = vocabulary(MEMORY);
        assert_eq!(
            apply_vocabulary("Let's talk about my Cash RAG project, cashflow aside, on Next Queue.", &v),
            "Let's talk about my KASH RAG project, cashflow aside, on NexQ."
        );
        assert_eq!(apply_vocabulary("कैश cash।", &v), "कैश KASH।");
        assert_eq!(apply_vocabulary("", &v), "");
    }

    #[test]
    fn words_merge_without_duplicates_up_to_the_cap() {
        let merged = merge_words(vec!["NexQ".into()], vec!["nexq".into(), "KASH".into(), "Priya".into()], 2);
        assert_eq!(merged, ["NexQ", "KASH"]);
    }

    #[test]
    fn update_messages_carry_memory_transcript_notes_and_files() {
        let (system, user) = update_messages("", &format!("{}You: my cash rag project", "x".repeat(20_000)), &["Demo on Friday".into()], &["kash-design.md".into()]);
        assert!(system.contains("## Vocabulary"));
        assert!(user.starts_with("## CURRENT MEMORY\n# About me\n\n## Vocabulary\n\n## TRANSCRIPT\n"));
        assert!(user.contains("You: my cash rag project\n\n## MEETING NOTES\n- Demo on Friday\n\n## FILES"));
        assert!(user.len() < 16_000);
    }

    #[test]
    fn accepts_a_fenced_update_and_refuses_a_wipe() {
        assert_eq!(accept_update("", "```markdown\n# About me\n- builds KASH\n```").unwrap(), "# About me\n- builds KASH\n");
        let big = "fact. ".repeat(200);
        assert!(accept_update(&big, "# About me").is_err());
        assert!(accept_update("", "  ").is_err());
    }

    #[test]
    fn save_keeps_the_previous_version() {
        let dir = std::env::temp_dir().join(format!("nexq-memory-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = path(&dir);
        assert_eq!(load(&p), "");
        save(&p, "one").unwrap();
        save(&p, "two").unwrap();
        assert_eq!((load(&p), std::fs::read_to_string(p.with_extension("md.bak")).unwrap()), ("two".into(), "one".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

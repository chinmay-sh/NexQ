//! Automatic meeting copilot: every few seconds the overlay sends the
//! transcript lines that arrived since its last check, and the model returns
//! new meeting notes (a running list, never repeated) and cards: answers to
//! what is being discussed and insights from the user's files, each naming the
//! files it used. A detected or typed question is answered at once as a
//! question card.
//!
//! This module is the prompt and the parsing; `commands::copilot_commands`
//! adds the knowledge-base passages and calls the model.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const MAX_NEW_CHARS: usize = 6_000;
const MAX_CONTEXT_CHARS: usize = 3_000;
const MAX_PASSAGE_CHARS: usize = 1_500;
const MAX_NOTES_PER_TICK: usize = 8;
const MAX_CARDS_PER_TICK: usize = 3;
/// Notes and topics sent back so the model doesn't repeat them.
const MAX_REMEMBERED: usize = 60;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CopilotRequest {
    /// Transcript lines since the last check ("Them: …"), oldest first.
    pub new_lines: String,
    /// Lines just before those, for context.
    pub recent_context: String,
    /// Notes already in the list.
    pub notes: Vec<String>,
    /// Topics of cards already shown.
    pub covered: Vec<String>,
    /// A detected or typed question to answer now.
    pub question: Option<String>,
    /// Model to use; empty means the active model.
    pub model: Option<String>,
    /// Let providers with native web search (Gemini, OpenRouter) use it.
    pub web_search: bool,
}

impl CopilotRequest {
    pub fn question(&self) -> Option<&str> {
        self.question.as_deref().map(str::trim).filter(|q| !q.is_empty())
    }
}

/// A knowledge-base passage offered to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct Passage {
    /// File path (folder files) or file name (uploaded files).
    pub source: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CopilotCard {
    /// "question" (answers a question), "insight" (from the user's files) or "answer".
    pub kind: String,
    pub topic: String,
    pub points: Vec<String>,
    /// Files the card is based on.
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CopilotResponse {
    pub notes: Vec<String>,
    pub cards: Vec<CopilotCard>,
}

const SYSTEM: &str = r#"You are NexQ's meeting copilot, silently following a live meeting. The transcript comes from speech recognition, so expect misheard words and a mix of Hindi and English; "You" is the user, "Them" is everyone else.

Reply with ONLY a JSON object, no other text:
{"notes": ["..."], "cards": [{"topic": "...", "points": ["..."], "files": ["F1"]}]}

notes: everything in the NEW LINES that belongs in meeting notes: requests, proposals and their numbers, decisions, possible changes, action items (with owner and date when said), and open questions. One short sentence each, keeping names, numbers and dates. Notes are the record of the meeting: a point that also gets a card still gets a note. Never repeat or rephrase anything under NOTES SO FAR. [] when nothing new.

cards: help for what is being discussed right now: answers to questions raised, facts the participants are unsure about, and relevant insights from the user's files under PASSAGES. Each card has a 3 to 8 word topic, 2 to 4 short points, and "files": the ids of the passages it uses ([] if none). Prefer the user's files to general knowledge, never invent facts about the user's own work, and say when general knowledge may be out of date. No card for a topic under ALREADY COVERED unless something new was asked about it. Small talk and scheduling get no card. [] when nothing is worth a card.

Write notes and cards in English."#;

const QUESTION_RULE: &str = "\n\nA QUESTION is given: \"cards\" must hold exactly one card that answers it, using the transcript as context. Notes may be [].";

fn tail(text: &str, max_chars: usize) -> &str {
    let count = text.chars().count();
    if count <= max_chars {
        return text.trim();
    }
    let skip = text.char_indices().nth(count - max_chars).map_or(0, |(i, _)| i);
    text[skip..].trim()
}

fn last<'a>(items: &'a [String], n: usize) -> &'a [String] {
    &items[items.len().saturating_sub(n)..]
}

/// The system prompt and the user message for one check.
pub fn build_messages(req: &CopilotRequest, passages: &[Passage]) -> (String, String) {
    let mut system = SYSTEM.to_string();
    if req.question().is_some() {
        system.push_str(QUESTION_RULE);
    }
    let mut user = String::new();
    let section = |user: &mut String, title: &str, body: &str| {
        if !body.trim().is_empty() {
            user.push_str(&format!("## {}\n{}\n\n", title, body.trim()));
        }
    };
    let list = |items: &[String]| items.iter().map(|i| format!("- {}", i)).collect::<Vec<_>>().join("\n");
    section(&mut user, "NOTES SO FAR", &list(last(&req.notes, MAX_REMEMBERED)));
    section(&mut user, "ALREADY COVERED", &list(last(&req.covered, MAX_REMEMBERED)));
    section(&mut user, "EARLIER IN THE MEETING", tail(&req.recent_context, MAX_CONTEXT_CHARS));
    section(&mut user, "NEW LINES", tail(&req.new_lines, MAX_NEW_CHARS));
    let passages = passages
        .iter()
        .enumerate()
        .map(|(i, p)| format!("[F{}] {}\n{}", i + 1, p.source, tail_start(&p.text, MAX_PASSAGE_CHARS)))
        .collect::<Vec<_>>()
        .join("\n\n");
    section(&mut user, "PASSAGES", &passages);
    if let Some(q) = req.question() {
        section(&mut user, "QUESTION", q);
    }
    (system, user.trim_end().to_string())
}

/// The first `max_chars` characters.
fn tail_start(text: &str, max_chars: usize) -> &str {
    text.char_indices().nth(max_chars).map_or(text, |(i, _)| &text[..i]).trim()
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawResponse {
    notes: Vec<serde_json::Value>,
    cards: Vec<RawCard>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCard {
    topic: String,
    points: Vec<serde_json::Value>,
    files: Vec<serde_json::Value>,
}

fn text_of(v: &serde_json::Value) -> Option<String> {
    let s = match v {
        serde_json::Value::String(s) => s.trim().trim_start_matches(['-', '•', '*']).trim().to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => return None,
    };
    (!s.is_empty()).then_some(s)
}

/// Lowercase letters and digits only, for spotting repeats.
fn normalize(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The JSON object in a model reply (which may be wrapped in a code fence or prose).
fn json_object(reply: &str) -> Option<&str> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    (end > start).then(|| &reply[start..=end])
}

/// Parse the model's reply: drop notes already in `req.notes` (or repeated
/// within the reply), map passage ids to file paths, and label each card.
pub fn parse_response(reply: &str, req: &CopilotRequest, passages: &[Passage]) -> Result<CopilotResponse, String> {
    let raw: RawResponse = json_object(reply)
        .and_then(|j| serde_json::from_str(j).ok())
        .ok_or_else(|| "The model's reply wasn't the JSON the copilot asked for".to_string())?;

    let mut seen: HashSet<String> = req.notes.iter().map(|n| normalize(n)).collect();
    let notes = raw
        .notes
        .iter()
        .filter_map(text_of)
        .filter(|n| seen.insert(normalize(n)))
        .take(MAX_NOTES_PER_TICK)
        .collect();

    let question = req.question().is_some();
    let cards = raw
        .cards
        .into_iter()
        .filter_map(|c| {
            let points: Vec<String> = c.points.iter().filter_map(text_of).collect();
            if points.is_empty() {
                return None;
            }
            let mut files: Vec<String> = Vec::new();
            for id in c.files.iter().filter_map(text_of) {
                let index = id.trim_start_matches(['F', 'f', '[']).trim_end_matches(']').parse::<usize>().ok();
                if let Some(p) = index.and_then(|i| i.checked_sub(1)).and_then(|i| passages.get(i)) {
                    if !files.contains(&p.source) {
                        files.push(p.source.clone());
                    }
                }
            }
            let kind = if question { "question" } else if files.is_empty() { "answer" } else { "insight" };
            let topic = c.topic.trim().to_string();
            Some(CopilotCard { kind: kind.into(), topic: if topic.is_empty() { "Answer".into() } else { topic }, points, files })
        })
        .take(if question { 1 } else { MAX_CARDS_PER_TICK })
        .collect();
    Ok(CopilotResponse { notes, cards })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> CopilotRequest {
        CopilotRequest {
            new_lines: "Them: Acme wants 50 seats at 4200 a month.\nYou: What did we quote them last year?".into(),
            recent_context: "Them: Let's talk pricing.".into(),
            notes: vec!["Launch moved to April.".into()],
            covered: vec!["Falcon launch date".into()],
            ..Default::default()
        }
    }

    fn passages() -> Vec<Passage> {
        vec![
            Passage { source: "E:/deals/acme.xlsx".into(), text: "Acme 2025 quote: 3900 per month".into() },
            Passage { source: "notes.md".into(), text: "unrelated".into() },
        ]
    }

    #[test]
    fn messages_carry_every_section_in_order() {
        let (system, user) = build_messages(&req(), &passages());
        assert!(!system.contains("A QUESTION is given"));
        let order = ["## NOTES SO FAR\n- Launch moved", "## ALREADY COVERED\n- Falcon", "## EARLIER IN THE MEETING", "## NEW LINES\nThem: Acme", "## PASSAGES\n[F1] E:/deals/acme.xlsx\nAcme 2025", "[F2] notes.md"];
        let mut at = 0;
        for part in order {
            let found = user[at..].find(part).unwrap_or_else(|| panic!("missing or out of order: {part}\n{user}"));
            at += found;
        }
        assert!(!user.contains("QUESTION"));
    }

    #[test]
    fn a_question_adds_the_rule_and_the_section() {
        let r = CopilotRequest { question: Some("  What did we quote Acme? ".into()), ..req() };
        let (system, user) = build_messages(&r, &[]);
        assert!(system.ends_with("Notes may be []."));
        assert!(user.ends_with("## QUESTION\nWhat did we quote Acme?"));
        assert!(!user.contains("PASSAGES"));
    }

    #[test]
    fn long_text_keeps_the_newest_part() {
        let r = CopilotRequest { new_lines: format!("{}END", "x".repeat(10_000)), ..Default::default() };
        let (_, user) = build_messages(&r, &[]);
        assert!(user.ends_with("END"));
        assert!(user.len() < 6_100);
    }

    #[test]
    fn parses_notes_and_cards_from_a_fenced_reply() {
        let reply = r#"Sure:
```json
{"notes": ["- Acme wants 50 seats at 4200/month.", "launch moved to april", "Acme wants 50 seats at 4200/month"],
 "cards": [
   {"topic": "Last year's Acme quote", "points": ["Quoted 3900 per month in 2025"], "files": ["F1", "F1", "F9"]},
   {"topic": "Seat pricing benchmarks", "points": ["Typical SaaS seat prices range widely"], "files": []},
   {"topic": "Empty", "points": [], "files": []}
 ]}
```"#;
        let out = parse_response(reply, &req(), &passages()).unwrap();
        assert_eq!(out.notes, ["Acme wants 50 seats at 4200/month."]);
        assert_eq!(out.cards.len(), 2);
        assert_eq!(out.cards[0].kind, "insight");
        assert_eq!(out.cards[0].files, ["E:/deals/acme.xlsx"]);
        assert_eq!(out.cards[1].kind, "answer");
    }

    #[test]
    fn a_question_reply_is_one_question_card() {
        let r = CopilotRequest { question: Some("What did we quote Acme?".into()), ..req() };
        let reply = r#"{"notes": [], "cards": [{"topic": "", "points": ["3900 a month"], "files": ["[F1]"]}, {"topic": "x", "points": ["y"]}]}"#;
        let out = parse_response(reply, &r, &passages()).unwrap();
        assert_eq!(out.cards, [CopilotCard { kind: "question".into(), topic: "Answer".into(), points: vec!["3900 a month".into()], files: vec!["E:/deals/acme.xlsx".into()] }]);
    }

    #[test]
    fn nothing_new_and_bad_replies() {
        assert_eq!(parse_response(r#"{"notes": [], "cards": []}"#, &req(), &[]).unwrap(), CopilotResponse::default());
        assert_eq!(parse_response(r#"{}"#, &req(), &[]).unwrap(), CopilotResponse::default());
        assert!(parse_response("NONE", &req(), &[]).is_err());
    }
}

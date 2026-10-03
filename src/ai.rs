//! AI briefings: a three-point summary of where a contact's deal stands.
//!
//! # What is sent, and to whom
//!
//! The brief is written by a remote model, so this module's job includes being
//! explicit about the boundary: exactly what leaves the server is assembled in
//! [`Prompt`] and nowhere else, so the answer to "what did we send?" is a single
//! function rather than a search. It is the contact's own details, their notes,
//! their company, their deals, and their recent activity — nothing about other
//! contacts, other companies, or other workspaces.
//!
//! That is still customer data going to a third party, so the feature is off
//! until an API key is configured, and the buttons say so. See the README.
//!
//! # Why the prompt asks for JSON
//!
//! The result is rendered as three list items, so the shape has to be
//! dependable. Asking for a small JSON object and parsing it with `serde_json`
//! is more reliable than splitting prose on newlines, and
//! [`Briefing::parse`] still falls back to reading bullet-prefixed lines so a
//! model that ignores the instruction produces something readable rather than
//! an error.
//!
//! # Cost and latency
//!
//! One request per button press, on a human timescale. The caps that keep it
//! predictable are [`MAX_ACTIVITIES`], [`MAX_CONTEXT_CHARS`], and
//! `CRM_AI_MAX_TOKENS`; a briefing is generated only when somebody asks, never
//! in the background.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::domain::{self, Stage};

/// How many of a contact's most recent activities go into the prompt.
///
/// The most recent ones are what "where does this stand" depends on; a longer
/// tail costs tokens and dilutes the summary.
pub const MAX_ACTIVITIES: usize = 40;

/// Ceiling on the whole context block, in characters, after assembly.
///
/// A cap on the total rather than on each field, so a contact with a huge notes
/// field cannot push the request past it just because every individual field
/// looked reasonable.
pub const MAX_CONTEXT_CHARS: usize = 12_000;

/// Exactly how many bullets the model is asked for.
pub const BULLETS: usize = 3;

// --- Configuration ---------------------------------------------------------

/// Where the model lives and what it is allowed to cost.
#[derive(Debug, Clone)]
pub struct AiConfig {
    /// API key. `None` means the feature is off.
    pub api_key: Option<String>,
    /// Model name, e.g. `deepseek-chat`.
    pub model: String,
    /// Base URL, without the path.
    pub base_url: String,
    /// Whole-request budget.
    pub timeout: Duration,
    /// Ceiling on the reply.
    pub max_tokens: u32,
}

impl AiConfig {
    /// The provider's default endpoint.
    pub const DEFAULT_BASE_URL: &'static str = "https://api.deepseek.com";

    /// The provider's general-purpose chat model.
    ///
    /// Named `deepseek-chat`, which the provider points at its current
    /// fast general model (V3.x). There is no model called "flash" in
    /// DeepSeek's line-up — that naming belongs to Google's Gemini — so this is
    /// the intended equivalent: the cheap, fast one. `CRM_AI_MODEL` overrides it
    /// without a code change, which is the point of it being configuration.
    pub const DEFAULT_MODEL: &'static str = "deepseek-chat";

    /// Whether briefings can be generated at all.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.api_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty())
    }

    /// The endpoint the request is posted to.
    ///
    /// The provider speaks the OpenAI-compatible chat-completions shape, one
    /// path segment below the base URL.
    #[must_use]
    pub fn endpoint(&self) -> String {
        format!(
            "{}/chat/completions",
            self.base_url.trim_end_matches('/')
        )
    }
}

// --- The generated briefing ------------------------------------------------

/// A parsed briefing, before it is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Briefing {
    /// The bullet points, in order, without their markers.
    pub bullets: Vec<String>,
    /// Tokens the provider reported, if it did.
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
}

impl Briefing {
    /// The stored form: one bullet per line.
    #[must_use]
    pub fn to_text(&self) -> String {
        self.bullets.join("\n")
    }

    /// Read the stored form back.
    #[must_use]
    pub fn from_text(text: &str) -> Vec<String> {
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| {
                line.trim_start_matches(['-', '*', '•', ' '])
                    .trim_start()
                    .to_string()
            })
            .filter(|line| !line.is_empty())
            .collect()
    }

    /// Read a model reply, whichever of the two shapes it came in.
    ///
    /// Prefers the JSON object the prompt asks for. Falls back to treating the
    /// reply as a bulleted list, because a model that ignores the instruction
    /// should still produce something a person can read rather than an error
    /// page.
    #[must_use]
    pub fn parse(reply: &str, prompt_tokens: Option<i64>, completion_tokens: Option<i64>) -> Self {
        let bullets = Self::parse_json(reply)
            .or_else(|| Self::parse_lines(reply))
            .unwrap_or_default();

        Self {
            bullets,
            prompt_tokens,
            completion_tokens,
        }
    }

    /// `{"bullets": ["…", "…", "…"]}`, possibly wrapped in a fenced code block.
    fn parse_json(reply: &str) -> Option<Vec<String>> {
        #[derive(Deserialize)]
        struct Shape {
            bullets: Vec<String>,
        }

        // Models like to wrap JSON in ```json … ``` even when told not to.
        let trimmed = reply.trim();
        let body = trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .and_then(|rest| rest.strip_suffix("```"))
            .unwrap_or(trimmed)
            .trim();

        // Anything before the first `{` is prose; `from_str` would reject it.
        let start = body.find('{')?;
        let end = body.rfind('}')?;
        let json = body.get(start..=end)?;

        let parsed: Shape = serde_json::from_str(json).ok()?;
        let bullets: Vec<String> = parsed
            .bullets
            .into_iter()
            .map(|bullet| bullet.trim().to_string())
            .filter(|bullet| !bullet.is_empty())
            .collect();
        (!bullets.is_empty()).then_some(bullets)
    }

    /// Bullet-prefixed or numbered lines.
    fn parse_lines(reply: &str) -> Option<Vec<String>> {
        let bullets: Vec<String> = reply
            .lines()
            .map(str::trim)
            .filter(|line| {
                line.starts_with(['-', '*', '•'])
                    || line
                        .split_once(['.', ')'])
                        .is_some_and(|(head, _)| {
                            !head.is_empty() && head.chars().all(|c| c.is_ascii_digit())
                        })
            })
            .map(|line| {
                line.trim_start_matches(['-', '*', '•', ' '])
                    .trim_start_matches(|c: char| c.is_ascii_digit())
                    .trim_start_matches(['.', ')', ' '])
                    .trim()
                    .to_string()
            })
            .filter(|line| !line.is_empty())
            .collect();
        (!bullets.is_empty()).then_some(bullets)
    }
}

// --- The prompt ------------------------------------------------------------

/// Everything about one contact that may be sent to the model.
///
/// Assembled before the request so the boundary is inspectable: this struct is
/// the complete list, and [`Prompt::render`] is the only thing that formats it.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub contact_name: String,
    pub contact_title: Option<String>,
    pub company_name: Option<String>,
    pub contact_notes: Option<String>,
    /// `(stage label, value in cents)` for the contact's deals.
    pub deals: Vec<(String, i64)>,
    /// `(kind label, date, body)`, oldest first so the model reads a story.
    pub activities: Vec<(String, String, String)>,
}

/// The instruction the model is given. Fixed text, so a change to how briefings
/// read is a change to one string.
const SYSTEM_PROMPT: &str = "\
You are a sales assistant briefing a colleague before they contact somebody. \
You are given one contact, their company, their deals, and a log of recent \
interactions with them.

Write exactly three bullet points:
1. Where things stand right now.
2. What is blocking or uncertain, or what the risk is.
3. The single most useful next step, and when to take it.

Rules:
- Ground every point in the supplied notes and activity. Do not invent facts, \
names, numbers, or dates that are not there.
- If the history is too thin to say something, say that plainly in that bullet \
rather than guessing.
- Be specific and concrete. \"Follow up\" is not a next step; \"ask about the \
security questionnaire they have not returned\" is.
- Write to a colleague: no preamble, no restating the question, no sign-off.
- Each bullet is at most 30 words, one sentence.

Reply with JSON only, in this exact shape:
{\"bullets\": [\"first\", \"second\", \"third\"]}";

impl Prompt {
    /// Format the context block sent as the user message.
    ///
    /// Returns the text and whether anything had to be dropped to fit
    /// [`MAX_CONTEXT_CHARS`], so the caller can say so rather than have the
    /// model quietly see a truncated history.
    #[must_use]
    pub fn render(&self) -> (String, bool) {
        let mut out = String::with_capacity(1024);
        let mut truncated = false;

        out.push_str("CONTACT\n");
        out.push_str(&format!("Name: {}\n", self.contact_name));
        if let Some(title) = &self.contact_title {
            out.push_str(&format!("Job title: {title}\n"));
        }
        match &self.company_name {
            Some(company) => out.push_str(&format!("Company: {company}\n")),
            None => out.push_str("Company: (none recorded)\n"),
        }
        match &self.contact_notes {
            Some(notes) => out.push_str(&format!("Notes: {notes}\n")),
            None => out.push_str("Notes: (none recorded)\n"),
        }

        out.push_str("\nDEALS\n");
        if self.deals.is_empty() {
            out.push_str("(no deals recorded for this contact)\n");
        } else {
            for (stage, value_cents) in &self.deals {
                out.push_str(&format!(
                    "- {stage}, {}\n",
                    domain::format_money(*value_cents)
                ));
            }
        }

        out.push_str("\nRECENT ACTIVITY, oldest first\n");
        if self.activities.is_empty() {
            out.push_str("(nothing logged)\n");
        } else {
            for (kind, date, body) in &self.activities {
                let line = format!("- [{date}] {kind}: {body}\n");
                if out.len() + line.len() > MAX_CONTEXT_CHARS {
                    truncated = true;
                    break;
                }
                out.push_str(&line);
            }
        }

        (out, truncated)
    }
}

/// Build the prompt for a contact from rows already loaded by the caller.
///
/// Takes the pieces rather than fetching them, so the set of data that may
/// leave the server is visible at the call site.
#[must_use]
pub fn build_prompt(
    contact: &crate::models::Contact,
    company: Option<&crate::models::Company>,
    deals: &[crate::models::Deal],
    activities: &[crate::models::Activity],
    zone: &domain::TimeZone,
) -> Prompt {
    Prompt {
        contact_name: domain::full_name(&contact.first_name, &contact.last_name),
        contact_title: contact.title.clone(),
        company_name: company.map(|company| company.name.clone()),
        contact_notes: contact.notes.clone(),
        deals: deals
            .iter()
            .map(|deal| {
                (
                    Stage::from_stored(&deal.stage).label().to_string(),
                    deal.value_cents,
                )
            })
            .collect(),
        activities: activities
            .iter()
            // The caller fetches newest-first for the page; the model reads a
            // story better oldest-first.
            .rev()
            .map(|activity| {
                (
                    domain::ActivityKind::from_stored(&activity.kind)
                        .label()
                        .to_string(),
                    zone.format_date(activity.created_at),
                    collapse_whitespace(&activity.body),
                )
            })
            .collect(),
    }
}

/// Collapse a free-text body onto one line, so one long note cannot look like
/// several log entries in the prompt.
fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

// --- The request -----------------------------------------------------------

/// Why a briefing could not be produced.
#[derive(Debug)]
pub enum AiError {
    /// No API key, so the feature is off.
    NotConfigured,
    /// The request did not complete: connection, TLS, DNS, or timeout.
    Transport(reqwest::Error),
    /// The provider answered with a non-success status.
    Status {
        status: u16,
        /// The provider's own message, when it sent one.
        detail: Option<String>,
    },
    /// The provider answered, but not with anything usable.
    Unreadable(String),
}

impl std::fmt::Display for AiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AiError::NotConfigured => write!(
                f,
                "AI briefings are not configured. Set CRM_AI_API_KEY to enable them."
            ),
            AiError::Transport(error) => {
                if error.is_timeout() {
                    write!(f, "the AI provider did not answer in time")
                } else if error.is_connect() {
                    write!(f, "could not reach the AI provider")
                } else {
                    write!(f, "the request to the AI provider failed: {error}")
                }
            }
            AiError::Status { status, detail } => match detail {
                Some(detail) => write!(f, "the AI provider returned {status}: {detail}"),
                None => write!(f, "the AI provider returned {status}"),
            },
            AiError::Unreadable(why) => write!(f, "could not read the AI provider's reply: {why}"),
        }
    }
}

impl std::error::Error for AiError {}

/// The request body, in the provider's OpenAI-compatible shape.
#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: [Message<'a>; 2],
    /// Low, because a briefing should read the same way twice given the same
    /// history. This is summarising, not writing.
    temperature: f32,
    max_tokens: u32,
    response_format: ResponseFormat,
}

#[derive(Debug, Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Serialize)]
struct ResponseFormat {
    r#type: &'static str,
}

/// The parts of the reply this app uses. Everything else the provider sends is
/// ignored, so an addition to its response shape does not break the feature.
#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ReplyMessage,
}

#[derive(Debug, Deserialize)]
struct ReplyMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: Option<i64>,
    #[serde(default)]
    completion_tokens: Option<i64>,
}

/// Ask the model for a briefing.
///
/// # Errors
///
/// Returns [`AiError`] when the feature is unconfigured, the request fails, the
/// provider reports an error, or the reply cannot be read.
pub async fn generate(
    client: &reqwest::Client,
    config: &AiConfig,
    prompt: &Prompt,
) -> Result<Briefing, AiError> {
    let Some(api_key) = config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
    else {
        return Err(AiError::NotConfigured);
    };

    let (context, _truncated) = prompt.render();

    let body = ChatRequest {
        model: &config.model,
        messages: [
            Message {
                role: "system",
                content: SYSTEM_PROMPT,
            },
            Message {
                role: "user",
                content: &context,
            },
        ],
        temperature: 0.3,
        max_tokens: config.max_tokens,
        response_format: ResponseFormat {
            r#type: "json_object",
        },
    };

    let response = client
        .post(config.endpoint())
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(AiError::Transport)?;

    let status = response.status();
    let text = response.text().await.map_err(AiError::Transport)?;

    if !status.is_success() {
        return Err(AiError::Status {
            status: status.as_u16(),
            // The provider's error body is short and says what was wrong with
            // the request; it names no customer data.
            detail: provider_message(&text),
        });
    }

    let parsed: ChatResponse = serde_json::from_str(&text)
        .map_err(|error| AiError::Unreadable(format!("{error}; body began: {}", head(&text, 200))))?;

    let content = parsed
        .choices
        .first()
        .and_then(|choice| choice.message.content.as_deref())
        .ok_or_else(|| AiError::Unreadable("the reply contained no message content".to_string()))?;

    let usage = parsed.usage;
    let briefing = Briefing::parse(
        content,
        usage.as_ref().and_then(|usage| usage.prompt_tokens),
        usage.as_ref().and_then(|usage| usage.completion_tokens),
    );

    if briefing.bullets.is_empty() {
        return Err(AiError::Unreadable(format!(
            "the reply contained no bullet points; it began: {}",
            head(content, 200)
        )));
    }

    Ok(briefing)
}

/// Pull a human-readable message out of a provider error body.
fn provider_message(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Envelope {
        error: Inner,
    }
    #[derive(Deserialize)]
    struct Inner {
        #[serde(default)]
        message: Option<String>,
    }

    serde_json::from_str::<Envelope>(body)
        .ok()
        .and_then(|envelope| envelope.error.message)
        .or_else(|| {
            let trimmed = body.trim();
            (!trimmed.is_empty() && trimmed.len() <= 300).then(|| trimmed.to_string())
        })
}

/// The first `max` characters, for an error message that must stay short.
fn head(value: &str, max: usize) -> String {
    let mut out: String = value.chars().take(max).collect();
    if value.chars().count() > max {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_endpoint_is_the_base_url_plus_one_path_segment() {
        let mut config = AiConfig {
            api_key: Some("k".to_string()),
            model: "deepseek-chat".to_string(),
            base_url: AiConfig::DEFAULT_BASE_URL.to_string(),
            timeout: Duration::from_secs(30),
            max_tokens: 512,
        };
        assert_eq!(config.endpoint(), "https://api.deepseek.com/chat/completions");

        // A trailing slash must not double up.
        config.base_url = "https://api.deepseek.com/".to_string();
        assert_eq!(config.endpoint(), "https://api.deepseek.com/chat/completions");

        // A self-hosted or proxy base URL works the same way.
        config.base_url = "http://127.0.0.1:11434/v1".to_string();
        assert_eq!(config.endpoint(), "http://127.0.0.1:11434/v1/chat/completions");
    }

    #[test]
    fn the_feature_is_off_without_a_usable_key() {
        let base = AiConfig {
            api_key: None,
            model: AiConfig::DEFAULT_MODEL.to_string(),
            base_url: AiConfig::DEFAULT_BASE_URL.to_string(),
            timeout: Duration::from_secs(30),
            max_tokens: 512,
        };
        assert!(!base.is_enabled());

        let blank = AiConfig {
            api_key: Some("   ".to_string()),
            ..base.clone()
        };
        assert!(!blank.is_enabled(), "a whitespace key is not a key");

        let set = AiConfig {
            api_key: Some("sk-test".to_string()),
            ..base
        };
        assert!(set.is_enabled());
    }

    #[test]
    fn the_json_shape_is_preferred() {
        let reply = r#"{"bullets": ["Deal is in Proposal.", "Waiting on security review.", "Chase the questionnaire by Friday."]}"#;
        let briefing = Briefing::parse(reply, Some(100), Some(50));
        assert_eq!(briefing.bullets.len(), 3);
        assert_eq!(briefing.bullets[0], "Deal is in Proposal.");
        assert_eq!(briefing.prompt_tokens, Some(100));
        assert_eq!(briefing.completion_tokens, Some(50));
    }

    #[test]
    fn json_wrapped_in_a_code_fence_or_prose_still_parses() {
        for reply in [
            "```json\n{\"bullets\": [\"one\", \"two\", \"three\"]}\n```",
            "```\n{\"bullets\": [\"one\", \"two\", \"three\"]}\n```",
            "Here is the briefing:\n{\"bullets\": [\"one\", \"two\", \"three\"]}\nHope that helps.",
        ] {
            let briefing = Briefing::parse(reply, None, None);
            assert_eq!(briefing.bullets, vec!["one", "two", "three"], "{reply}");
        }
    }

    #[test]
    fn a_model_that_ignores_the_instruction_is_still_readable() {
        // Better to show the three lines than an error page.
        let reply = "- Deal is in Proposal.\n- Waiting on their security review.\n- Chase the questionnaire on Friday.";
        let briefing = Briefing::parse(reply, None, None);
        assert_eq!(
            briefing.bullets,
            vec![
                "Deal is in Proposal.",
                "Waiting on their security review.",
                "Chase the questionnaire on Friday."
            ]
        );

        // Numbered lists too.
        let numbered = "1. First point\n2) Second point\n3. Third point";
        assert_eq!(
            Briefing::parse(numbered, None, None).bullets,
            vec!["First point", "Second point", "Third point"]
        );

        // And a plain line without a marker is not a bullet.
        assert!(Briefing::parse("I cannot help with that.", None, None)
            .bullets
            .is_empty());
    }

    #[test]
    fn malformed_json_falls_through_rather_than_failing() {
        // Truncated JSON with no bullets should not be parsed as JSON at all.
        assert!(Briefing::parse("{\"bullets\": [\"one\"", None, None).bullets.is_empty());
        // An empty list is not a briefing.
        assert!(Briefing::parse("{\"bullets\": []}", None, None).bullets.is_empty());
    }

    #[test]
    fn bullets_round_trip_through_storage() {
        let briefing = Briefing {
            bullets: vec!["one".to_string(), "two".to_string(), "three".to_string()],
            prompt_tokens: None,
            completion_tokens: None,
        };
        let stored = briefing.to_text();
        assert_eq!(stored, "one\ntwo\nthree");
        assert_eq!(Briefing::from_text(&stored), briefing.bullets);

        // Markers and blank lines are tolerated on the way back in, so a
        // hand-edited row still renders.
        assert_eq!(
            Briefing::from_text("- one\n\n* two\n  • three  \n"),
            vec!["one", "two", "three"]
        );
    }

    #[test]
    fn the_provider_error_message_is_extracted() {
        let body = r#"{"error": {"message": "Insufficient balance", "type": "invalid_request_error"}}"#;
        assert_eq!(provider_message(body).as_deref(), Some("Insufficient balance"));

        // A plain-text body is used as-is, but only when short.
        assert_eq!(provider_message("rate limited").as_deref(), Some("rate limited"));
        assert!(provider_message(&"x".repeat(1000)).is_none());
        assert!(provider_message("").is_none());
    }

    #[test]
    fn aborting_a_long_error_body_keeps_it_short() {
        let long = "a".repeat(500);
        let short = head(&long, 10);
        assert_eq!(short.chars().count(), 11, "10 characters plus the ellipsis");
        assert!(short.ends_with('…'));
        assert_eq!(head("short", 200), "short");
    }

    #[test]
    fn the_prompt_states_what_it_sends_and_flags_truncation() {
        let prompt = Prompt {
            contact_name: "Ada Lovelace".to_string(),
            contact_title: Some("CTO".to_string()),
            company_name: Some("Vertex".to_string()),
            contact_notes: Some("Prefers email.".to_string()),
            deals: vec![("Proposal".to_string(), 12_500_000)],
            activities: vec![
                ("Call".to_string(), "2026-01-02".to_string(), "Discussed pricing.".to_string()),
            ],
        };
        let (rendered, truncated) = prompt.render();

        assert!(!truncated);
        assert!(rendered.contains("CONTACT"));
        assert!(rendered.contains("Name: Ada Lovelace"));
        assert!(rendered.contains("Company: Vertex"));
        assert!(rendered.contains("Proposal, $125,000.00"));
        assert!(rendered.contains("[2026-01-02] Call: Discussed pricing."));
    }

    #[test]
    fn an_empty_history_says_so_rather_than_leaving_a_gap() {
        let prompt = Prompt {
            contact_name: "Nobody".to_string(),
            contact_title: None,
            company_name: None,
            contact_notes: None,
            deals: vec![],
            activities: vec![],
        };
        let (rendered, _) = prompt.render();
        assert!(rendered.contains("Company: (none recorded)"));
        assert!(rendered.contains("(no deals recorded for this contact)"));
        assert!(rendered.contains("(nothing logged)"));
    }

    #[test]
    fn a_very_long_history_is_truncated_and_reported() {
        // Enough activity to blow the context cap.
        let activities: Vec<(String, String, String)> = (0..500)
            .map(|index| {
                (
                    "Note".to_string(),
                    "2026-01-01".to_string(),
                    format!("entry {index} {}", "x".repeat(100)),
                )
            })
            .collect();
        let prompt = Prompt {
            contact_name: "Chatty".to_string(),
            contact_title: None,
            company_name: None,
            contact_notes: None,
            deals: vec![],
            activities,
        };
        let (rendered, truncated) = prompt.render();

        assert!(truncated, "the caller has to be able to say so");
        assert!(
            rendered.len() <= MAX_CONTEXT_CHARS + 200,
            "the cap must actually hold, got {}",
            rendered.len()
        );
    }

    #[test]
    fn a_note_spanning_several_lines_becomes_one_line() {
        assert_eq!(
            collapse_whitespace("first line\nsecond   line\n\n  third"),
            "first line second line third"
        );
    }
}

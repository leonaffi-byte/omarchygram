//! System prompts for the Assistant's built-in features. Kept short and
//! plain; the model does the work. All take the user's own data as input.

pub const ASSISTANT: &str = "You are the assistant inside Omarchygram, a Telegram client on an \
Arch Linux desktop running Omarchy. Be concise and plain. When given messages, they are the \
user's own conversations; summarize faithfully, never invent content. Prefer short answers; use \
bullet points only for lists.";

/// "What did I miss" — feed the recent messages of one chat.
pub fn catch_up(chat_title: &str, transcript: &str) -> (String, String) {
    (
        format!("{ASSISTANT} Task: summarize what happened recently in the chat \"{chat_title}\". Lead with anything that needs a reply or a decision. Keep it under 120 words unless there is a lot going on."),
        format!("Recent messages (oldest first):\n\n{transcript}\n\nWhat did I miss?"),
    )
}

/// Draft a reply to one message in context.
pub fn draft_reply(chat_title: &str, transcript: &str, target: &str, hint: &str) -> (String, String) {
    let hint_line = if hint.trim().is_empty() { String::new() } else { format!(" The user wants the reply to be: {hint}.") };
    (
        format!("{ASSISTANT} Task: draft a reply the user could send in the chat \"{chat_title}\", in the user's own voice as seen in their earlier messages (marked 'You'). Output ONLY the reply text, no preamble, no quotes.{hint_line}"),
        format!("Recent messages (oldest first):\n\n{transcript}\n\nReply to this message:\n{target}"),
    )
}

pub fn translate(text: &str, target_lang: &str) -> (String, String) {
    (
        format!("{ASSISTANT} Task: translate the user's text to {target_lang}. Output ONLY the translation."),
        text.to_string(),
    )
}

pub fn summarize(text: &str) -> (String, String) {
    (
        format!("{ASSISTANT} Task: summarize the text in a few sentences."),
        text.to_string(),
    )
}

/// Search: pick the messages relevant to a question from a candidate list.
pub fn search(question: &str, candidates: &str) -> (String, String) {
    (
        format!("{ASSISTANT} Task: from the candidate messages, list the ones that answer or relate to the question, quoting each briefly with its chat and time. If none match, say so."),
        format!("Question: {question}\n\nCandidates:\n{candidates}"),
    )
}

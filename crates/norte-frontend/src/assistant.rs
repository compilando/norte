//! The assistant panel's conversation (ADR 0186), the same model for the
//! terminal and the window: what the reader typed, what came back, which
//! turn the cursor is on, and the request the next Enter sends.
//!
//! An `Order` turn is a proposal. Applying it is the reader's gesture, and
//! it runs through the box's own executor (ADR 0185) — nothing here runs
//! anything.

use norte_proto::VPath;
use norte_proto::methods::{
    AI_CHAT_MAX_MESSAGES, AI_CHAT_MAX_TEXT_BYTES, AiChatMessage, AiChatParams, AiChatRole,
    AiChatTurn,
};

/// One turn of the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Turn {
    /// What the reader wrote.
    Reader(String),
    /// A vetted order, with the folder it was written for.
    Order {
        /// The line.
        text: String,
        /// What it does, in the model's words.
        note: String,
        /// The focused folder when it was asked: Apply runs against the
        /// CURRENT pane, and the panel says when the two differ.
        written_for: VPath,
    },
    /// Prose.
    Answer(String),
    /// Why there was no turn, already translated; never sent back.
    Failed(String),
}

/// The most turns the conversation keeps; older ones fall off. Only the
/// newest [`AI_CHAT_MAX_MESSAGES`] are ever sent, and every repaint wraps
/// all that are kept.
pub const MAX_TURNS: usize = 200;

/// The conversation.
#[derive(Debug, Clone, Default)]
pub struct Assistant {
    turns: Vec<Turn>,
    input: String,
    pending: Option<VPath>,
    cursor: usize,
    /// Bumped by every request and every cancel: an answer carrying an
    /// older one is for a request nobody is waiting for any more.
    generation: u64,
    /// How many turns the cap has dropped: what turns an index into a
    /// stable name, since a turn dropping shifts every index after it.
    dropped: usize,
}

/// `text` cut to at most `max` bytes, on a char boundary.
fn capped(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

impl Assistant {
    /// The turns, oldest first.
    #[must_use]
    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }

    /// What is being typed.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }

    /// Whether a request is in flight.
    #[must_use]
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    /// The turn under the cursor.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Types a character.
    pub fn push_char(&mut self, c: char) {
        if self.input.len() + c.len_utf8() <= AI_CHAT_MAX_TEXT_BYTES {
            self.input.push(c);
        }
    }

    /// Appends a paste (its first line), within the cap.
    pub fn push_str(&mut self, s: &str) {
        for c in s.lines().next().unwrap_or("").chars() {
            self.push_char(c);
        }
    }

    /// Deletes the last character.
    pub fn backspace(&mut self) {
        self.input.pop();
    }

    /// The cursor one turn up.
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// The cursor one turn down.
    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.turns.len().saturating_sub(1));
    }

    /// The reader's Enter: the request to send, or `None` while one is in
    /// flight or there is nothing typed. The history is the newest turns
    /// that fit [`AI_CHAT_MAX_MESSAGES`]; an order goes back as its line,
    /// a failure not at all.
    pub fn submit(&mut self, here: &VPath, other: Option<&VPath>) -> Option<AiChatParams> {
        if self.pending.is_some() || self.input.trim().is_empty() {
            return None;
        }
        let text = std::mem::take(&mut self.input);
        self.ask(&text, here, other)
    }

    /// A message that does not come from the input (`@` in the box, the
    /// window's own field): the request, or `None` while a turn is in flight
    /// or there is nothing to say. The input is left as it is.
    pub fn ask(&mut self, text: &str, here: &VPath, other: Option<&VPath>) -> Option<AiChatParams> {
        let text = capped(text.trim(), AI_CHAT_MAX_TEXT_BYTES);
        if self.pending.is_some() || text.is_empty() {
            return None;
        }
        self.push(Turn::Reader(text));
        self.pending = Some(here.clone());
        self.generation += 1;
        let mut messages: Vec<AiChatMessage> = self
            .turns
            .iter()
            .filter_map(|t| {
                let (role, text) = match t {
                    Turn::Reader(s) => (AiChatRole::User, s.as_str()),
                    Turn::Order { text, .. } => (AiChatRole::Assistant, text.as_str()),
                    Turn::Answer(s) => (AiChatRole::Assistant, s.as_str()),
                    Turn::Failed(_) => return None,
                };
                Some(AiChatMessage {
                    role,
                    text: capped(text, AI_CHAT_MAX_TEXT_BYTES),
                })
            })
            .collect();
        let skip = messages.len().saturating_sub(AI_CHAT_MAX_MESSAGES);
        messages.drain(..skip);
        Some(AiChatParams {
            messages,
            here: here.clone(),
            other: other.cloned(),
        })
    }

    /// The answer to the request in flight: a turn, or why there is none.
    /// With nothing in flight — it was cancelled — it is dropped.
    pub fn land(&mut self, res: Result<AiChatTurn, String>) {
        let Some(written_for) = self.pending.take() else {
            return;
        };
        let turn = match res {
            Ok(AiChatTurn::Order { text, note }) => Turn::Order {
                text,
                note,
                written_for,
            },
            Ok(AiChatTurn::Answer { text }) => Turn::Answer(text),
            Ok(AiChatTurn::Unknown) => Turn::Failed(norte_i18n::t_in(
                norte_i18n::active(),
                "assistant-newer-kind",
            )),
            Err(why) => Turn::Failed(why),
        };
        self.push(turn);
    }

    /// [`Self::land`] for an answer to request `generation`: one for an
    /// earlier request, or a cancelled one, is dropped.
    pub fn land_for(&mut self, generation: u64, res: Result<AiChatTurn, String>) {
        if generation == self.generation {
            self.land(res);
        }
    }

    /// The current request's number, for [`Self::land_for`].
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Stops waiting for the turn in flight, and says so in `lang`. `false`
    /// if there was none.
    pub fn cancel(&mut self, lang: norte_i18n::Lang) -> bool {
        if self.pending.take().is_none() {
            return false;
        }
        self.generation += 1;
        self.push(Turn::Failed(norte_i18n::t_in(lang, "assistant-cancelled")));
        true
    }

    /// The absolute number of the first kept turn: turn `i` of
    /// [`Self::turns`] is `first() + i` for as long as it is kept.
    #[must_use]
    pub fn first(&self) -> usize {
        self.dropped
    }

    /// The order of the turn numbered `abs` (see [`Self::first`]), if it is
    /// still kept and is one: a click that names a dropped turn finds
    /// nothing rather than its neighbour.
    #[must_use]
    pub fn order_numbered(&self, abs: usize) -> Option<&str> {
        self.order_at(abs.checked_sub(self.dropped)?)
    }

    /// Appends a turn under the cursor, dropping the oldest beyond
    /// [`MAX_TURNS`].
    fn push(&mut self, turn: Turn) {
        self.turns.push(turn);
        let over = self.turns.len().saturating_sub(MAX_TURNS);
        self.turns.drain(..over);
        self.dropped += over;
        self.cursor = self.turns.len() - 1;
    }

    /// The order under the cursor, if the cursor is on one: what Apply runs.
    #[must_use]
    pub fn selected_order(&self) -> Option<&str> {
        self.order_at(self.cursor)
    }

    /// The order of turn `i`, if that turn is one (a click on its button).
    #[must_use]
    pub fn order_at(&self, i: usize) -> Option<&str> {
        match self.turns.get(i) {
            Some(Turn::Order { text, .. }) => Some(text),
            _ => None,
        }
    }
}

/// Why a turn did not come, as the panel says it: with AI off or no
/// provider, what to add to `norte.toml`; otherwise the error's sentence.
#[must_use]
pub fn failure_text(e: &norte_proto::Error, lang: norte_i18n::Lang) -> String {
    match e {
        norte_proto::Error::Unsupported => setup_help(lang).join("\n"),
        norte_proto::Error::PolicyDenied { rule } if rule == "ai-disabled" => {
            setup_help(lang).join("\n")
        }
        _ => norte_i18n::t_in(lang, crate::error::error_key(e)),
    }
}

/// What the panel shows with AI off: the `[ai]` lines to add to
/// `norte.toml`, translated.
#[must_use]
pub fn setup_help(lang: norte_i18n::Lang) -> Vec<String> {
    vec![
        norte_i18n::t_in(lang, "assistant-setup-1"),
        "[ai]".to_owned(),
        "enabled = true".to_owned(),
        "rename_provider = \"local\"".to_owned(),
        "[ai.providers.local]".to_owned(),
        "kind = \"ollama\"".to_owned(),
        "model = \"llama3.2\"".to_owned(),
        norte_i18n::t_in(lang, "assistant-setup-2"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn here() -> VPath {
        VPath::parse("file:///home").expect("vpath")
    }

    fn typed(a: &mut Assistant, s: &str) {
        a.push_str(s);
    }

    #[test]
    fn submit_sends_the_history_and_where_the_reader_is() {
        let mut a = Assistant::default();
        typed(&mut a, "hola");
        let p = a.submit(&here(), None).expect("request");
        assert_eq!(p.messages.len(), 1);
        assert_eq!(p.here, here());
        a.land(Ok(AiChatTurn::Answer { text: "hi".into() }));
        typed(&mut a, "copy the pdfs");
        let p = a.submit(&here(), None).expect("request");
        let roles: Vec<AiChatRole> = p.messages.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            [AiChatRole::User, AiChatRole::Assistant, AiChatRole::User]
        );
    }

    #[test]
    fn a_second_submit_while_pending_sends_nothing() {
        let mut a = Assistant::default();
        typed(&mut a, "one");
        assert!(a.submit(&here(), None).is_some());
        typed(&mut a, "two");
        assert!(a.submit(&here(), None).is_none());
        assert_eq!(a.input(), "two", "what was typed is kept");
    }

    #[test]
    fn nothing_typed_sends_nothing() {
        let mut a = Assistant::default();
        typed(&mut a, "   ");
        assert!(a.submit(&here(), None).is_none());
    }

    #[test]
    fn apply_takes_the_order_under_the_cursor() {
        let mut a = Assistant::default();
        typed(&mut a, "copy");
        let _ = a.submit(&here(), None);
        a.land(Ok(AiChatTurn::Order {
            text: "sel *.pdf | cp >other".into(),
            note: "n".into(),
        }));
        assert_eq!(a.selected_order(), Some("sel *.pdf | cp >other"));
        a.up();
        assert_eq!(
            a.selected_order(),
            None,
            "the reader's turn is not an order"
        );
        let Turn::Order { written_for, .. } = &a.turns()[1] else {
            panic!()
        };
        assert_eq!(written_for, &here());
    }

    /// A message from outside the input (`@` in the box, the window's own
    /// field) never touches what is typed, and while a turn is in flight it
    /// is refused — not parked to be glued onto the next message.
    #[test]
    fn ask_never_parks_text_while_a_turn_is_in_flight() {
        let mut a = Assistant::default();
        typed(&mut a, "half typed");
        assert!(a.ask("a", &here(), None).is_some());
        assert_eq!(a.input(), "half typed", "the input is not touched");
        assert!(a.ask("b", &here(), None).is_none(), "refused while pending");
        a.land(Ok(AiChatTurn::Answer { text: "ok".into() }));
        let p = a.ask("c", &here(), None).expect("request");
        assert_eq!(p.messages.last().map(|m| m.text.as_str()), Some("c"));
        assert!(p.messages.iter().all(|m| m.text != "b" && m.text != "bc"));
    }

    #[test]
    fn a_failure_is_shown_but_never_sent_back() {
        let mut a = Assistant::default();
        typed(&mut a, "one");
        let _ = a.submit(&here(), None);
        a.land(Err("AI is off".into()));
        assert!(!a.pending());
        typed(&mut a, "two");
        let p = a.submit(&here(), None).expect("request");
        assert!(p.messages.iter().all(|m| m.text != "AI is off"));
        assert_eq!(p.messages.len(), 2);
    }

    #[test]
    fn a_turn_of_a_later_kind_says_it_needs_a_newer_norte() {
        let mut a = Assistant::default();
        typed(&mut a, "one");
        let _ = a.submit(&here(), None);
        a.land(Ok(AiChatTurn::Unknown));
        assert!(!a.pending());
        assert!(
            matches!(a.turns().last(), Some(Turn::Failed(why)) if !why.starts_with("assistant-"))
        );
    }

    #[test]
    fn the_history_keeps_the_newest_that_fit() {
        let mut a = Assistant::default();
        for i in 0..30 {
            typed(&mut a, &format!("q{i}"));
            let _ = a.submit(&here(), None);
            a.land(Ok(AiChatTurn::Answer {
                text: format!("a{i}"),
            }));
        }
        typed(&mut a, "last");
        let p = a.submit(&here(), None).expect("request");
        assert_eq!(p.messages.len(), AI_CHAT_MAX_MESSAGES);
        assert_eq!(p.messages.last().map(|m| m.text.as_str()), Some("last"));
    }

    #[test]
    fn typing_stops_at_the_cap_on_a_char_boundary() {
        let mut a = Assistant::default();
        for _ in 0..AI_CHAT_MAX_TEXT_BYTES {
            a.push_char('é');
        }
        assert!(a.input().len() <= AI_CHAT_MAX_TEXT_BYTES);
        assert!(a.input().is_char_boundary(a.input().len()));
        assert_eq!(capped("añb", 2), "a");
    }

    #[test]
    fn setup_help_reads_in_both_locales() {
        for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
            let lines = setup_help(lang);
            assert!(
                !lines[0].starts_with("assistant-"),
                "{lang:?}: {}",
                lines[0]
            );
            assert!(lines.iter().any(|l| l == "enabled = true"));
        }
    }

    /// A cancelled turn says so, and its late answer is dropped — also one
    /// carrying the generation of a request made before a newer one.
    #[test]
    fn a_cancelled_turn_drops_its_late_answer() {
        let mut a = Assistant::default();
        a.ask("one", &here(), None).expect("request");
        let first = a.generation();
        assert!(a.cancel(norte_i18n::Lang::En));
        assert!(!a.pending());
        assert!(matches!(a.turns().last(), Some(Turn::Failed(_))));
        assert!(!a.cancel(norte_i18n::Lang::En), "nothing left to cancel");
        a.land_for(
            first,
            Ok(AiChatTurn::Answer {
                text: "late".into(),
            }),
        );
        assert_eq!(a.turns().len(), 2, "the late answer is dropped");

        a.ask("two", &here(), None).expect("request");
        a.land_for(
            first,
            Ok(AiChatTurn::Answer {
                text: "stale".into(),
            }),
        );
        assert!(a.pending(), "an older request's answer is not this one's");
        a.land_for(a.generation(), Ok(AiChatTurn::Answer { text: "ok".into() }));
        assert_eq!(a.turns().last(), Some(&Turn::Answer("ok".into())));
        a.land(Ok(AiChatTurn::Answer {
            text: "unasked".into(),
        }));
        assert_eq!(a.turns().last(), Some(&Turn::Answer("ok".into())));
    }

    /// The conversation keeps at most `MAX_TURNS`, the cursor on the last.
    #[test]
    fn the_conversation_is_capped() {
        let mut a = Assistant::default();
        for i in 0..MAX_TURNS {
            a.ask(&format!("q{i}"), &here(), None).expect("request");
            a.land(Ok(AiChatTurn::Answer { text: "a".into() }));
        }
        assert_eq!(a.turns().len(), MAX_TURNS);
        assert_eq!(a.cursor(), MAX_TURNS - 1);
        assert_eq!(a.turns().last(), Some(&Turn::Answer("a".into())));
    }

    /// A turn keeps its number when older ones are dropped: a click that
    /// names a dropped turn finds nothing, never its neighbour.
    #[test]
    fn an_order_keeps_its_number_across_the_cap() {
        let mut a = Assistant::default();
        a.ask("q", &here(), None).expect("request");
        a.land(Ok(AiChatTurn::Order {
            text: "sel *".into(),
            note: String::new(),
        }));
        let abs = a.first() + 1;
        assert_eq!(a.order_numbered(abs), Some("sel *"));
        for _ in 0..MAX_TURNS / 2 - 1 {
            a.ask("q", &here(), None).expect("request");
            a.land(Ok(AiChatTurn::Answer { text: "a".into() }));
        }
        assert_eq!(a.first(), 0, "nothing dropped yet");
        assert_eq!(a.order_numbered(abs), Some("sel *"));
        a.ask("q", &here(), None).expect("request");
        a.land(Ok(AiChatTurn::Answer { text: "a".into() }));
        assert_eq!(a.first(), 2);
        assert_eq!(a.order_numbered(abs), None, "dropped: found nothing");
        assert_eq!(a.order_numbered(0), None);
    }
}

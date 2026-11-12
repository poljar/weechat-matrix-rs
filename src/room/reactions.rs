//! Reactions (`m.reaction` annotations) aggregated per message.
//!
//! Reactions are shown after the message they annotate, in the style of
//! wee-slack: ` [👍2 😄1]`, with the reactions the own user added highlighted.

use std::collections::HashMap;

use matrix_sdk::ruma::{EventId, OwnedEventId, OwnedUserId, UserId};

/// All senders of one reaction key on one message, in arrival order.
#[derive(Debug, Clone)]
struct KeyReactions {
    key: String,
    senders: Vec<(OwnedUserId, OwnedEventId)>,
}

#[derive(Debug, Default)]
pub struct Reactions {
    /// Target message -> reaction keys in the order they first appeared.
    targets: HashMap<OwnedEventId, Vec<KeyReactions>>,
    /// Reaction event -> (target message, key), for redactions.
    events: HashMap<OwnedEventId, (OwnedEventId, String)>,
    /// Target message -> suffix currently printed after it.
    rendered: HashMap<OwnedEventId, String>,
}

impl Reactions {
    /// Record a reaction. Returns false if it was already known.
    pub fn add(
        &mut self,
        reaction_id: OwnedEventId,
        target: OwnedEventId,
        key: String,
        sender: OwnedUserId,
    ) -> bool {
        if self.events.contains_key(&reaction_id) {
            return false;
        }

        let keys = self.targets.entry(target.clone()).or_default();
        let entry = match keys.iter_mut().position(|k| k.key == key) {
            Some(i) => &mut keys[i],
            None => {
                keys.push(KeyReactions {
                    key: key.clone(),
                    senders: Vec::new(),
                });
                keys.last_mut().expect("just pushed")
            }
        };

        // One reaction per sender and key counts once.
        if entry.senders.iter().any(|(s, _)| *s == sender) {
            self.events.insert(reaction_id, (target, key));
            return false;
        }

        entry.senders.push((sender, reaction_id.clone()));
        self.events.insert(reaction_id, (target, key));
        true
    }

    /// Forget a redacted reaction. Returns the message it annotated.
    pub fn remove(&mut self, reaction_id: &EventId) -> Option<OwnedEventId> {
        let (target, key) = self.events.remove(reaction_id)?;

        if let Some(keys) = self.targets.get_mut(&target) {
            if let Some(entry) = keys.iter_mut().find(|k| k.key == key) {
                entry.senders.retain(|(_, id)| id != reaction_id);
            }
            keys.retain(|k| !k.senders.is_empty());
            if keys.is_empty() {
                self.targets.remove(&target);
            }
        }

        Some(target)
    }

    /// The own user's reaction event for `key` on `target`, if any.
    pub fn own_reaction(
        &self,
        target: &EventId,
        key: &str,
        own_user: &UserId,
    ) -> Option<OwnedEventId> {
        self.targets
            .get(target)?
            .iter()
            .find(|k| k.key == key)?
            .senders
            .iter()
            .find(|(sender, _)| sender == own_user)
            .map(|(_, id)| id.clone())
    }

    /// Render the suffix for `target`, empty if it has no reactions.
    pub fn suffix(
        &self,
        target: &EventId,
        own_user: &UserId,
        color: &str,
        self_color: &str,
    ) -> String {
        let Some(keys) = self.targets.get(target) else {
            return String::new();
        };

        let parts: Vec<String> = keys
            .iter()
            .map(|k| {
                let text = format!("{}{}", k.key, k.senders.len());
                if k.senders.iter().any(|(sender, _)| sender == own_user) {
                    format!("{self_color}{text}{color}")
                } else {
                    text
                }
            })
            .collect();

        format!(" {color}[{}]", parts.join(" "))
    }

    /// The suffix currently printed after `target`.
    pub fn rendered(&self, target: &EventId) -> String {
        self.rendered.get(target).cloned().unwrap_or_default()
    }

    pub fn set_rendered(&mut self, target: &EventId, suffix: String) {
        if suffix.is_empty() {
            self.rendered.remove(target);
        } else {
            self.rendered.insert(target.to_owned(), suffix);
        }
    }

    /// Forget the printed suffix, e.g. after the message was re-rendered.
    pub fn forget_rendered(&mut self, target: &EventId) {
        self.rendered.remove(target);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactionChange {
    Add,
    Remove,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ReactionInput {
    /// 1-based index of the message, counted from the most recent.
    pub index: usize,
    pub change: ReactionChange,
    pub key: String,
}

/// Parse wee-slack style reaction input: `[N]+emoji` or `[N]-emoji`, where
/// emoji is an emoji character or a `:shortcode:`.
pub fn parse_reaction_input(input: &str) -> Option<ReactionInput> {
    let input = input.trim_end();
    let digits = input.len() - input.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let (number, rest) = input.split_at(digits);
    let index = if number.is_empty() {
        1
    } else {
        number.parse::<usize>().ok().filter(|i| *i > 0)?
    };

    let mut chars = rest.chars();
    let change = match chars.next()? {
        '+' => ReactionChange::Add,
        '-' => ReactionChange::Remove,
        _ => return None,
    };
    let emoji = chars.as_str();

    let key = if let Some(name) =
        emoji.strip_prefix(':').and_then(|e| e.strip_suffix(':'))
    {
        emojis::get_by_shortcode(name)?.as_str().to_owned()
    } else if !emoji.is_empty()
        && emoji.chars().all(|c| !c.is_ascii() && !c.is_alphanumeric())
    {
        emoji.to_owned()
    } else {
        return None;
    };

    Some(ReactionInput { index, change, key })
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::{event_id, user_id};

    use super::*;

    fn reactions() -> Reactions {
        let mut r = Reactions::default();
        let target = event_id!("$msg:example.org").to_owned();
        r.add(
            event_id!("$r1:example.org").to_owned(),
            target.clone(),
            "👍".to_owned(),
            user_id!("@alice:example.org").to_owned(),
        );
        r.add(
            event_id!("$r2:example.org").to_owned(),
            target.clone(),
            "👍".to_owned(),
            user_id!("@me:example.org").to_owned(),
        );
        r.add(
            event_id!("$r3:example.org").to_owned(),
            target,
            "😄".to_owned(),
            user_id!("@bob:example.org").to_owned(),
        );
        r
    }

    #[test]
    fn suffix_counts_keys_in_arrival_order() {
        let r = reactions();
        let suffix = r.suffix(
            event_id!("$msg:example.org"),
            user_id!("@nobody:example.org"),
            "<c>",
            "<s>",
        );
        assert_eq!(suffix, " <c>[👍2 😄1]");
    }

    #[test]
    fn suffix_highlights_own_reactions() {
        let r = reactions();
        let suffix = r.suffix(
            event_id!("$msg:example.org"),
            user_id!("@me:example.org"),
            "<c>",
            "<s>",
        );
        assert_eq!(suffix, " <c>[<s>👍2<c> 😄1]");
    }

    #[test]
    fn duplicate_events_and_senders_count_once() {
        let mut r = reactions();
        let target = event_id!("$msg:example.org").to_owned();
        assert!(!r.add(
            event_id!("$r1:example.org").to_owned(),
            target.clone(),
            "👍".to_owned(),
            user_id!("@alice:example.org").to_owned(),
        ));
        assert!(!r.add(
            event_id!("$r4:example.org").to_owned(),
            target.clone(),
            "👍".to_owned(),
            user_id!("@alice:example.org").to_owned(),
        ));
        let suffix = r.suffix(&target, user_id!("@x:example.org"), "", "");
        assert_eq!(suffix, " [👍2 😄1]");
    }

    #[test]
    fn remove_updates_counts_and_drops_empty_keys() {
        let mut r = reactions();
        let target = event_id!("$msg:example.org");
        assert_eq!(
            r.remove(event_id!("$r3:example.org")).as_deref(),
            Some(target)
        );
        assert_eq!(r.suffix(target, user_id!("@x:example.org"), "", ""), " [👍2]");
        r.remove(event_id!("$r1:example.org"));
        r.remove(event_id!("$r2:example.org"));
        assert_eq!(r.suffix(target, user_id!("@x:example.org"), "", ""), "");
        assert!(r.remove(event_id!("$unknown:example.org")).is_none());
    }

    #[test]
    fn own_reaction_lookup() {
        let r = reactions();
        let target = event_id!("$msg:example.org");
        assert_eq!(
            r.own_reaction(target, "👍", user_id!("@me:example.org"))
                .as_deref(),
            Some(event_id!("$r2:example.org"))
        );
        assert!(r.own_reaction(target, "😄", user_id!("@me:example.org")).is_none());
    }

    #[test]
    fn parses_reaction_input() {
        assert_eq!(
            parse_reaction_input("+👍"),
            Some(ReactionInput { index: 1, change: ReactionChange::Add, key: "👍".to_owned() })
        );
        assert_eq!(
            parse_reaction_input("3-👍"),
            Some(ReactionInput { index: 3, change: ReactionChange::Remove, key: "👍".to_owned() })
        );
        assert_eq!(
            parse_reaction_input("2+:smile:").map(|r| (r.index, r.key)),
            Some((2, "😄".to_owned()))
        );
    }

    #[test]
    fn parses_common_shortcodes() {
        for name in ["thumbsup", "+1", "heart", "joy", "tada", "eyes", "smile", "pray"] {
            assert!(
                parse_reaction_input(&format!("+:{name}:")).is_some(),
                "{}",
                name
            );
        }
    }

    #[test]
    fn ignores_ordinary_messages() {
        for input in ["hello", "+1", "-", "+", "0+👍", "+:nosuchemoji:", "+ok", "a+👍", "+👍 thanks", "+été", "-Ünd"] {
            assert_eq!(parse_reaction_input(input), None, "{}", input);
        }
    }
}

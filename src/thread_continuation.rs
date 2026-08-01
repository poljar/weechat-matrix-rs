use std::collections::{HashMap, HashSet};

use matrix_sdk::{
    ruma::{EventId, OwnedEventId, OwnedRoomId, RoomId},
    Client,
};
use serde::{Deserialize, Serialize};

const STORE_KEY: &[u8] = b"org.weechat.matrix.thread_continuations.v1";
const MAX_UPGRADE_DEPTH: usize = 32;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub(crate) struct ThreadKey {
    pub room_id: OwnedRoomId,
    pub thread_root: OwnedEventId,
}

impl ThreadKey {
    pub(crate) fn new(room_id: &RoomId, thread_root: &EventId) -> Self {
        Self {
            room_id: room_id.to_owned(),
            thread_root: thread_root.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ThreadContinuation {
    pub source: ThreadKey,
    pub target: ThreadKey,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ThreadContinuationState {
    continuations: Vec<ThreadContinuation>,
}

impl ThreadContinuationState {
    pub(crate) async fn load(client: &Client) -> Result<Self, String> {
        let Some(bytes) = client
            .state_store()
            .get_custom_value(STORE_KEY)
            .await
            .map_err(|error| error.to_string())?
        else {
            return Ok(Self::default());
        };

        let state: Self = serde_json::from_slice(&bytes).map_err(|error| {
            format!("invalid persisted thread continuation data: {error}")
        })?;
        state.validate()?;
        Ok(state)
    }

    pub(crate) async fn save(&self, client: &Client) -> Result<(), String> {
        self.validate()?;
        let bytes =
            serde_json::to_vec(self).map_err(|error| error.to_string())?;
        client
            .state_store()
            .set_custom_value_no_read(STORE_KEY, bytes)
            .await
            .map_err(|error| error.to_string())
    }

    pub(crate) fn resolve(
        &self,
        source: &ThreadKey,
    ) -> Result<Option<ThreadKey>, String> {
        let by_source: HashMap<_, _> = self
            .continuations
            .iter()
            .map(|continuation| (&continuation.source, &continuation.target))
            .collect();
        let mut current = source;
        let mut visited = HashSet::new();

        for _ in 0..MAX_UPGRADE_DEPTH {
            let Some(target) = by_source.get(current) else {
                return Ok((current != source).then(|| current.clone()));
            };
            if !visited.insert(current.clone()) {
                return Err(
                    "cyclic Matrix thread continuation mapping".to_owned()
                );
            }
            current = target;
        }

        Err("Matrix thread continuation chain is too deep".to_owned())
    }

    pub(crate) fn insert(
        &mut self,
        source: ThreadKey,
        target: ThreadKey,
    ) -> Result<(), String> {
        if source == target {
            return Err(
                "Matrix thread continuation cannot point to itself".to_owned()
            );
        }
        if let Some(existing) = self
            .continuations
            .iter()
            .find(|continuation| continuation.source == source)
        {
            return (existing.target == target).then_some(()).ok_or_else(
                || {
                    "Matrix thread already has a different continuation"
                        .to_owned()
                },
            );
        }

        self.continuations.push(ThreadContinuation {
            source: source.clone(),
            target,
        });
        if let Err(error) = self.validate() {
            self.continuations
                .retain(|continuation| continuation.source != source);
            return Err(error);
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), String> {
        let mut sources = HashSet::new();
        for continuation in &self.continuations {
            if continuation.source == continuation.target {
                return Err(
                    "Matrix thread continuation cannot point to itself"
                        .to_owned(),
                );
            }
            if !sources.insert(&continuation.source) {
                return Err(
                    "duplicate Matrix thread continuation source".to_owned()
                );
            }
        }
        for source in sources {
            self.resolve(source)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::{EventId, RoomId};

    use super::{ThreadContinuationState, ThreadKey};

    fn key(room: &str, event: &str) -> ThreadKey {
        ThreadKey {
            room_id: RoomId::parse(room).unwrap(),
            thread_root: EventId::parse(event).unwrap(),
        }
    }

    #[test]
    fn zero_predecessor_has_no_route() {
        let state = ThreadContinuationState::default();
        assert_eq!(
            None,
            state
                .resolve(&key("!a:example.org", "$a:example.org"))
                .unwrap()
        );
    }

    #[test]
    fn persisted_mapping_round_trips_and_reuses_latest_target() {
        let source = key("!a:example.org", "$a:example.org");
        let middle = key("!b:example.org", "$b:example.org");
        let target = key("!c:example.org", "$c:example.org");
        let mut state = ThreadContinuationState::default();
        state.insert(source.clone(), middle.clone()).unwrap();
        state.insert(middle, target.clone()).unwrap();

        let encoded = serde_json::to_vec(&state).unwrap();
        let restored: ThreadContinuationState =
            serde_json::from_slice(&encoded).unwrap();
        restored.validate().unwrap();
        assert_eq!(Some(target), restored.resolve(&source).unwrap());
    }

    #[test]
    fn duplicate_creation_is_idempotent_but_conflicts_are_rejected() {
        let source = key("!a:example.org", "$a:example.org");
        let target = key("!b:example.org", "$b:example.org");
        let other = key("!b:example.org", "$other:example.org");
        let mut state = ThreadContinuationState::default();
        state.insert(source.clone(), target.clone()).unwrap();
        state.insert(source.clone(), target).unwrap();
        assert!(state.insert(source, other).is_err());
    }

    #[test]
    fn malformed_cycle_is_rejected() {
        let first = key("!a:example.org", "$a:example.org");
        let second = key("!b:example.org", "$b:example.org");
        let mut state = ThreadContinuationState::default();
        state.insert(first.clone(), second.clone()).unwrap();
        assert!(state.insert(second, first).is_err());
    }
}

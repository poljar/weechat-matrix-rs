use matrix_sdk::{
    deserialized_responses::AnySyncMessageEvent,
    events::{
        room::message::Relation,
        AnyMessageEvent,
    },
    identifiers::EventId,
};
use matrix_sdk::events::room::message::MessageEventContent;

pub trait Edit {
    fn is_edit(&self) -> bool;
    fn get_edit(&self) -> Option<(&EventId, &MessageEventContent)>;
}

impl Edit for MessageEventContent {
    fn is_edit(&self) -> bool {
        if let Some(Relation::Replacement(_)) = self.relates_to.as_ref() {
            self.new_content.is_some()
        } else {
            false
        }
    }

    fn get_edit(&self) -> Option<(&EventId, &MessageEventContent)> {
        if let Some(Relation::Replacement(r)) = self.relates_to.as_ref() {
            if let Some(content) = self.new_content.as_ref() {
                Some((&r.event_id, &*content))
            } else {
                None
            }
        } else {
            None
        }
    }
}

impl Edit for AnySyncMessageEvent {
    fn is_edit(&self) -> bool {
        if let AnySyncMessageEvent::RoomMessage(c) = self {
            c.content.is_edit()
        } else {
            false
        }
    }

    fn get_edit(&self) -> Option<(&EventId, &MessageEventContent)> {
        if let AnySyncMessageEvent::RoomMessage(c) = self {
            c.content.get_edit()
        } else {
            None
        }
    }
}

impl Edit for AnyMessageEvent {
    fn is_edit(&self) -> bool {
        if let AnyMessageEvent::RoomMessage(c) = self {
            c.content.is_edit()
        } else {
            false
        }
    }

    fn get_edit(&self) -> Option<(&EventId, &MessageEventContent)> {
        if let AnyMessageEvent::RoomMessage(c) = self {
            c.content.get_edit()
        } else {
            None
        }
    }
}

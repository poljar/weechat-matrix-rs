use matrix_sdk::{
    events::{
        room::message::{MessageEventContent, Relation},
        AnyMessageEvent, AnySyncMessageEvent,
    },
    identifiers::{EventId, UserId},
};

pub trait ToTag {
    fn to_tag(&self) -> String;
}

impl ToTag for EventId {
    fn to_tag(&self) -> String {
        format!("matrix_id_{}", self.as_str())
    }
}

impl ToTag for UserId {
    fn to_tag(&self) -> String {
        format!("matrix_sender_{}", self.as_str())
    }
}

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
            self.new_content
                .as_ref()
                .map(|content| (&r.event_id, content.as_ref()))
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

use matrix_sdk::ruma::{
    events::{
        room::message::{Relation, RoomMessageEventContent},
        AnyMessageLikeEvent, AnySyncMessageLikeEvent,
    },
    EventId, UserId,
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
    fn get_edit(&self) -> Option<(&EventId, &RoomMessageEventContent)>;
}

impl Edit for RoomMessageEventContent {
    fn is_edit(&self) -> bool {
        matches!(self.relates_to.as_ref(), Some(Relation::Replacement(_)))
    }

    fn get_edit(&self) -> Option<(&EventId, &RoomMessageEventContent)> {
        if let Some(Relation::Replacement(r)) = self.relates_to.as_ref() {
            Some((&r.event_id, &r.new_content))
        } else {
            None
        }
    }
}

impl Edit for AnySyncMessageLikeEvent {
    fn is_edit(&self) -> bool {
        if let AnySyncMessageLikeEvent::RoomMessage(e) = self {
            e.as_original()
                .map(|e| e.content.is_edit())
                .unwrap_or_default()
        } else {
            false
        }
    }

    fn get_edit(&self) -> Option<(&EventId, &RoomMessageEventContent)> {
        if let AnySyncMessageLikeEvent::RoomMessage(e) = self {
            e.as_original().map(|e| e.content.get_edit()).flatten()
        } else {
            None
        }
    }
}

impl Edit for AnyMessageLikeEvent {
    fn is_edit(&self) -> bool {
        if let AnyMessageLikeEvent::RoomMessage(c) = self {
            c.as_original()
                .map(|e| e.content.is_edit())
                .unwrap_or_default()
        } else {
            false
        }
    }

    fn get_edit(&self) -> Option<(&EventId, &RoomMessageEventContent)> {
        if let AnyMessageLikeEvent::RoomMessage(e) = self {
            e.as_original().map(|e| e.content.get_edit()).flatten()
        } else {
            None
        }
    }
}

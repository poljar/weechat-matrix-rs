use matrix_sdk::ruma::{
    events::{
        room::message::{MessageEventContent, MessageType, Relation},
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

pub trait VerificationEvent {
    fn is_verification(&self) -> bool;
}

impl VerificationEvent for AnySyncMessageEvent {
    fn is_verification(&self) -> bool {
        match self {
            AnySyncMessageEvent::KeyVerificationReady(_)
            | AnySyncMessageEvent::KeyVerificationStart(_)
            | AnySyncMessageEvent::KeyVerificationCancel(_)
            | AnySyncMessageEvent::KeyVerificationAccept(_)
            | AnySyncMessageEvent::KeyVerificationKey(_)
            | AnySyncMessageEvent::KeyVerificationMac(_)
            | AnySyncMessageEvent::KeyVerificationDone(_) => true,
            AnySyncMessageEvent::RoomMessage(m) => {
                if let MessageType::VerificationRequest(_) = m.content.msgtype {
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

impl Edit for MessageEventContent {
    fn is_edit(&self) -> bool {
        matches!(self.relates_to.as_ref(), Some(Relation::Replacement(_)))
    }

    fn get_edit(&self) -> Option<(&EventId, &MessageEventContent)> {
        if let Some(Relation::Replacement(r)) = self.relates_to.as_ref() {
            Some((&r.event_id, &r.new_content))
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

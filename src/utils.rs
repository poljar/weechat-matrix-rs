use matrix_sdk::{
    events::{
        room::message::{
            MessageEventContent, NoticeMessageEventContent, Relation,
            TextMessageEventContent,
        },
        AnyMessageEvent, AnySyncMessageEvent,
    },
    identifiers::EventId,
};

pub trait Edit {
    fn is_edit(&self) -> bool;
    fn get_edit(&self) -> Option<(&EventId, &MessageEventContent)>;
}

macro_rules! impl_relation {
    ($name:ident) => {
        impl Edit for $name {
            fn is_edit(&self) -> bool {
                if let Some(Relation::Replacement(_)) = self.relates_to.as_ref()
                {
                    self.new_content.is_some()
                } else {
                    false
                }
            }

            fn get_edit(&self) -> Option<(&EventId, &MessageEventContent)> {
                if let Some(Relation::Replacement(r)) = self.relates_to.as_ref()
                {
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
    };
}

impl_relation!(TextMessageEventContent);
impl_relation!(NoticeMessageEventContent);

impl Edit for MessageEventContent {
    fn is_edit(&self) -> bool {
        match self {
            MessageEventContent::Notice(n) => n.is_edit(),
            MessageEventContent::Text(t) => t.is_edit(),
            _ => false,
        }
    }

    fn get_edit(&self) -> Option<(&EventId, &MessageEventContent)> {
        match self {
            MessageEventContent::Notice(n) => n.get_edit(),
            MessageEventContent::Text(t) => t.get_edit(),
            _ => None,
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

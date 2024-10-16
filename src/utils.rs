use matrix_sdk::ruma::{
    events::{
        room::message::{
            MessageType, Relation, RoomMessageEventContent,
            RoomMessageEventContentWithoutRelation,
        },
        AnyMessageLikeEvent, AnySyncMessageLikeEvent, SyncMessageLikeEvent,
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
    fn get_edit(
        &self,
    ) -> Option<(&EventId, &RoomMessageEventContentWithoutRelation)>;
}

pub trait VerificationEvent {
    fn is_verification(&self) -> bool;
}

impl VerificationEvent for AnySyncMessageLikeEvent {
    fn is_verification(&self) -> bool {
        match self {
            AnySyncMessageLikeEvent::KeyVerificationReady(_)
            | AnySyncMessageLikeEvent::KeyVerificationStart(_)
            | AnySyncMessageLikeEvent::KeyVerificationCancel(_)
            | AnySyncMessageLikeEvent::KeyVerificationAccept(_)
            | AnySyncMessageLikeEvent::KeyVerificationKey(_)
            | AnySyncMessageLikeEvent::KeyVerificationMac(_)
            | AnySyncMessageLikeEvent::KeyVerificationDone(_) => true,
            AnySyncMessageLikeEvent::RoomMessage(m) => {
                if let SyncMessageLikeEvent::Original(m) = m {
                    if let MessageType::VerificationRequest(_) =
                        m.content.msgtype
                    {
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }
}

impl Edit for RoomMessageEventContent {
    fn is_edit(&self) -> bool {
        matches!(self.relates_to.as_ref(), Some(Relation::Replacement(_)))
    }

    fn get_edit(
        &self,
    ) -> Option<(&EventId, &RoomMessageEventContentWithoutRelation)> {
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

    fn get_edit(
        &self,
    ) -> Option<(&EventId, &RoomMessageEventContentWithoutRelation)> {
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

    fn get_edit(
        &self,
    ) -> Option<(&EventId, &RoomMessageEventContentWithoutRelation)> {
        if let AnyMessageLikeEvent::RoomMessage(e) = self {
            e.as_original().map(|e| e.content.get_edit()).flatten()
        } else {
            None
        }
    }
}

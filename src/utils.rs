use matrix_sdk::ruma::{
    events::{
        room::{
            message::{MessageType, Relation, RoomMessageEventContent},
            redaction::SyncRoomRedactionEvent,
        },
        AnyMessageLikeEvent, AnyMessageLikeEventContent,
        AnySyncMessageLikeEvent, AnySyncRoomEvent, MessageLikeEvent,
        SyncMessageLikeEvent,
    },
    EventId, UserId,
};

pub trait Content {
    fn content(&self) -> Option<AnyMessageLikeEventContent>;
}

impl Content for AnySyncMessageLikeEvent {
    fn content(&self) -> Option<AnyMessageLikeEventContent> {
        match self {
            AnySyncMessageLikeEvent::CallAnswer(e) => match e {
                SyncMessageLikeEvent::Original(e) => {
                    Some(e.content.to_owned().into())
                }
                SyncMessageLikeEvent::Redacted(_) => None,
            },
            AnySyncMessageLikeEvent::CallInvite(_) => None,
            AnySyncMessageLikeEvent::CallHangup(_) => None,
            AnySyncMessageLikeEvent::CallCandidates(_) => None,
            AnySyncMessageLikeEvent::KeyVerificationReady(_) => None,
            AnySyncMessageLikeEvent::KeyVerificationStart(_) => None,
            AnySyncMessageLikeEvent::KeyVerificationCancel(_) => None,
            AnySyncMessageLikeEvent::KeyVerificationAccept(_) => None,
            AnySyncMessageLikeEvent::KeyVerificationKey(_) => None,
            AnySyncMessageLikeEvent::KeyVerificationMac(_) => None,
            AnySyncMessageLikeEvent::KeyVerificationDone(_) => None,
            AnySyncMessageLikeEvent::Reaction(
                SyncMessageLikeEvent::Original(e),
            ) => Some(e.content.to_owned().into()),
            AnySyncMessageLikeEvent::RoomEncrypted(
                SyncMessageLikeEvent::Original(e),
            ) => Some(e.content.to_owned().into()),
            AnySyncMessageLikeEvent::RoomMessage(
                SyncMessageLikeEvent::Original(e),
            ) => Some(e.content.to_owned().into()),
            AnySyncMessageLikeEvent::RoomRedaction(
                SyncRoomRedactionEvent::Original(e),
            ) => Some(e.content.to_owned().into()),
            AnySyncMessageLikeEvent::Sticker(_) => None,
            _ => None,
        }
    }
}

impl Content for AnyMessageLikeEvent {
    fn content(&self) -> Option<AnyMessageLikeEventContent> {
        match self {
            AnyMessageLikeEvent::CallAnswer(e) => match e {
                MessageLikeEvent::Original(e) => {
                    Some(e.content.to_owned().into())
                }
                MessageLikeEvent::Redacted(_) => None,
            },
            AnyMessageLikeEvent::CallInvite(_) => None,
            AnyMessageLikeEvent::CallHangup(_) => None,
            AnyMessageLikeEvent::CallCandidates(_) => None,
            AnyMessageLikeEvent::KeyVerificationReady(_) => None,
            AnyMessageLikeEvent::KeyVerificationStart(_) => None,
            AnyMessageLikeEvent::KeyVerificationCancel(_) => None,
            AnyMessageLikeEvent::KeyVerificationAccept(_) => None,
            AnyMessageLikeEvent::KeyVerificationKey(_) => None,
            AnyMessageLikeEvent::KeyVerificationMac(_) => None,
            AnyMessageLikeEvent::KeyVerificationDone(_) => None,
            AnyMessageLikeEvent::Reaction(MessageLikeEvent::Original(e)) => {
                Some(e.content.to_owned().into())
            }
            AnyMessageLikeEvent::RoomEncrypted(MessageLikeEvent::Original(
                e,
            )) => Some(e.content.to_owned().into()),
            AnyMessageLikeEvent::RoomMessage(MessageLikeEvent::Original(e)) => {
                Some(e.content.to_owned().into())
            }
            AnyMessageLikeEvent::Sticker(_) => None,
            _ => None,
        }
    }
}

pub trait TransactionId {
    fn transaction_id(&self) -> Option<&matrix_sdk::ruma::TransactionId>;
}

impl TransactionId for AnySyncMessageLikeEvent {
    fn transaction_id(&self) -> Option<&matrix_sdk::ruma::TransactionId> {
        match self {
            AnySyncMessageLikeEvent::CallAnswer(_) => None,
            AnySyncMessageLikeEvent::CallInvite(_) => None,
            AnySyncMessageLikeEvent::CallHangup(_) => None,
            AnySyncMessageLikeEvent::CallCandidates(_) => None,
            AnySyncMessageLikeEvent::KeyVerificationReady(_) => None,
            AnySyncMessageLikeEvent::KeyVerificationStart(_) => None,
            AnySyncMessageLikeEvent::KeyVerificationCancel(_) => None,
            AnySyncMessageLikeEvent::KeyVerificationAccept(_) => None,
            AnySyncMessageLikeEvent::KeyVerificationKey(_) => None,
            AnySyncMessageLikeEvent::KeyVerificationMac(_) => None,
            AnySyncMessageLikeEvent::KeyVerificationDone(_) => None,
            AnySyncMessageLikeEvent::Reaction(_) => None,
            AnySyncMessageLikeEvent::RoomEncrypted(_) => None,
            AnySyncMessageLikeEvent::RoomMessage(
                SyncMessageLikeEvent::Original(e),
            ) => e.unsigned.transaction_id.as_deref(),
            AnySyncMessageLikeEvent::RoomRedaction(_) => None,
            AnySyncMessageLikeEvent::Sticker(_) => None,
            _ => None,
        }
    }
}

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

pub trait VerificationEvent {
    fn is_verification(&self) -> bool;
}

impl VerificationEvent for AnySyncRoomEvent {
    fn is_verification(&self) -> bool {
        match self {
            AnySyncRoomEvent::MessageLike(m) => m.is_verification(),
            AnySyncRoomEvent::State(_) => false,
            // | AnySyncRoomEvent::RedactedMessageLike(_)
            // | AnySyncRoomEvent::RedactedState(_) => false,
        }
    }
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
            AnySyncMessageLikeEvent::RoomMessage(
                SyncMessageLikeEvent::Original(m),
            ) => {
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
        if let AnySyncMessageLikeEvent::RoomMessage(
            SyncMessageLikeEvent::Original(c),
        ) = self
        {
            c.content.is_edit()
        } else {
            false
        }
    }

    fn get_edit(&self) -> Option<(&EventId, &RoomMessageEventContent)> {
        if let AnySyncMessageLikeEvent::RoomMessage(
            SyncMessageLikeEvent::Original(c),
        ) = self
        {
            c.content.get_edit()
        } else {
            None
        }
    }
}

impl Edit for AnyMessageLikeEvent {
    fn is_edit(&self) -> bool {
        if let AnyMessageLikeEvent::RoomMessage(MessageLikeEvent::Original(c)) =
            self
        {
            c.content.is_edit()
        } else {
            false
        }
    }

    fn get_edit(&self) -> Option<(&EventId, &RoomMessageEventContent)> {
        if let AnyMessageLikeEvent::RoomMessage(MessageLikeEvent::Original(c)) =
            self
        {
            c.content.get_edit()
        } else {
            None
        }
    }
}

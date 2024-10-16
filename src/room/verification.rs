use std::{cell::RefCell, rc::Rc};

use matrix_sdk::{
    encryption::verification::{SasVerification, VerificationRequest},
    ruma::{
        events::{
            key::verification::VerificationMethod, room::message::MessageType,
            AnySyncMessageLikeEvent,
        },
        UserId,
    },
};

use crate::{
    connection::Connection,
    render::{Render, StartVerificationContext, VerificationContext},
};

use super::{buffer::RoomBuffer, members::Members};

#[derive(Clone)]
pub struct Verification {
    own_user_id: Rc<UserId>,
    connection: Rc<RefCell<Option<Connection>>>,
    members: Members,
    buffer: RoomBuffer,
    inner: Rc<RefCell<Option<ActiveVerification>>>,
}

#[derive(Clone, Debug)]
enum ActiveVerification {
    Request(VerificationRequest),
    Sas(SasVerification),
}

impl From<VerificationRequest> for ActiveVerification {
    fn from(v: VerificationRequest) -> Self {
        Self::Request(v)
    }
}

impl From<SasVerification> for ActiveVerification {
    fn from(v: SasVerification) -> Self {
        Self::Sas(v)
    }
}

impl Verification {
    pub fn new(
        own_user_id: Rc<UserId>,
        connection: Rc<RefCell<Option<Connection>>>,
        members: Members,
        buffer: RoomBuffer,
    ) -> Self {
        Self {
            own_user_id,
            connection,
            members,
            buffer,
            inner: Rc::new(RefCell::new(None)),
        }
    }

    pub async fn confirm(&self) {
        let connection = self.connection.borrow().clone();

        if let Some(c) = connection {
            if let Some(ActiveVerification::Sas(verification)) =
                self.inner.borrow().clone()
            {
                let ret =
                    c.spawn(async move { verification.confirm().await }).await;
            }
        }
    }

    pub async fn accept(&self) {
        let connection = self.connection.borrow().clone();
        let verification = self.inner.borrow().clone();

        if let Some(c) = connection {
            if let Some(ActiveVerification::Request(verification)) =
                verification
            {
                let verification_clone = verification.clone();

                let ret = c
                    .spawn(async move {
                        verification
                            .accept_with_methods(vec![
                                VerificationMethod::SasV1,
                            ])
                            .await
                    })
                    .await;

                // We automatically start SAS verification here since it's the
                // only method we support.
                if let Some(sas) = c
                    .spawn(async move { verification_clone.start_sas().await })
                    .await
                    .unwrap()
                {
                    *self.inner.borrow_mut() = Some(sas.into());
                }
            }
        }
    }

    pub async fn handle_room_verification(
        &self,
        event: &AnySyncMessageLikeEvent,
    ) {
        // TODO remove this expect.
        let sender =
            self.members.get(event.sender()).await.expect(
                "Rendering a message but the sender isn't in the nicklist",
            );
        let own_member = self
            .members
            .get(&self.own_user_id)
            .await
            .expect("Own member missing from the store");
        let send_time = event.origin_server_ts();
        let connection = self.connection.borrow().clone();

        match event {
            AnySyncMessageLikeEvent::KeyVerificationReady(_) => {}
            AnySyncMessageLikeEvent::KeyVerificationStart(e) => {
                if let Some(connection) = connection {
                    let Some(e) = e.as_original() else {
                        // Unhandled redacted event
                        return;
                    };
                    let flow_id = &e.content.relates_to.event_id;

                    if let Some(sas) = connection
                        .client()
                        .encryption()
                        .get_verification(&e.sender, flow_id.as_str())
                        .await
                        .map(|s| s.sas())
                        .flatten()
                    {
                        let context = StartVerificationContext::Room(
                            e.sender.to_owned(),
                            sas.clone().into(),
                        );
                        let rendered = e.content.render_with_prefix(
                            send_time,
                            event.event_id(),
                            &sender,
                            &context,
                        );
                        self.buffer
                            .replace_verification_event(flow_id, rendered);
                        *self.inner.borrow_mut() = Some(sas.clone().into());

                        // We accept here automatically since the only method
                        // we're supporting is SAS verification
                        let ret = connection
                            .spawn(async move { sas.accept().await })
                            .await;
                    }
                }
            }
            AnySyncMessageLikeEvent::KeyVerificationCancel(_) => {
                self.inner.borrow_mut().take();
            }
            AnySyncMessageLikeEvent::KeyVerificationAccept(_) => {}
            AnySyncMessageLikeEvent::KeyVerificationKey(e) => {
                let Some(e) = e.as_original() else {
                    // Unhandled redacted event
                    return;
                };
                let flow_id = &e.content.relates_to.event_id;
                if let Some(ActiveVerification::Sas(sas)) =
                    self.inner.borrow().clone()
                {
                    if sas.can_be_presented() {
                        let rendered = e.content.render_with_prefix(
                            send_time,
                            event.event_id(),
                            &sender,
                            &sas,
                        );
                        self.buffer
                            .replace_verification_event(flow_id, rendered);
                    }
                }
            }
            AnySyncMessageLikeEvent::KeyVerificationMac(_) => {}
            AnySyncMessageLikeEvent::KeyVerificationDone(_) => {}
            AnySyncMessageLikeEvent::RoomMessage(e) => {
                let Some(e) = e.as_original() else {
                    // Unhandled redacted event
                    return;
                };
                if let MessageType::VerificationRequest(content) =
                    &e.content.msgtype
                {
                    let rendered = content.render_with_prefix(
                        send_time,
                        &e.event_id,
                        &sender.clone(),
                        &VerificationContext::Room(sender, own_member),
                    );
                    self.buffer.print_rendered_event(rendered);

                    if let Some(connection) = connection {
                        if let Some(verification) = connection
                            .client()
                            .encryption()
                            .get_verification_request(&e.sender, &e.event_id)
                            .await
                        {
                            *self.inner.borrow_mut() =
                                Some(verification.into());
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

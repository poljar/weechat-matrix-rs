use std::{cell::RefCell, rc::Rc};

use matrix_sdk::{
    ruma::{
        events::{
            key::verification::VerificationMethod, room::message::MessageType,
            AnySyncMessageEvent,
        },
        identifiers::UserId,
    },
    verification::{
        CancelInfo, SasVerification, Verification as SdkVerification,
        VerificationRequest,
    },
    Result,
};

use crate::{
    connection::Connection,
    render::{
        CancelContext, Render, VerificationContext, VerificationRequestContext,
    },
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

impl ActiveVerification {
    fn is_done(&self) -> bool {
        match self {
            ActiveVerification::Request(v) => v.is_done(),
            ActiveVerification::Sas(v) => v.is_done(),
        }
    }

    fn cancel_info(&self) -> Option<CancelInfo> {
        match self {
            ActiveVerification::Request(v) => v.cancel_info(),
            ActiveVerification::Sas(v) => v.cancel_info(),
        }
    }

    async fn cancel(&self) -> Result<()> {
        match self {
            ActiveVerification::Request(v) => v.cancel().await,
            ActiveVerification::Sas(v) => v.cancel().await,
        }
    }
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

    pub async fn cancel(&self) {
        let connection = self.connection.borrow().clone();

        if let Some(c) = connection {
            if let Some(verification) = self.inner.borrow().clone() {
                let ret =
                    c.spawn(async move { verification.cancel().await }).await;
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
                    .ok()
                    .flatten()
                {
                    *self.inner.borrow_mut() = Some(sas.into());
                }
            }
        }
    }

    pub async fn handle_room_verification(&self, event: &AnySyncMessageEvent) {
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
            AnySyncMessageEvent::KeyVerificationReady(_) => {}
            AnySyncMessageEvent::KeyVerificationStart(e) => {
                if let Some(connection) = connection {
                    let flow_id = &e.content.relates_to.event_id;

                    if let Some(sas) = connection
                        .client()
                        .get_verification(&e.sender, flow_id.as_str())
                        .await
                        .map(|s| s.sas())
                        .flatten()
                    {
                        let context = VerificationContext::Room(
                            sender.clone(),
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
            AnySyncMessageEvent::KeyVerificationAccept(_) => {}
            AnySyncMessageEvent::KeyVerificationKey(e) => {
                let flow_id = &e.content.relates_to.event_id;

                if let Some(ActiveVerification::Sas(sas)) =
                    self.inner.borrow().clone()
                {
                    let rendered = e.content.render_with_prefix(
                        send_time,
                        event.event_id(),
                        &sender,
                        &sas,
                    );
                    self.buffer.replace_verification_event(flow_id, rendered);
                }
            }
            AnySyncMessageEvent::KeyVerificationMac(_)
            | AnySyncMessageEvent::KeyVerificationDone(_) => {
                if self
                    .inner
                    .borrow()
                    .as_ref()
                    .map(|v| v.is_done())
                    .unwrap_or_default()
                {
                    self.inner.borrow_mut().take();
                    todo!("Print done");
                }
            }
            AnySyncMessageEvent::KeyVerificationCancel(e) => {
                let flow_id = &e.content.relates_to.event_id;

                let cancelled = self
                    .inner
                    .borrow()
                    .as_ref()
                    .map(|v| v.cancel_info())
                    .unwrap_or_default();

                if let Some(cancel_info) = cancelled {
                    let verification =
                        if let Some(v) = self.inner.borrow_mut().take() {
                            v
                        } else {
                            return;
                        };

                    let member = if cancel_info.cancelled_by_us() {
                        own_member.clone()
                    } else {
                        sender.clone()
                    };

                    let verification = match verification {
                        ActiveVerification::Request(r) => r.into(),
                        ActiveVerification::Sas(v) => {
                            SdkVerification::from(v).into()
                        }
                    };

                    let context = CancelContext::Room(member, verification);
                    let rendered = cancel_info.render_with_prefix(
                        send_time,
                        &e.event_id,
                        &sender.clone(),
                        &context,
                    );
                    self.buffer.replace_verification_event(flow_id, rendered);
                }
            }
            AnySyncMessageEvent::RoomMessage(e) => {
                if let MessageType::VerificationRequest(content) =
                    &e.content.msgtype
                {
                    if let Some(connection) = connection {
                        if let Some(verification) = connection
                            .client()
                            .get_verification_request(&e.sender, &e.event_id)
                            .await
                        {
                            let rendered = content.render_with_prefix(
                                send_time,
                                &e.event_id,
                                &sender.clone(),
                                &VerificationRequestContext::Room(
                                    verification.clone(),
                                    sender,
                                    own_member,
                                ),
                            );
                            self.buffer.print_rendered_event(rendered);

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
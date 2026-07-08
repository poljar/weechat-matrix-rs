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
    Error,
};
use weechat::{Prefix, Weechat};

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
    Sas {
        flow_id: String,
        verification: SasVerification,
    },
}

impl From<VerificationRequest> for ActiveVerification {
    fn from(v: VerificationRequest) -> Self {
        Self::Request(v)
    }
}

impl ActiveVerification {
    async fn cancel(self) -> Result<(), Error> {
        match self {
            ActiveVerification::Request(request) => request.cancel().await,
            ActiveVerification::Sas { verification, .. } => {
                verification.cancel().await
            }
        }
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

    pub fn release_sdk_state(&self) {
        self.inner.borrow_mut().take();
    }

    pub async fn confirm(&self) {
        let connection = self.connection.borrow().clone();

        if let Some(c) = connection {
            if let Some(ActiveVerification::Sas { verification, .. }) =
                self.inner.borrow().clone()
            {
                if let Err(e) =
                    c.spawn(async move { verification.confirm().await }).await
                {
                    self.print(&format!("Error confirming verification: {e}"));
                }
            }
        }
    }

    pub async fn cancel(&self) {
        let connection = self.connection.borrow().clone();
        let verification = self.inner.borrow_mut().take();

        if let (Some(c), Some(verification)) = (connection, verification) {
            match c.spawn(async move { verification.cancel().await }).await {
                Ok(()) => self.print("Verification canceled"),
                Err(e) => {
                    self.print(&format!("Error canceling verification: {e}"))
                }
            }
        } else {
            self.print("No active verification to cancel");
        }
    }

    pub async fn accept(&self) {
        let connection = self.connection.borrow().clone();
        let verification = self.inner.borrow().clone();

        if let Some(c) = connection {
            if let Some(ActiveVerification::Request(verification)) =
                verification
            {
                let flow_id = verification.flow_id().to_owned();
                let verification_clone = verification.clone();

                if let Err(e) = c
                    .spawn(async move {
                        verification
                            .accept_with_methods(vec![
                                VerificationMethod::SasV1,
                            ])
                            .await
                    })
                    .await
                {
                    self.print(&format!("Error accepting verification: {e}"));
                    return;
                }

                // We automatically start SAS verification here since it's the
                // only method we support.
                match c
                    .spawn(async move { verification_clone.start_sas().await })
                    .await
                {
                    Ok(Some(sas)) => {
                        *self.inner.borrow_mut() =
                            Some(ActiveVerification::Sas {
                                flow_id,
                                verification: sas,
                            });
                    }
                    Ok(None) => {
                        self.print("Could not start emoji verification");
                    }
                    Err(e) => {
                        self.print(&format!(
                            "Error starting emoji verification: {e}"
                        ));
                    }
                }
            }
        }
    }

    fn print(&self, message: &str) {
        if let Ok(buffer) = self.buffer.buffer_handle().upgrade() {
            buffer.print(&format!(
                "{}{}",
                Weechat::prefix(Prefix::Network),
                message
            ));
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
                        *self.inner.borrow_mut() =
                            Some(ActiveVerification::Sas {
                                flow_id: flow_id.to_string(),
                                verification: sas.clone(),
                            });

                        // We accept here automatically since the only method
                        // we're supporting is SAS verification
                        if let Err(e) = connection
                            .spawn(async move { sas.accept().await })
                            .await
                        {
                            self.print(&format!(
                                "Error accepting verification: {e}"
                            ));
                        }
                    }
                }
            }
            AnySyncMessageLikeEvent::KeyVerificationCancel(e) => {
                self.inner.borrow_mut().take();
                let Some(e) = e.as_original() else {
                    self.print("Verification canceled");
                    return;
                };
                self.print(&format!(
                    "Verification canceled: {} ({})",
                    e.content.reason, e.content.code
                ));
            }
            AnySyncMessageLikeEvent::KeyVerificationAccept(_) => {}
            AnySyncMessageLikeEvent::KeyVerificationKey(e) => {
                let Some(e) = e.as_original() else {
                    // Unhandled redacted event
                    return;
                };
                let flow_id = &e.content.relates_to.event_id;
                if let Some(ActiveVerification::Sas {
                    verification: sas, ..
                }) = self.inner.borrow().clone()
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
            AnySyncMessageLikeEvent::KeyVerificationDone(e) => {
                let Some(e) = e.as_original() else {
                    return;
                };
                let completed_flow = e.content.relates_to.event_id.as_str();
                let matches_active = self
                    .inner
                    .borrow()
                    .as_ref()
                    .map(|active| match active {
                        ActiveVerification::Request(request) => {
                            request.flow_id() == completed_flow
                        }
                        ActiveVerification::Sas { flow_id, .. } => {
                            flow_id == completed_flow
                        }
                    })
                    .unwrap_or(false);

                if matches_active {
                    self.inner.borrow_mut().take();
                    self.print("Verification done");
                }
            }
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
                        &VerificationContext::Room { own_member, sender },
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

        Weechat::bar_item_update("buffer_modes");
    }
}

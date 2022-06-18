use std::{
    cell::RefCell,
    collections::HashMap,
    convert::TryInto,
    rc::{Rc, Weak},
};

use weechat::{
    buffer::{
        Buffer, BufferBuilderAsync, BufferCloseCallback, BufferHandle,
        BufferInputCallbackAsync,
    },
    Weechat,
};

use qrcode::QrCode;

use matrix_sdk::{
    async_trait,
    encryption::verification::{
        CancelInfo, QrVerification, SasVerification,
        Verification as SdkVerification, VerificationRequest,
    },
    ruma::{
        events::{
            key::verification::VerificationMethod, room::message::MessageType,
            AnySyncMessageLikeEvent, AnyToDeviceEvent, SyncMessageLikeEvent,
        },
        OwnedUserId, UserId,
    },
    Error,
};

use crate::{
    connection::Connection,
    render::{CancelContext, Render, RenderedContent, RenderedLine},
};

#[derive(Clone)]
pub struct VerificationBuffer {
    inner: InnerVerificationBuffer,
    buffer: BufferHandle,
}

#[derive(Clone, Debug)]
pub enum Verification {
    Request(VerificationRequest),
    Sas(SasVerification),
    Qr(QrVerification),
}

impl TryInto<SdkVerification> for Verification {
    type Error = ();

    fn try_into(self) -> Result<SdkVerification, Self::Error> {
        match self {
            Verification::Request(_) => Err(()),
            Verification::Sas(s) => Ok(s.into()),
            Verification::Qr(qr) => Ok(qr.into()),
        }
    }
}

impl From<SasVerification> for Verification {
    fn from(s: SasVerification) -> Self {
        Self::Sas(s)
    }
}

impl From<VerificationRequest> for Verification {
    fn from(v: VerificationRequest) -> Self {
        Self::Request(v)
    }
}

impl From<QrVerification> for Verification {
    fn from(v: QrVerification) -> Self {
        Self::Qr(v)
    }
}

impl Verification {
    fn is_done(&self) -> bool {
        match self {
            Verification::Request(r) => r.is_done(),
            Verification::Sas(v) => v.is_done(),
            Verification::Qr(v) => v.is_done(),
        }
    }

    fn is_self_verification(&self) -> bool {
        match self {
            Verification::Request(v) => v.is_self_verification(),
            Verification::Sas(v) => v.is_self_verification(),
            Verification::Qr(v) => v.is_self_verification(),
        }
    }

    fn other_user_id(&self) -> &UserId {
        match self {
            Verification::Request(v) => v.other_user_id(),
            Verification::Sas(v) => v.other_user_id(),
            Verification::Qr(v) => v.other_user_id(),
        }
    }

    fn cancel_info(&self) -> Option<CancelInfo> {
        match self {
            Verification::Request(v) => v.cancel_info(),
            Verification::Sas(v) => v.cancel_info(),
            Verification::Qr(v) => v.cancel_info(),
        }
    }

    async fn accept(&self) -> Result<(), Error> {
        match self {
            Verification::Request(r) => r.accept().await,
            Verification::Sas(s) => s.accept().await,
            Verification::Qr(qr) => qr.confirm().await,
        }
    }

    async fn cancel(&self) -> Result<(), Error> {
        match self {
            Verification::Request(r) => r.cancel().await,
            Verification::Sas(s) => s.cancel().await,
            Verification::Qr(qr) => qr.cancel().await,
        }
    }
}

#[derive(Clone)]
struct InnerVerificationBuffer {
    verification: Rc<RefCell<Verification>>,
    connection: Rc<RefCell<Option<Connection>>>,
    verification_buffers:
        Weak<RefCell<HashMap<OwnedUserId, VerificationBuffer>>>,
}

impl InnerVerificationBuffer {
    fn print_done(&self, buffer: BufferHandle) {
        self.print_raw(buffer, "The verification finished successfully.");
    }

    fn print_cancel(&self, buffer: BufferHandle, info: Option<CancelInfo>) {
        if let Some(info) = info {
            let verification = match self.verification.borrow().clone() {
                Verification::Request(r) => r.into(),
                Verification::Sas(s) => SdkVerification::from(s).into(),
                Verification::Qr(q) => SdkVerification::from(q).into(),
            };

            let context = CancelContext::ToDevice(verification);

            let content = info.render(&context);
            self.print(buffer, content)
        } else {
            self.print_raw(buffer, "You cancelled the verification flow.");
        }
    }

    fn print_waiting(&self, buffer: BufferHandle) {
        self.print_raw(buffer, "Waiting for the other side to confirm...")
    }

    fn print(&self, buffer: BufferHandle, content: RenderedContent) {
        if let Ok(buffer) = buffer.upgrade() {
            for line in content.lines {
                let tags: Vec<&str> =
                    line.tags.iter().map(|t| t.as_str()).collect();
                buffer.print_date_tags(0, &tags, &line.message);
            }
        } else {
            Weechat::print("Error: The verification buffer has been closed");
        }
    }

    fn print_raw(&self, buffer: BufferHandle, message: &str) {
        if let Ok(buffer) = buffer.upgrade() {
            buffer.print(message)
        } else {
            Weechat::print("Error: Verification buffer has been closed")
        }
    }

    async fn start_sas(
        &self,
        request: VerificationRequest,
    ) -> Result<(), Error> {
        if let Some(c) = self.connection.borrow().clone() {
            if let Some(sas) =
                c.spawn(async move { request.start_sas().await }).await?
            {
                *self.verification.borrow_mut() = sas.into();
            }
        }

        Ok(())
    }

    pub async fn accept(&self, buffer: BufferHandle) -> Result<(), Error> {
        if let Some(c) = self.connection.borrow().clone() {
            let verification = self.verification.borrow().clone();
            let verification_clone = verification.clone();

            c.spawn(async move { verification_clone.accept().await })
                .await?;

            self.something(verification, buffer).await?;
        }

        Ok(())
    }

    pub async fn something(
        &self,
        verification: Verification,
        buffer: BufferHandle,
    ) -> Result<(), Error> {
        if let Verification::Request(request) = verification {
            if request
                .their_supported_methods()
                .unwrap_or_default()
                .contains(&VerificationMethod::QrCodeScanV1)
            {
                if let Some(code) = request
                    .generate_qr_code()
                    .await?
                    .and_then(|qr| qr.to_qr_code().ok())
                {
                    let content = <QrCode as Render>::render(&code, &());
                    self.print(buffer, content)
                } else {
                    self.start_sas(request).await?;
                }
            } else {
                self.start_sas(request).await?;
            }
        }

        Ok(())
    }

    pub async fn confirm(&self, buffer: BufferHandle) -> Result<(), Error> {
        if let Some(c) = self.connection.borrow().clone() {
            if let Verification::Sas(s) = self.verification.borrow().clone() {
                let sas = s.clone();
                c.spawn(async move { s.confirm().await }).await?;

                if sas.is_done() {
                    self.print_done(buffer);
                } else {
                    self.print_waiting(buffer);
                }
            } else if let Verification::Qr(qr) =
                self.verification.borrow().clone()
            {
                let qr_clone = qr.clone();
                c.spawn(async move { qr.confirm().await }).await?;

                if qr_clone.is_done() {
                    self.print_done(buffer);
                } else {
                    self.print_waiting(buffer);
                }
            } else {
                if let Ok(b) = buffer.upgrade() {
                    b.print("Error, can't confirm the verification yet");
                }
            }
        }

        Ok(())
    }

    pub async fn cancel(&self) -> Result<(), Error> {
        if let Some(c) = self.connection.borrow().clone() {
            let verification = self.verification.borrow().clone();
            c.spawn(async move { verification.cancel().await }).await?;
        }

        Ok(())
    }

    async fn handle_input(
        &mut self,
        buffer: BufferHandle,
        input: &str,
    ) -> Result<(), Error> {
        if input == "accept" {
            self.accept(buffer).await?;
        } else if input == "confirm" {
            self.confirm(buffer).await?;
        } else if input == "cancel" {
            self.cancel().await?;
        }

        Ok(())
    }
}

#[async_trait(?Send)]
impl BufferInputCallbackAsync for InnerVerificationBuffer {
    async fn callback(&mut self, buffer: BufferHandle, input: String) {
        if let Err(e) = self.handle_input(buffer.clone(), &input).await {
            if let Ok(buffer) = buffer.upgrade() {
                buffer.print(&format!(
                    "Error with the verification flow {:?}",
                    e
                ));
            }
        }
    }
}

impl BufferCloseCallback for InnerVerificationBuffer {
    fn callback(&mut self, _: &Weechat, _: &Buffer) -> Result<(), ()> {
        let inner = self.clone();

        if let Some(buffers) = self.verification_buffers.upgrade() {
            buffers
                .borrow_mut()
                .remove(self.verification.borrow().other_user_id());
        }

        if let Some(task) =
            Weechat::spawn_checked(async move { inner.cancel().await })
        {
            task.detach();
        }

        Ok(())
    }
}

impl VerificationBuffer {
    pub fn new(
        server_name: &str,
        sender: &UserId,
        verification: impl Into<Verification>,
        connection: Rc<RefCell<Option<Connection>>>,
        verification_buffers: &Rc<
            RefCell<HashMap<OwnedUserId, VerificationBuffer>>,
        >,
    ) -> Result<Self, ()> {
        let verification = verification.into();

        let inner = InnerVerificationBuffer {
            verification: Rc::new(RefCell::new(verification.clone())),
            connection,
            verification_buffers: Rc::downgrade(verification_buffers),
        };

        let buffer_name = format!("{}.{}.verification", server_name, sender);

        let buffer_handle = BufferBuilderAsync::new(&buffer_name)
            .input_callback(inner.clone())
            .close_callback(inner.clone())
            .build()?;

        let buffer = buffer_handle.upgrade()?;

        buffer.disable_nicklist();
        buffer.disable_nicklist_groups();
        buffer.enable_multiline();

        let buffer_name = if verification.is_self_verification() {
            "Self verification".to_owned()
        } else {
            format!("Verification with {}", sender)
        };

        buffer.set_short_name(&buffer_name);
        buffer.set_title(&buffer_name);
        buffer.set_localvar("server", server_name);

        let verification_buffer = Self {
            inner,
            buffer: buffer_handle,
        };

        match verification {
            Verification::Request(r) => verification_buffer.print_request(r),
            Verification::Sas(s) => verification_buffer.print_start(s.into()),
            Verification::Qr(_) => (),
        }

        Ok(verification_buffer)
    }

    pub fn buffer(&self) -> BufferHandle {
        self.buffer.clone()
    }

    pub fn accept(&self) {
        let buffer = self.buffer();
        let inner = self.inner.clone();
        Weechat::spawn(async move { inner.accept(buffer).await }).detach();
    }

    pub fn cancel(&self) {
        let inner = self.inner.clone();
        Weechat::spawn(async move { inner.cancel().await }).detach();
        self.inner.print_cancel(self.buffer(), None);
    }

    pub fn start_sas(&self) {
        if let Verification::Request(request) =
            self.inner.verification.borrow().clone()
        {
            let inner = self.inner.clone();
            Weechat::spawn(async move { inner.start_sas(request).await })
                .detach();
        } else {
            self.print_raw(&[], "Can't start emoji verification")
        }
    }

    pub fn confirm(&self) {
        let buffer = self.buffer();
        let inner = self.inner.clone();
        Weechat::spawn(async move { inner.confirm(buffer).await }).detach();
    }

    pub async fn update_qr(&self, qr: QrVerification) {
        *self.inner.verification.borrow_mut() = qr.into();
    }

    pub fn replace_verification(&self, verification: impl Into<Verification>) {
        *self.inner.verification.borrow_mut() = verification.into();
    }

    pub async fn update(&self, sas: SasVerification) -> Result<(), Error> {
        *self.inner.verification.borrow_mut() = sas.into();
        let verification = self.inner.verification.borrow().clone();

        if let Some(c) = self.inner.connection.borrow().clone() {
            c.spawn(async move { verification.accept().await }).await?;
        } else {
            // TODO print an error
        }

        Ok(())
    }

    pub fn handle_key_event(&self) {
        let verification = self.inner.verification.borrow().clone().try_into();

        if let Ok(SdkVerification::SasV1(sas)) = verification {
            self.print_sas(sas);
        }
    }

    pub fn handle_start_event(&self) {
        let verification = self.inner.verification.borrow().clone().try_into();

        if let Ok(verification) = verification {
            if let SdkVerification::SasV1(s) = &verification {
                if s.started_from_request() {
                    Weechat::print("AUTO ACCEPTING");
                    self.accept();
                } else {
                    Weechat::print("AUTO ACCEPTING");
                }
            }

            self.print_start(verification)
        }
    }

    fn handle_ready_event(&self) {
        let verification = self.inner.verification.borrow().clone();
        let inner = self.inner.clone();
        let buffer = self.buffer();

        Weechat::spawn(async move {
            let ret = inner.something(verification, buffer).await;
            if let Err(e) = ret {
                Weechat::print(&format!(
                    "Error readying the verification {:?}",
                    e
                ));
            }
        })
        .detach();
    }

    pub fn handle_request_event(&self) {
        let verification = self.inner.verification.borrow().clone().try_into();

        if let Ok(Verification::Request(r)) = verification {
            self.print_request(r)
        }
    }

    pub fn handle_done(&self) {
        if self.inner.verification.borrow().is_done() {
            self.inner.print_done(self.buffer.clone());
        }
    }

    pub async fn handle_event(&self, event: &AnyToDeviceEvent) {
        Weechat::print(&format!("Handling event {}", event.event_type()));

        if let Some(info) = self.inner.verification.borrow().cancel_info() {
            self.inner.print_cancel(self.buffer(), Some(info));
            return;
        }

        match event {
            AnyToDeviceEvent::KeyVerificationRequest(_) => {
                self.handle_request_event()
            }
            AnyToDeviceEvent::KeyVerificationStart(_) => {
                self.handle_start_event()
            }
            AnyToDeviceEvent::KeyVerificationReady(_) => {
                self.handle_ready_event()
            }
            AnyToDeviceEvent::KeyVerificationKey(_) => self.handle_key_event(),
            AnyToDeviceEvent::KeyVerificationMac(_)
            | AnyToDeviceEvent::KeyVerificationDone(_) => self.handle_done(),
            _ => {}
        }
    }

    pub async fn handle_room_event(&self, event: &AnySyncMessageLikeEvent) {
        if let Some(info) = self.inner.verification.borrow().cancel_info() {
            self.inner.print_cancel(self.buffer(), Some(info));
            return;
        }

        match event {
            AnySyncMessageLikeEvent::RoomMessage(
                SyncMessageLikeEvent::Original(m),
            ) => {
                if let MessageType::VerificationRequest(_) = &m.content.msgtype
                {
                    self.handle_request_event()
                }
            }
            AnySyncMessageLikeEvent::KeyVerificationStart(_) => {
                self.handle_start_event()
            }
            AnySyncMessageLikeEvent::KeyVerificationKey(_) => {
                self.handle_key_event();
            }
            AnySyncMessageLikeEvent::KeyVerificationMac(_)
            | AnySyncMessageLikeEvent::KeyVerificationDone(_) => {
                self.handle_done();
                if self.inner.verification.borrow().is_done() {
                    self.inner.print_done(self.buffer.clone());
                }
            }
            _ => {}
        }
    }

    fn print_raw(&self, tags: &[String], message: &str) {
        let tags: Vec<&str> = tags.iter().map(|t| t.as_str()).collect();

        if let Ok(buffer) = self.buffer.upgrade() {
            buffer.print_date_tags(0, &tags, message);
        } else {
            Weechat::print("Error: The verification buffer has been closed");
        }
    }

    fn print(&self, message: &RenderedContent) {
        for line in &message.lines {
            self.print_raw(&line.tags, &line.message);
        }
    }

    fn print_sas(&self, sas: SasVerification) {
        let (message, short_auth_string) = if sas.supports_emoji() {
            let emoji = if let Some(emoji) = sas.emoji() {
                emoji
            } else {
                return;
            };

            let (emojis, descriptions): (Vec<_>, Vec<_>) =
                emoji.iter().map(|e| (e.symbol, e.description)).unzip();
            let center_emoji = |emoji: &str| -> String {
                const EMOJI_WIDTH: usize = 2;
                // These are emojis that need VARIATION-SELECTOR-16
                // (U+FE0F) so that they are rendered with coloured
                // glyphs. For these, we need to add an extra space
                // after them so that they are rendered properly in
                // Weechat.
                const VARIATION_SELECTOR_EMOJIS: [&str; 7] =
                    ["☁️", "❤️", "☂️", "✏️", "✂️", "☎️", "✈️"];

                // Hack to make weechat behave properly when one of the
                // above is printed.
                let emoji = if VARIATION_SELECTOR_EMOJIS.contains(&emoji) {
                    format!("{} ", emoji)
                } else {
                    emoji.to_string()
                };

                // This is a trick to account for the fact that emojis
                // are wider than other monospace characters.
                let placeholder = ".".repeat(EMOJI_WIDTH);

                format!("{:^12}", placeholder).replace(&placeholder, &emoji)
            };

            let emoji_string = emojis
                .iter()
                .map(|e| center_emoji(e))
                .collect::<Vec<_>>()
                .join("");

            let description = descriptions
                .iter()
                .map(|d| format!("{:^12}", d))
                .collect::<Vec<_>>()
                .join("");

            (
                "Do the emojis match?".to_string(),
                [emoji_string, description].join("\n"),
            )
        } else {
            let decimals = if let Some(decimals) = sas.decimals() {
                decimals
            } else {
                return;
            };

            let decimals =
                format!("{} - {} - {}", decimals.0, decimals.1, decimals.2);
            ("Do the decimals match?".to_string(), decimals)
        };

        let content = RenderedContent {
            lines: vec![
                RenderedLine {
                    message,
                    tags: Default::default(),
                },
                RenderedLine {
                    message: short_auth_string,
                    tags: Default::default(),
                },
                RenderedLine {
                    message: "Confirm with '/verification confirm', \
                                      or cancel with '/verification cancel'"
                        .to_string(),
                    tags: Default::default(),
                },
            ],
        };

        self.print(&content)
    }

    fn print_request(&self, request: VerificationRequest) {
        let (message, tags) = if request.we_started() {
            ("You sent a verification request".to_string(), vec![])
        } else {
            let nick = request.other_user_id().to_string();

            let message = if request.is_self_verification() {
                format!("You sent a verification request from another \
                                device, accept the request with '/verification accept`")
            } else {
                format!(
                    "{} has sent a verification request accept \
                                with '/verification accept'",
                    nick
                )
            };

            (message, vec!["notify_highlight".to_string()])
        };

        let content = RenderedContent {
            lines: vec![RenderedLine { message, tags }],
        };

        self.print(&content)
    }

    fn print_start(&self, verification: SdkVerification) {
        let message = match &verification {
            SdkVerification::SasV1(sas) => {
                if sas.we_started() {
                    if sas.is_self_verification() {
                        format!(
                            "You have started an interactive emoji \
                             verification, accept on your other device.",
                        )
                    } else {
                        format!(
                            "You have started an interactive emoji \
                             verification, waiting for {} to accept",
                            sas.other_device().user_id()
                        )
                    }
                } else {
                    if sas.started_from_request() {
                        // We auto accept emoji verifications that start
                        // from a verification request, so don't print
                        // anything.
                        return;
                    } else {
                        if sas.is_self_verification() {
                            format!(
                                "You started an interactive emoji \
                                 verification on one of your devices, \
                                 accept with the '/verification \
                                 accept' command",
                            )
                        } else {
                            format!(
                                "{} has started an interactive emoji verifiaction \
                                 with you, accept with the '/verification \
                                 accept' command",
                                sas.other_device().user_id()
                            )
                        }
                    }
                }
            }
            SdkVerification::QrV1(qr) => {
                if qr.we_started() {
                    format!(
                        "Succesfully scanned the QR code, waiting for \
                                 the other side to confirm the scanning."
                    )
                } else {
                    if qr.is_self_verification() {
                        "The other device has scanned our QR code, \
                                confirm that it did so with \
                                '/verification confirm'"
                            .to_string()
                    } else {
                        format!(
                            "{} has scanned our QR code, confirm that he \
                                        has done so TODO",
                            verification.other_user_id(),
                        )
                    }
                }
            }
        };

        let tags = if verification.we_started() {
            vec![]
        } else {
            vec!["notify_highlight".to_string()]
        };

        let content = RenderedContent {
            lines: vec![RenderedLine { message, tags }],
        };

        self.print(&content)
    }
}

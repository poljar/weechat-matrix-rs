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
    ruma::{
        events::{
            key::verification::{
                key::KeyToDeviceEventContent, VerificationMethod,
            },
            AnyToDeviceEvent,
        },
        identifiers::UserId,
    },
    verification::{
        CancelInfo, QrVerification, SasVerification,
        Verification as SdkVerification, VerificationRequest,
    },
    Error,
};

use crate::{
    connection::Connection,
    render::{
        CancelContext, Render, RenderedContent, VerificationContext,
        VerificationRequestContext,
    },
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
    verification_buffers: Weak<RefCell<HashMap<UserId, VerificationBuffer>>>,
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

            if let Verification::Request(request) = verification {
                if request
                    .their_supported_methods()
                    .unwrap_or_default()
                    .contains(&VerificationMethod::QrCodeShowV1)
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
        verification_buffers: &Rc<RefCell<HashMap<UserId, VerificationBuffer>>>,
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
            format!("Verification with {}", sender)
        } else {
            "Self verification".to_owned()
        };

        buffer.set_short_name(&buffer_name);
        buffer.set_title(&buffer_name);
        buffer.set_localvar("server", server_name);

        Ok(Self {
            inner,
            buffer: buffer_handle,
        })
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

    pub async fn handle_event(&self, event: &AnyToDeviceEvent) {
        if let Some(info) = self.inner.verification.borrow().cancel_info() {
            self.inner.print_cancel(self.buffer(), Some(info));
            return;
        }

        match event {
            AnyToDeviceEvent::KeyVerificationRequest(e) => {
                if let Verification::Request(request) =
                    self.inner.verification.borrow().clone()
                {
                    let content = e
                        .content
                        .render(&VerificationRequestContext::ToDevice(request));

                    self.print(&content);
                }
            }
            AnyToDeviceEvent::KeyVerificationStart(e) => {
                let verification = self.inner.verification.borrow().clone();

                if let Ok(verification) = verification.try_into() {
                    let content = e
                        .content
                        .render(&VerificationContext::ToDevice(verification));

                    self.print(&content);
                }
            }
            AnyToDeviceEvent::KeyVerificationKey(e) => {
                self.print_sas(&e.content);
            }
            AnyToDeviceEvent::KeyVerificationMac(_)
            | AnyToDeviceEvent::KeyVerificationDone(_) => {
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

    fn print_sas(&self, content: &KeyToDeviceEventContent) {
        if let Verification::Sas(sas) = self.inner.verification.borrow().clone()
        {
            let message = content.render(&sas);
            self.print(&message);
        }
    }
}

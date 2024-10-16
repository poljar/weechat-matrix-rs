use std::{cell::RefCell, convert::TryInto, rc::Rc};

use weechat::{
    buffer::{
        Buffer, BufferBuilderAsync, BufferCloseCallback, BufferHandle,
        BufferInputCallbackAsync,
    },
    Weechat,
};

use qrcode::render::unicode::Dense1x2;

use matrix_sdk::{
    async_trait,
    encryption::verification::{
        QrVerification, SasVerification, Verification as SdkVerification,
        VerificationRequest,
    },
    ruma::{
        events::{
            key::verification::{
                key::ToDeviceKeyVerificationKeyEventContent, VerificationMethod,
            },
            AnyToDeviceEvent,
        },
        UserId,
    },
    Error,
};

use crate::{
    connection::Connection,
    render::{
        Render, RenderedContent, StartVerificationContext, VerificationContext,
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
    async fn accept(&self) -> Result<(), Error> {
        match self {
            Verification::Request(r) => r.accept().await,
            Verification::Sas(s) => s.accept().await,
            Verification::Qr(qr) => qr.confirm().await,
        }
    }

    async fn generate_qr_code(&self) -> Option<QrVerification> {
        match self {
            Verification::Request(r) => r.generate_qr_code().await.unwrap(),
            Verification::Sas(_) => None,
            Verification::Qr(_) => None,
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
}

impl InnerVerificationBuffer {
    fn print_done(&self, buffer: BufferHandle) {}

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
                    if let Some(qr_code) = request.generate_qr_code().await? {
                        if let Ok(code) = qr_code.to_qr_code() {
                            if let Ok(b) = buffer.upgrade() {
                                let string = code
                                    .render::<Dense1x2>()
                                    .light_color(Dense1x2::Dark)
                                    .dark_color(Dense1x2::Light)
                                    .build();
                                b.print(&string);
                            }
                        }
                    }
                } else if let Some(sas) =
                    c.spawn(async move { request.start_sas().await }).await?
                {
                    *self.verification.borrow_mut() = sas.into();
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
                }
            } else if let Verification::Qr(qr) =
                self.verification.borrow().clone()
            {
                c.spawn(async move { qr.confirm().await }).await?;

                // if qr.is_done() {
                //     self.print_done(buffer);
                // }
            } else if let Ok(b) = buffer.upgrade() {
                b.print("Error, can't confirm the verification yet");
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
        Weechat::spawn(async move { inner.cancel().await }).detach();

        Ok(())
    }
}

impl VerificationBuffer {
    pub fn new(
        server_name: &str,
        sender: &UserId,
        verification: impl Into<Verification>,
        connection: Rc<RefCell<Option<Connection>>>,
    ) -> Self {
        let inner = InnerVerificationBuffer {
            verification: Rc::new(RefCell::new(verification.into())),
            connection,
        };

        let buffer_name = format!("{}.verification", server_name);

        let buffer_handle = BufferBuilderAsync::new(&buffer_name)
            .input_callback(inner.clone())
            .close_callback(|_weechat: &Weechat, _buffer: &Buffer| {
                // TODO remove the roombuffer from the server here.
                // TODO leave the room if the plugin isn't unloading.
                Ok(())
            })
            .build()
            .expect("Can't create new room buffer");

        let buffer = buffer_handle
            .upgrade()
            .expect("Can't upgrade newly created buffer");

        buffer.disable_nicklist();
        buffer.disable_nicklist_groups();
        buffer.enable_multiline();

        buffer.set_localvar("server", server_name);

        Self {
            inner,
            buffer: buffer_handle,
        }
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
    }

    pub fn confirm(&self) {
        let buffer = self.buffer();
        let inner = self.inner.clone();
        Weechat::spawn(async move { inner.confirm(buffer).await }).detach();
    }

    pub async fn update_qr(&mut self, qr: QrVerification) {
        *self.inner.verification.borrow_mut() = qr.into();
    }

    pub async fn update(&mut self, sas: SasVerification) -> Result<(), Error> {
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
        match event {
            AnyToDeviceEvent::KeyVerificationRequest(e) => {
                if let Verification::Request(request) =
                    self.inner.verification.borrow().clone()
                {
                    let content = e
                        .content
                        .render(&VerificationContext::ToDevice(request));

                    self.print(&content);
                }
            }
            AnyToDeviceEvent::KeyVerificationStart(e) => {
                let verification = self.inner.verification.borrow().clone();

                if let Ok(verification) = verification.try_into() {
                    let content =
                        e.content.render(&StartVerificationContext::ToDevice(
                            e.sender.clone(),
                            verification,
                        ));

                    self.print(&content);
                }
            }
            AnyToDeviceEvent::KeyVerificationCancel(_) => {
                // let message =
                //     format!("The verification flow has been canceled");
                // self.print(&message);
            }
            AnyToDeviceEvent::KeyVerificationKey(e) => {
                self.print_sas(&e.content);
            }
            AnyToDeviceEvent::KeyVerificationMac(_) => {
                if let Verification::Sas(sas) =
                    self.inner.verification.borrow().clone()
                {
                    if sas.is_done() {
                        self.inner.print_done(self.buffer.clone());
                    }
                }
            }
            AnyToDeviceEvent::KeyVerificationDone(_) => {}
            _ => {}
        }
    }

    fn print(&self, message: &RenderedContent) {
        if let Ok(buffer) = self.buffer.upgrade() {
            for line in &message.lines {
                buffer.print_date_tags(0, &[], &line.message);
            }
        } else {
            Weechat::print("BUFFER CLOSED");
        }
    }

    fn print_sas(&self, content: &ToDeviceKeyVerificationKeyEventContent) {
        if let Verification::Sas(sas) = self.inner.verification.borrow().clone()
        {
            let message = content.render(&sas);
            self.print(&message);
        }
    }
}

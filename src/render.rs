use url::Url;

use matrix_sdk::{
    encryption::verification::{format_emojis, SasVerification, Verification},
    ruma::{
        events::{
            key::verification::{
                key::{
                    KeyVerificationKeyEventContent,
                    ToDeviceKeyVerificationKeyEventContent,
                },
                ready::{
                    KeyVerificationReadyEventContent,
                    ToDeviceKeyVerificationReadyEventContent,
                },
                request::ToDeviceKeyVerificationRequestEventContent,
                start::{
                    KeyVerificationStartEventContent,
                    ToDeviceKeyVerificationStartEventContent,
                },
            },
            room::{
                encrypted::RoomEncryptedEventContent,
                member::{MembershipChange, RoomMemberEventContent},
                message::{
                    AudioMessageEventContent, EmoteMessageEventContent,
                    FileMessageEventContent, ImageMessageEventContent,
                    KeyVerificationRequestEventContent,
                    LocationMessageEventContent, MessageFormat,
                    NoticeMessageEventContent, RedactedRoomMessageEventContent,
                    ServerNoticeMessageEventContent, TextMessageEventContent,
                    VideoMessageEventContent,
                },
                EncryptedFile, MediaSource,
            },
            OriginalSyncStateEvent, RedactedSyncMessageLikeEvent,
        },
        uint, EventId, MilliSecondsSinceUnixEpoch, MxcUri, OwnedUserId,
        TransactionId, UserId,
    },
};
use ruma_html::{
    matrix::{AnchorUri, MatrixElement},
    Html, NodeData, NodeRef, SanitizerConfig,
};

use weechat::{Prefix, Weechat};

use crate::{room::WeechatRoomMember, utils::ToTag};

/// The rendered version of an event.
pub struct RenderedEvent {
    /// The UNIX timestamp of the event.
    pub message_timestamp: isize,
    pub prefix: String,
    pub content: RenderedContent,
}

impl RenderedEvent {
    const MSG_TAGS: &'static [&'static str] = &["notify_message"];
    const SELF_TAGS: &'static [&'static str] =
        &["notify_none", "no_highlight", "self_msg"];

    pub fn add_self_tags(self) -> Self {
        self.add_tags(Self::SELF_TAGS)
    }

    pub fn add_msg_tags(self) -> Self {
        self.add_tags(Self::MSG_TAGS)
    }

    fn add_tags(mut self, tags: &[&str]) -> Self {
        for line in &mut self.content.lines {
            line.tags.extend(tags.iter().map(|tag| tag.to_string()))
        }

        self
    }

    pub fn add_reply_context(
        mut self,
        event_id: &EventId,
        sender: Option<&str>,
    ) -> Self {
        let reply_fallback = self.content.reply_fallback.take();
        let reply_sender = sender
            .map(ToOwned::to_owned)
            .or_else(|| {
                reply_fallback
                    .as_ref()
                    .and_then(|fallback| fallback.sender.clone())
            })
            .unwrap_or_else(|| event_id.as_str().to_owned());
        let mut tags = self
            .content
            .lines
            .first()
            .map(|line| line.tags.clone())
            .unwrap_or_default();
        tags.push("matrix_reply".to_owned());

        let mut context_lines = match reply_fallback {
            Some(fallback) => {
                let mut lines = vec![RenderedLine {
                    tags: tags.clone(),
                    message: format!("Reply to {}:", reply_sender),
                }];

                lines.extend(reply_quote_lines(&fallback.body).map(
                    |message| RenderedLine {
                        tags: tags.clone(),
                        message,
                    },
                ));

                lines
            }
            None => vec![RenderedLine {
                tags,
                message: format!("Reply to {}", reply_sender),
            }],
        };

        self.content.lines.splice(0..0, context_lines.drain(..));

        self
    }
}

#[derive(Debug)]
pub struct RenderedLine {
    /// The tags of the line.
    pub tags: Vec<String>,
    /// The message of the line.
    pub message: String,
}

#[derive(Debug)]
pub struct RenderedContent {
    /// The collection of lines that the event has.
    pub lines: Vec<RenderedLine>,
    reply_fallback: Option<ReplyFallback>,
}

impl RenderedContent {
    fn new(lines: Vec<RenderedLine>) -> Self {
        Self {
            lines,
            reply_fallback: None,
        }
    }

    fn with_reply_fallback(
        mut self,
        reply_fallback: Option<ReplyFallback>,
    ) -> Self {
        self.reply_fallback = reply_fallback;
        self
    }
}

#[derive(Clone, Debug)]
struct ReplyFallback {
    sender: Option<String>,
    body: String,
}

/// Trait allowing events to be rendered for Weechat.
pub trait Render {
    /// The event specific tags that should be attached to the rendered event.
    const TAGS: &'static [&'static str];

    /// Some events might need additional context to be rendered. For example,
    /// instead of displaying the MXID for the sender, we might want to display
    /// the disambiguated display name, which isn't available in the event.
    ///
    /// This allows the render implementation to be passed some additional data
    /// when rendering.
    type RenderContext;

    fn tags(&self) -> Vec<String> {
        Self::TAGS.iter().map(|t| t.to_string()).collect()
    }

    fn event_tags(
        &self,
        event_id: &EventId,
        sender: &UserId,
        nick: &str,
        color_name: &str,
    ) -> Vec<String> {
        let mut tags = self.tags();
        let event_tag = event_id.to_tag();
        let sender_tag = sender.to_tag();
        let nick_tag = format!("nick_{}", nick);
        let color = format!("prefix_nick_{}", color_name);
        tags.push(event_tag);
        tags.push(sender_tag);
        tags.push(nick_tag);
        tags.push(color);

        tags
    }

    fn prefix(&self, sender: &WeechatRoomMember) -> String {
        format!("{}\t", sender.nick_colored())
    }

    /// Render the event.
    fn render_with_prefix(
        &self,
        timestamp: MilliSecondsSinceUnixEpoch,
        event_id: &EventId,
        sender: &WeechatRoomMember,
        context: &Self::RenderContext,
    ) -> RenderedEvent {
        let prefix = self.prefix(sender);
        let mut content = self.render(context);
        let timestamp: i64 = (timestamp.0 / uint!(1000)).into();

        let tags = self.event_tags(
            event_id,
            sender.user_id(),
            &sender.nick(),
            sender.color(),
        );

        for line in &mut content.lines {
            line.tags = tags.clone();
        }

        RenderedEvent {
            prefix,
            message_timestamp: timestamp as isize,
            content,
        }
    }

    fn render_with_prefix_for_echo(
        &self,
        sender: &WeechatRoomMember,
        uuid: &TransactionId,
        context: &Self::RenderContext,
    ) -> RenderedEvent {
        let content = self.render_for_echo(uuid, context);
        let prefix = self.prefix(sender);

        RenderedEvent {
            prefix,
            message_timestamp: 0,
            content,
        }
    }

    fn render_for_echo(
        &self,
        uuid: &TransactionId,
        context: &Self::RenderContext,
    ) -> RenderedContent {
        let mut content = self.render(context);
        let uuid_tag = format!("matrix_echo_{}", uuid);

        for line in &mut content.lines {
            let message = Weechat::remove_color(&line.message);
            line.message = format!(
                "{}{}{}",
                Weechat::color_pair("darkgray", "default"),
                message,
                Weechat::color("reset")
            );
            line.tags.push(uuid_tag.clone())
        }

        content
    }

    fn render(&self, context: &Self::RenderContext) -> RenderedContent;
}

impl Render for TextMessageEventContent {
    const TAGS: &'static [&'static str] = &["matrix_text"];
    type RenderContext = ();

    fn render(&self, _: &Self::RenderContext) -> RenderedContent {
        if let Some(body) = self.formatted_body() {
            render_formatted_message_body(body, self.tags())
        } else {
            render_plain_message_body(self.body(), self.tags())
        }
    }
}

impl Render for EmoteMessageEventContent {
    const TAGS: &'static [&'static str] = &["matrix_emote"];
    type RenderContext = WeechatRoomMember;

    fn prefix(&self, _: &WeechatRoomMember) -> String {
        Weechat::prefix(Prefix::Action)
    }

    fn render(&self, sender: &Self::RenderContext) -> RenderedContent {
        let message = format!(
            "{} {}",
            sender.nick(),
            self.formatted_body()
                .map(formatted_body_to_plain_text)
                .unwrap_or_else(|| self.body().to_owned())
        );

        let line = RenderedLine {
            message,
            tags: self.tags(),
        };

        RenderedContent::new(vec![line])
    }
}

impl Render for LocationMessageEventContent {
    const TAGS: &'static [&'static str] = &["matrix_location"];
    type RenderContext = WeechatRoomMember;

    fn prefix(&self, _: &WeechatRoomMember) -> String {
        Weechat::prefix(Prefix::Action)
    }

    fn render(&self, sender: &Self::RenderContext) -> RenderedContent {
        let message = format!(
            "{} has shared a location: {color_delimiter}<{color_reset}{}{color_delimiter}>\
            [{color_reset}{}{color_delimiter}]{color_reset}",
            sender.nick(),
            self.body,
            self.geo_uri,
            color_delimiter = Weechat::color("color_delimiter"),
            color_reset = Weechat::color("reset")
        );

        let line = RenderedLine {
            message,
            tags: self.tags(),
        };

        RenderedContent::new(vec![line])
    }
}

impl Render for NoticeMessageEventContent {
    const TAGS: &'static [&'static str] = &["matrix_notice"];
    type RenderContext = WeechatRoomMember;

    fn prefix(&self, _: &WeechatRoomMember) -> String {
        Weechat::prefix(Prefix::Network)
    }

    fn render(&self, sender: &Self::RenderContext) -> RenderedContent {
        let message = format!(
            "{color_notice}Notice\
            {color_delim}({color_reset}{}{color_delim}){color_reset}: {}",
            sender.nick(),
            self.formatted_body()
                .map(formatted_body_to_plain_text)
                .unwrap_or_else(|| self.body().to_owned()),
            color_notice = Weechat::color("irc.color.notice"),
            color_delim = Weechat::color("chat_delimiters"),
            color_reset = Weechat::color("reset"),
        );

        let line = RenderedLine {
            message,
            tags: self.tags(),
        };

        RenderedContent::new(vec![line])
    }
}

impl Render for ServerNoticeMessageEventContent {
    const TAGS: &'static [&'static str] = &["matrix_server_notice"];
    type RenderContext = WeechatRoomMember;

    fn prefix(&self, _: &WeechatRoomMember) -> String {
        Weechat::prefix(Prefix::Network)
    }

    fn render(&self, sender: &Self::RenderContext) -> RenderedContent {
        let message = format!(
            "{color_notice}Server notice\
            {color_delim}({color_reset}{}{color_delim}){color_reset}: {}",
            sender.nick(),
            self.body,
            color_notice = Weechat::color("irc.color.notice"),
            color_delim = Weechat::color("chat_delimiters"),
            color_reset = Weechat::color("reset"),
        );

        let line = RenderedLine {
            message,
            tags: self.tags(),
        };

        RenderedContent::new(vec![line])
    }
}

/// Create an HTTP download path from a matrix content URI
fn mxc_to_http_download_path(
    mxc_url: Url,
) -> Result<String, Box<dyn std::error::Error>> {
    Ok(format!(
        "/_matrix/media/r0/download/{server_name}{media_id}",
        server_name = mxc_url.host_str().ok_or("Missing host")?,
        media_id = mxc_url.path(),
    ))
}

/// Convert a matrix content URI to HTTP(s), respecting a user's homeserver
fn mxc_to_http(
    mxc_url: &MxcUri,
    homeserver: &Url,
) -> Result<String, Box<dyn std::error::Error>> {
    let url = url::Url::parse(mxc_url.as_str())?;

    if url.scheme() != "mxc" {
        return Err("URL missing MXC scheme".into());
    }

    if url.path().is_empty() {
        return Err("URL missing path".into());
    }

    Ok(homeserver
        .join(&mxc_to_http_download_path(url)?)?
        .to_string())
}

/// Convert a matrix content URI to an encrypted mxc URI, respecting a user's homeserver.
///
/// The return value of this function will have a URI schema of emxc://. The path of the URI will
/// be converted just like the mxc_to_http() function does, but it will also contain query
/// parameters that are necessary to decrypt the payload the URI is pointing to.
///
/// This function is useful to present a clickable URI that can be passed to a plumber program that
/// will download and decrypt the content that the matrix content URI is pointing to.
///
/// The returned URI should never be converted to http and opened directly, as that would expose
/// the decryption parameters to any middleman or ISP.
fn mxc_to_emxc(
    mxc_url: &MxcUri,
    homeserver: &Url,
    encrypted: &EncryptedFile,
) -> Result<String, Box<dyn std::error::Error>> {
    let url = url::Url::parse(mxc_url.as_str())?;

    if url.scheme() != "mxc" {
        return Err("URL missing MXC scheme".into());
    }

    if url.path().is_empty() {
        return Err("URL missing path".into());
    }

    let host_str = format!(
        "emxc://{}",
        homeserver
            .host_str()
            .ok_or("Missing homeserver host string")?
    );

    let mut emxc_url = url::Url::parse(&host_str)?;
    emxc_url
        .set_port(homeserver.port_or_known_default())
        .map_err(|_| "Can't set port")?;

    emxc_url = emxc_url.join(&mxc_to_http_download_path(url)?)?;

    // Add query parameters
    emxc_url
        .query_pairs_mut()
        .append_pair("key", &encrypted.key.k.encode())
        .append_pair(
            "hash",
            &encrypted
                .hashes
                .get("sha256")
                .ok_or("Missing sha256 hash")?
                .encode(),
        )
        .append_pair("iv", &encrypted.iv.encode());

    Ok(emxc_url.to_string())
}

fn media_download_command(source: &MediaSource) -> Option<String> {
    match source {
        MediaSource::Plain(url) => {
            Some(format!("/matrix media download {} [file]", url.as_str()))
        }
        MediaSource::Encrypted(_) => None,
    }
}

fn media_download_message<C: HasUrlOrFile>(content: &C) -> Option<String> {
    media_download_command(content.source()).map(|command| {
        format!(
            "attached `{}`, authenticated download: {}",
            content.body(),
            command
        )
    })
}

impl<C: HasUrlOrFile> Render for C {
    type RenderContext = Url;
    const TAGS: &'static [&'static str] = &["matrix_media"];

    fn render(&self, homeserver: &Self::RenderContext) -> RenderedContent {
        // Convert MXC to HTTP(s) or EMXC, but fallback to MXC if unable to.
        let mxc_url = match self.encrypted_file() {
            Some(encrypted_file) => {
                mxc_to_emxc(self.resolve_url(), homeserver, encrypted_file)
            }
            None => mxc_to_http(self.resolve_url(), homeserver),
        }
        .unwrap_or_else(|_| self.resolve_url().to_string());

        let message = media_download_message(self).unwrap_or_else(|| {
            format!(
                "{color_delimiter}<{color_reset}{}{color_delimiter}>\
                    [{color_reset}{}{color_delimiter}]{color_reset}",
                self.body(),
                mxc_url,
                color_delimiter = Weechat::color("color_delimiter"),
                color_reset = Weechat::color("reset")
            )
        });

        let line = RenderedLine {
            message,
            tags: self.tags(),
        };

        RenderedContent::new(vec![line])
    }
}

impl Render for RoomEncryptedEventContent {
    const TAGS: &'static [&'static str] = &["matrix_encrypted"];
    type RenderContext = ();

    fn render(&self, _: &Self::RenderContext) -> RenderedContent {
        let message = format!(
            "{}<{}Unable to decrypt message{}>{}",
            Weechat::color("chat_delimiters"),
            Weechat::color("logger.color.backlog_line"),
            Weechat::color("chat_delimiters"),
            Weechat::color("reset"),
        );

        let line = RenderedLine {
            message,
            // TODO: add tags that allow us decrypt the event at a later point in
            // time, sender key, algorithm, session id.
            tags: self.tags(),
        };

        RenderedContent::new(vec![line])
    }
}

impl Render for RedactedSyncMessageLikeEvent<RedactedRoomMessageEventContent> {
    type RenderContext = WeechatRoomMember;
    const TAGS: &'static [&'static str] = &["matrix_redacted"];

    fn render(&self, redacter: &Self::RenderContext) -> RenderedContent {
        // TODO: add the redaction reason.
        let message = format!(
            "{}<{}Message redacted by: {}{}>{}",
            Weechat::color("chat_delimiters"),
            Weechat::color("logger.color.backlog_line"),
            redacter.nick(),
            Weechat::color("chat_delimiters"),
            Weechat::color("reset"),
        );

        let line = RenderedLine {
            message,
            tags: self.tags(),
        };

        RenderedContent::new(vec![line])
    }
}

pub enum StartVerificationContext {
    Room(OwnedUserId, Verification),
    ToDevice(OwnedUserId, Verification),
}

impl StartVerificationContext {
    fn sender(&self) -> &UserId {
        match self {
            StartVerificationContext::Room(s, _) => &s,
            StartVerificationContext::ToDevice(s, _) => &s,
        }
    }

    fn verification(&self) -> &Verification {
        match self {
            StartVerificationContext::Room(_, v) => &v,
            StartVerificationContext::ToDevice(_, v) => &v,
        }
    }

    fn is_self_verification(&self) -> bool {
        self.verification().is_self_verification()
    }
}

macro_rules! render_start_content {
    ($type: ident) => {
        impl Render for $type {
            const TAGS: &'static [&'static str] = &[];

            type RenderContext = StartVerificationContext;

            fn prefix(&self, _: &WeechatRoomMember) -> String {
                Weechat::prefix(Prefix::Network)
            }

            fn render(&self, context: &Self::RenderContext) -> RenderedContent {
                let message = match context.verification() {
                    Verification::SasV1(sas) => {
                        if context.sender() == sas.own_user_id() {
                            if context.is_self_verification() {
                                if sas.started_from_request() {
                                    // We auto accept emoji verifications that start
                                    // from a verification request, so don't print
                                    // anything.
                                    return RenderedContent::new(vec![]);
                                } else {
                                    format!(
                                        "You have started an interactive emoji \
                                            verification, accept on your other device.",
                                    )
                                }
                            } else {
                                format!(
                                    "You have started an interactive emoji \
                                        verification, waiting for {} to accept",
                                    sas.other_device().user_id()
                                )
                            }
                        } else {
                            if sas.started_from_request() {
                                format!(
                                    "{} has started an interactive emoji verification \
                                        with you, waiting for emojis",
                                    sas.other_device().user_id()
                                )
                            } else {
                                // We auto accept emoji verifications that start
                                // from a verification request, so don't print
                                // anything.
                                return RenderedContent::new(vec![]);
                            }
                        }
                    }
                    Verification::QrV1(_) => {
                        // We don't support QR code scanning, so if there's an QR
                        // code verification struct it's because someone else
                        // scanned our QR code.
                        format!(
                            "{} has scanned our QR code, confirm that he \
                                has done so TODO",
                            context.sender(),
                        )
                    }
                    _ => unreachable!(),
                };

                RenderedContent::new(vec![RenderedLine {
                        message,
                        tags: self.tags(),
                    }])
            }
        }
    };
}

render_start_content!(KeyVerificationStartEventContent);
render_start_content!(ToDeviceKeyVerificationStartEventContent);

pub enum VerificationContext {
    Room {
        own_member: WeechatRoomMember,
        sender: WeechatRoomMember,
    },
    ToDevice,
}

macro_rules! render_request_content {
    ($type: ident) => {
        impl Render for $type {
            const TAGS: &'static [&'static str] = &[];

            type RenderContext = VerificationContext;

            fn prefix(&self, _: &WeechatRoomMember) -> String {
                Weechat::prefix(Prefix::Network)
            }

            fn render(&self, context: &Self::RenderContext) -> RenderedContent {
                let message = match context {
                    VerificationContext::Room { own_member, sender } => {
                        if own_member == sender {
                            "You sent a verification request".to_string()
                        } else {
                            format!(
                                "{} has sent a verification request",
                                sender.nick_colored()
                            )
                        }
                    }
                    VerificationContext::ToDevice => {
                        format!("You have requested this device to be verified")
                    }
                };

                RenderedContent::new(vec![RenderedLine {
                    message,
                    tags: self.tags(),
                }])
            }
        }
    };
}

render_request_content!(KeyVerificationRequestEventContent);
render_request_content!(ToDeviceKeyVerificationRequestEventContent);

macro_rules! render_ready_content {
    ($type: ident) => {
        impl Render for $type {
            const TAGS: &'static [&'static str] = &[];

            type RenderContext = (WeechatRoomMember, WeechatRoomMember);

            fn prefix(&self, _: &WeechatRoomMember) -> String {
                Weechat::prefix(Prefix::Network)
            }

            fn render(&self, context: &Self::RenderContext) -> RenderedContent {
                let (own_mebmer, sender) = context;

                let message = if own_mebmer == sender {
                    "You answered the verification request".to_string()
                } else {
                    format!(
                        "{} has answered the verification request",
                        sender.nick_colored()
                    )
                };

                RenderedContent::new(vec![RenderedLine {
                    message,
                    tags: self.tags(),
                }])
            }
        }
    };
}

render_ready_content!(KeyVerificationReadyEventContent);
render_ready_content!(ToDeviceKeyVerificationReadyEventContent);

fn render_sas_short_auth_string(sas: &SasVerification) -> Vec<String> {
    if sas.supports_emoji() {
        if let Some(emojis) = sas.emoji() {
            return format_emojis(emojis)
                .lines()
                .map(ToOwned::to_owned)
                .collect();
        }
    }

    if let Some((first, second, third)) = sas.decimals() {
        return vec![format!("{first:04} {second:04} {third:04}")];
    }

    vec!["Short authentication string is not ready yet".to_owned()]
}

macro_rules! render_key_content {
    ($type: ident) => {
        impl Render for $type {
            const TAGS: &'static [&'static str] = &[];
            type RenderContext = SasVerification;

            fn prefix(&self, _: &WeechatRoomMember) -> String {
                Weechat::prefix(Prefix::Network)
            }

            fn render(&self, sas: &Self::RenderContext) -> RenderedContent {
                let (message, short_auth_string) = if sas.supports_emoji() {
                    ("Do the emojis match?", render_sas_short_auth_string(sas))
                } else {
                    (
                        "Do the decimals match?",
                        render_sas_short_auth_string(sas),
                    )
                };

                let lines = std::iter::once(message.to_owned())
                    .chain(short_auth_string)
                    .map(|message| RenderedLine {
                        message,
                        tags: self.tags(),
                    })
                    .collect();

                RenderedContent::new(lines)
            }
        }
    };
}

render_key_content!(KeyVerificationKeyEventContent);
render_key_content!(ToDeviceKeyVerificationKeyEventContent);

fn render_formatted_message_body(
    body: &str,
    tags: Vec<String>,
) -> RenderedContent {
    let body = formatted_message_body_to_plain_text(body);

    render_plain_message_body(&body.text, tags)
        .with_reply_fallback(body.reply_fallback)
}

fn render_plain_message_body(body: &str, tags: Vec<String>) -> RenderedContent {
    let lines = body
        .lines()
        .map(|l| RenderedLine {
            message: l.to_owned(),
            tags: tags.clone(),
        })
        .collect();

    RenderedContent::new(lines)
}

fn formatted_body_to_plain_text(body: &str) -> String {
    formatted_message_body_to_plain_text(body).text
}

struct FormattedMessageBody {
    text: String,
    reply_fallback: Option<ReplyFallback>,
}

fn formatted_message_body_to_plain_text(body: &str) -> FormattedMessageBody {
    let html = Html::parse(body);
    html.sanitize_with(&SanitizerConfig::compat());

    let mut reply_fallback = None;

    for node in html.children() {
        if reply_fallback.is_none() {
            reply_fallback = render_reply_fallback(&node);
        }
    }

    html.sanitize();

    let mut output = String::new();
    let mut context = HtmlRenderContext::default();

    for node in html.children() {
        render_html_node(&node, &mut output, &mut context);
    }

    FormattedMessageBody {
        text: trim_newlines(output),
        reply_fallback,
    }
}

#[derive(Clone, Default)]
struct HtmlRenderContext {
    lists: Vec<ListContext>,
    in_pre: bool,
}

#[derive(Clone)]
enum ListContext {
    Unordered,
    Ordered { next: i64 },
}

fn render_html_node(
    node: &NodeRef,
    output: &mut String,
    context: &mut HtmlRenderContext,
) {
    match node.data() {
        NodeData::Text(text) => output.push_str(&text.borrow()),
        NodeData::Element(element) => match element.to_matrix().element {
            MatrixElement::Br => push_newline(output),
            MatrixElement::Hr => {
                push_newline(output);
                output.push_str("---");
                push_newline(output);
            }
            MatrixElement::P
            | MatrixElement::Div(_)
            | MatrixElement::Details
            | MatrixElement::Summary
            | MatrixElement::Table
            | MatrixElement::Thead
            | MatrixElement::Tbody
            | MatrixElement::Tr
            | MatrixElement::Caption
            | MatrixElement::H(_) => {
                push_newline(output);
                render_html_children(node, output, context);
                push_newline(output);
            }
            MatrixElement::Blockquote => {
                let mut quote = String::new();
                let mut quote_context = context.clone();

                render_html_children(node, &mut quote, &mut quote_context);

                push_newline(output);
                output.push_str(&prefix_lines(trim_newlines(quote), "> "));
                push_newline(output);
            }
            MatrixElement::Ul => {
                context.lists.push(ListContext::Unordered);
                push_newline(output);
                render_html_children(node, output, context);
                context.lists.pop();
                push_newline(output);
            }
            MatrixElement::Ol(ordered) => {
                context.lists.push(ListContext::Ordered {
                    next: ordered.start.unwrap_or(1),
                });
                push_newline(output);
                render_html_children(node, output, context);
                context.lists.pop();
                push_newline(output);
            }
            MatrixElement::Th | MatrixElement::Td => {
                render_html_children(node, output, context);
                output.push('\t');
            }
            MatrixElement::Li => {
                push_newline(output);
                output.push_str(
                    &"  ".repeat(context.lists.len().saturating_sub(1)),
                );
                output.push_str(&list_marker(context));
                render_html_children(node, output, context);
            }
            MatrixElement::A(anchor) => {
                let start = output.len();
                render_html_children(node, output, context);
                if let Some(href) =
                    anchor.href.and_then(|href| anchor_uri_to_string(&href))
                {
                    if output[start..].trim() != href {
                        output.push_str(" <");
                        output.push_str(&href);
                        output.push('>');
                    }
                }
            }
            MatrixElement::B | MatrixElement::Strong => {
                render_wrapped_html_children(node, output, context, "*");
            }
            MatrixElement::I | MatrixElement::Em => {
                render_wrapped_html_children(node, output, context, "_");
            }
            MatrixElement::S | MatrixElement::Del => {
                render_wrapped_html_children(node, output, context, "~");
            }
            MatrixElement::Code(_) if !context.in_pre => {
                render_wrapped_html_children(node, output, context, "`");
            }
            MatrixElement::Pre => {
                let was_in_pre = context.in_pre;
                context.in_pre = true;

                let mut code = String::new();
                render_html_children(node, &mut code, context);

                context.in_pre = was_in_pre;

                push_newline(output);
                output.push_str("```");
                push_newline(output);
                output.push_str(&trim_newlines(code));
                push_newline(output);
                output.push_str("```");
                push_newline(output);
            }
            MatrixElement::Img(image) => {
                if let Some(alt) = image.alt {
                    output.push_str(&alt);
                } else if let Some(src) = image.src {
                    output.push_str(src.as_str());
                }
            }
            MatrixElement::MatrixReply => {}
            MatrixElement::Other(_) => {
                render_html_children(node, output, context)
            }
            _ => render_html_children(node, output, context),
        },
        NodeData::Document | NodeData::Other => {
            render_html_children(node, output, context)
        }
    }
}

fn render_reply_fallback(node: &NodeRef) -> Option<ReplyFallback> {
    let reply_node = find_first_matrix_reply(node)?;
    let blockquote = find_first_blockquote(&reply_node)?;
    let body = render_reply_fallback_body(&blockquote);

    if body.is_empty() {
        return None;
    }

    Some(ReplyFallback {
        sender: render_reply_fallback_sender(&blockquote),
        body,
    })
}

fn find_first_matrix_reply(node: &NodeRef) -> Option<NodeRef> {
    if is_matrix_element(node, |element| {
        matches!(element, MatrixElement::MatrixReply)
    }) {
        return Some(node.clone());
    }

    node.children()
        .find_map(|child| find_first_matrix_reply(&child))
}

fn find_first_blockquote(node: &NodeRef) -> Option<NodeRef> {
    if is_matrix_element(node, |element| {
        matches!(element, MatrixElement::Blockquote)
    }) {
        return Some(node.clone());
    }

    node.children()
        .find_map(|child| find_first_blockquote(&child))
}

fn is_matrix_element<F>(node: &NodeRef, predicate: F) -> bool
where
    F: FnOnce(MatrixElement) -> bool,
{
    match node.data() {
        NodeData::Element(element) => predicate(element.to_matrix().element),
        _ => false,
    }
}

fn render_reply_fallback_sender(blockquote: &NodeRef) -> Option<String> {
    let mut anchors = Vec::new();

    collect_anchor_text_before_first_br(blockquote, &mut anchors);

    anchors
        .into_iter()
        .filter_map(|anchor| {
            let anchor = collapse_whitespace(&anchor);
            (!anchor.is_empty() && !anchor.eq_ignore_ascii_case("in reply to"))
                .then_some(anchor)
        })
        .next_back()
}

fn collect_anchor_text_before_first_br(
    node: &NodeRef,
    anchors: &mut Vec<String>,
) -> bool {
    match node.data() {
        NodeData::Element(element)
            if matches!(element.to_matrix().element, MatrixElement::Br) =>
        {
            return true;
        }
        NodeData::Element(element)
            if matches!(element.to_matrix().element, MatrixElement::A(_)) =>
        {
            let text = plain_text_children(node);
            if !text.trim().is_empty() {
                anchors.push(text);
            }
        }
        _ => {}
    }

    for child in node.children() {
        if collect_anchor_text_before_first_br(&child, anchors) {
            return true;
        }
    }

    false
}

fn plain_text_children(node: &NodeRef) -> String {
    let mut text = String::new();

    collect_plain_text(node, &mut text);

    text
}

fn collect_plain_text(node: &NodeRef, output: &mut String) {
    match node.data() {
        NodeData::Text(text) => output.push_str(&text.borrow()),
        _ => {
            for child in node.children() {
                collect_plain_text(&child, output);
            }
        }
    }
}

fn render_reply_fallback_body(blockquote: &NodeRef) -> String {
    let mut output = String::new();
    let mut context = HtmlRenderContext::default();
    let mut after_intro = false;

    for child in blockquote.children() {
        render_after_first_br(
            &child,
            &mut output,
            &mut context,
            &mut after_intro,
        );
    }

    trim_newlines(output)
}

fn render_after_first_br(
    node: &NodeRef,
    output: &mut String,
    context: &mut HtmlRenderContext,
    after_intro: &mut bool,
) {
    if !*after_intro {
        if is_matrix_element(node, |element| {
            matches!(element, MatrixElement::Br)
        }) {
            *after_intro = true;
            return;
        }

        for child in node.children() {
            render_after_first_br(&child, output, context, after_intro);
        }

        return;
    }

    render_html_node(node, output, context);
}

fn render_html_children(
    node: &NodeRef,
    output: &mut String,
    context: &mut HtmlRenderContext,
) {
    for child in node.children() {
        render_html_node(&child, output, context);
    }
}

fn render_wrapped_html_children(
    node: &NodeRef,
    output: &mut String,
    context: &mut HtmlRenderContext,
    wrapper: &str,
) {
    output.push_str(wrapper);
    render_html_children(node, output, context);
    output.push_str(wrapper);
}

fn list_marker(context: &mut HtmlRenderContext) -> String {
    match context.lists.last_mut() {
        Some(ListContext::Ordered { next }) => {
            let marker = format!("{}. ", next);
            *next += 1;
            marker
        }
        _ => "- ".to_owned(),
    }
}

fn anchor_uri_to_string(uri: &AnchorUri) -> Option<String> {
    match uri {
        AnchorUri::Matrix(uri) => Some(uri.to_string()),
        AnchorUri::MatrixTo(uri) => Some(uri.to_string()),
        AnchorUri::Other(uri) => Some(uri.to_string()),
        _ => None,
    }
}

fn reply_quote_lines(body: &str) -> impl Iterator<Item = String> + '_ {
    body.lines().map(|line| {
        let line = line.trim();

        if line.is_empty() {
            ">".to_owned()
        } else {
            format!("> {}", line)
        }
    })
}

fn collapse_whitespace(body: &str) -> String {
    body.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn prefix_lines(text: String, prefix: &str) -> String {
    text.lines()
        .map(|line| {
            if line.is_empty() {
                prefix.trim_end().to_owned()
            } else {
                format!("{prefix}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn push_newline(output: &mut String) {
    if !output.ends_with('\n') {
        output.push('\n');
    }
}

fn trim_newlines(mut output: String) -> String {
    while output.starts_with('\n') {
        output.remove(0);
    }

    while output.ends_with('\n') {
        output.pop();
    }

    output
}

/// Trait for message event types that contain an optional formatted body.
trait HasFormattedBody {
    fn body(&self) -> &str;
    fn formatted_body(&self) -> Option<&str>;
}

// Repeating this for each event type would get boring fast so lets use a simple
// macro to implement the trait for a struct that has a `body` and
// `formatted_body` field
macro_rules! has_formatted_body {
    ($content: ident) => {
        impl HasFormattedBody for $content {
            #[inline]
            fn body(&self) -> &str {
                &self.body
            }

            #[inline]
            fn formatted_body(&self) -> Option<&str> {
                self.formatted
                    .as_ref()
                    .filter(|f| f.format == MessageFormat::Html)
                    .map(|f| f.body.as_ref())
            }
        }
    };
}

has_formatted_body!(EmoteMessageEventContent);
has_formatted_body!(NoticeMessageEventContent);
has_formatted_body!(TextMessageEventContent);

/// This trait is implemented for message types that can contain either an URL
/// or an encrypted file. One of these _must_ be present.
pub trait HasUrlOrFile {
    fn body(&self) -> &str;

    #[inline]
    fn resolve_url(&self) -> &MxcUri {
        match self.source() {
            MediaSource::Plain(s) => s,
            MediaSource::Encrypted(e) => &e.url,
        }
    }

    fn encrypted_file(&self) -> Option<&EncryptedFile>;

    fn source(&self) -> &MediaSource;
}

// Same as above: a simple macro to implement the trait for structs with `url`
// and `file` fields.
macro_rules! has_url_or_file {
    ($content: ident) => {
        impl HasUrlOrFile for $content {
            fn body(&self) -> &str {
                &self.body
            }

            fn source(&self) -> &MediaSource {
                &self.source
            }

            fn encrypted_file(&self) -> Option<&EncryptedFile> {
                match &self.source {
                    MediaSource::Encrypted(e) => Some(&e),
                    _ => None,
                }
            }
        }
    };
}

has_url_or_file!(AudioMessageEventContent);
has_url_or_file!(FileMessageEventContent);
has_url_or_file!(ImageMessageEventContent);
has_url_or_file!(VideoMessageEventContent);

/// Rendering implementation for membership events (joins, leaves, bans, profile
/// changes, etc).
pub fn render_membership(
    event: &OriginalSyncStateEvent<RoomMemberEventContent>,
    sender: &WeechatRoomMember,
    target: &WeechatRoomMember,
) -> RenderedLine {
    use MembershipChange::*;
    let change_op = event.membership_change();

    let operation = match change_op {
        None => "did nothing",
        Error => "caused an error", // must never happen
        Joined => "has joined the room",
        Left => "has left the room",
        Banned => "was banned by",
        Unbanned => "was unbanned by",
        Kicked => "was kicked from the room by",
        Invited => "was invited to the room by",
        KickedAndBanned => "was kicked and banned by",
        InvitationRejected => "rejected the invitation",
        InvitationRevoked => "had the invitation revoked by",
        ProfileChanged { .. } => "_",
        _ => "performed an unimplemented operation",
    };

    fn formatted_name(member: &WeechatRoomMember) -> String {
        match member.display_name() {
            Some(display_name) => {
                format!(
                    "{name} {color_delim}({color_reset}{user_id}{color_delim}){color_reset}",
                    name = display_name,
                    user_id = member.user_id(),
                    color_delim = Weechat::color("chat_delimiters"),
                    color_reset = Weechat::color("reset"))
            }

            Option::None => member.user_id().to_string(),
        }
    }

    let (prefix, color_action) = match change_op {
        Joined => (Prefix::Join, "green"),
        Banned | ProfileChanged { .. } | Invited => {
            (Prefix::Network, "magenta")
        }
        _ => (Prefix::Quit, "red"),
    };

    let color_action = Weechat::color(color_action);
    let color_reset = Weechat::color("reset");

    let operation = format!(
        "{color_action}{op}{color_reset}",
        color_action = color_action,
        op = operation,
        color_reset = color_reset
    );

    let target_name = format!(
        "{color_user}{target_name}{color_reset}",
        target_name = formatted_name(target),
        color_user = Weechat::color("reset"), // TODO
        color_reset = Weechat::color("reset")
    );

    let sender_name = format!(
        "{color_user}{sender_name}{color_reset}",
        sender_name = formatted_name(sender),
        color_user = Weechat::color("reset"), // TODO
        color_reset = Weechat::color("reset")
    );

    let message = match change_op {
        ProfileChanged {
            displayname_change,
            avatar_url_change,
        } => {
            let new_display_name = &event.content.displayname;

            // TODO: Should we display the new avatar URL?
            // let new_avatar = self.content.avatar_url.as_ref();

            match (displayname_change.is_some(), avatar_url_change.is_some()) {
                (false, true) =>
                    format!(
                        "{prefix}{target} {color_action}changed their avatar{color_reset}",
                        prefix = Weechat::prefix(prefix),
                        target = target_name,
                        color_action = color_action,
                        color_reset = color_reset
                        ),
                (true, false) => {
                    match new_display_name {
                        Some(name) => format!(
                            "{prefix}{target} {color_action}changed their display name to{color_reset} {new}",
                            prefix = Weechat::prefix(prefix),
                            target = event.prev_content().as_ref().and_then(|p| p.displayname.clone()).unwrap_or(target_name),
                            new = name,
                            color_action = color_action,
                            color_reset = color_reset
                            ),
                        Option::None => format!(
                            "{prefix}{target} {color_action}removed their display name{color_reset}",
                            prefix = Weechat::prefix(prefix),
                            target = target_name,
                            color_action = color_action,
                            color_reset = color_reset
                            ),
                    }
                }
                (true, true) =>
                    match new_display_name {
                        Some(name) => format!(
                            "{prefix}{target} {color_action}changed their avatar \
                            and changed their display name to{color_reset} {new}",
                            prefix = Weechat::prefix(prefix),
                            target = target_name,
                            new = name,
                            color_action = color_action,
                            color_reset = color_reset
                            ),
                        Option::None => format!(
                            "{prefix}{target} {color_action}changed their \
                            avatar and removed display name{color_reset}",
                            prefix = Weechat::prefix(prefix),
                            target = target_name,
                            color_action = color_action,
                            color_reset = color_reset
                            ),
                    }
                (false, false) =>
                    "Cannot happen: got profile changed but nothing really changed".to_string()
            }
        }
        Banned | Unbanned | Kicked | Invited | InvitationRevoked
        | KickedAndBanned => format!(
            "{prefix}{target} {op} {sender}",
            prefix = Weechat::prefix(prefix),
            target = target_name,
            op = operation,
            sender = sender_name
        ),
        _ => format!(
            "{prefix}{target} {op}",
            prefix = Weechat::prefix(prefix),
            target = target_name,
            op = operation
        ),
    };

    RenderedLine {
        tags: vec!["matrix_membership".to_owned()],
        message,
    }
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::{
        events::room::{EncryptedFileInit, JsonWebKeyInit},
        serde::Base64,
        OwnedMxcUri,
    };

    use super::*;

    #[test]
    fn test_mxc_to_http() {
        let homeserver = url::Url::parse("https://matrix.org").unwrap();
        let mxc_url = OwnedMxcUri::from("mxc://matrix.org/some-media-id");
        let expected =
            "https://matrix.org/_matrix/media/r0/download/matrix.org/some-media-id";
        assert_eq!(expected, mxc_to_http(&mxc_url, &homeserver).unwrap());
    }

    #[test]
    fn test_emxc_to_http() {
        use std::collections::BTreeMap;

        let homeserver = url::Url::parse("https://matrix.org").unwrap();
        let mxc_url = OwnedMxcUri::from("mxc://matrix.org/some-media-id");
        let mut hashes: BTreeMap<String, Base64> = BTreeMap::new();
        hashes.insert("sha256".to_string(), Base64::parse("aGFzaA").unwrap());
        let encrypt_info = EncryptedFileInit {
            key: JsonWebKeyInit {
                k: Base64::parse("dGVzdA").unwrap(),
                kty: "oct".to_string(),
                key_ops: vec![],
                ext: true,
                alg: "A256CTR".to_string(),
            }
            .into(),
            iv: Base64::parse("aXY").unwrap(),
            v: "v2".to_string(),
            url: OwnedMxcUri::from("mxc://some-url"),
            hashes,
        }
        .into();
        let expected =
            "emxc://matrix.org:443/_matrix/media/r0/download/matrix.org/some-media-id?key=dGVzdA&hash=aGFzaA&iv=aXY";
        assert_eq!(
            expected,
            mxc_to_emxc(&mxc_url, &homeserver, &encrypt_info).unwrap()
        );
    }

    #[test]
    fn test_plain_media_download_command() {
        let source = MediaSource::Plain(OwnedMxcUri::from(
            "mxc://matrix.org/some-media-id",
        ));

        assert_eq!(
            Some(
                "/matrix media download mxc://matrix.org/some-media-id [file]"
                    .to_owned()
            ),
            media_download_command(&source)
        );
    }

    #[test]
    fn test_plain_media_renders_authenticated_download_message() {
        let content = ImageMessageEventContent::plain(
            "image.png".to_owned(),
            OwnedMxcUri::from("mxc://matrix.org/some-media-id"),
        );

        assert_eq!(
            Some(
                "attached `image.png`, authenticated download: /matrix media \
                 download mxc://matrix.org/some-media-id [file]"
                    .to_owned()
            ),
            media_download_message(&content)
        );
    }

    #[test]
    fn formatted_body_plain_text_decodes_matrix_html() {
        let body = "<p>Hello <b>world</b> &amp; \
            <a href=\"https://example.org\">link</a></p>\
            <ul><li>one</li><li>two</li></ul>";

        assert_eq!(
            "Hello *world* & link <https://example.org>\n- one\n- two",
            formatted_body_to_plain_text(body)
        );
    }

    #[test]
    fn test_encrypted_media_has_no_plain_download_command() {
        use std::collections::BTreeMap;

        let mut hashes: BTreeMap<String, Base64> = BTreeMap::new();
        hashes.insert("sha256".to_string(), Base64::parse("aGFzaA").unwrap());
        let encrypt_info = EncryptedFileInit {
            key: JsonWebKeyInit {
                k: Base64::parse("dGVzdA").unwrap(),
                kty: "oct".to_string(),
                key_ops: vec![],
                ext: true,
                alg: "A256CTR".to_string(),
            }
            .into(),
            iv: Base64::parse("aXY").unwrap(),
            v: "v2".to_string(),
            url: OwnedMxcUri::from("mxc://some-url"),
            hashes,
        }
        .into();
        let source = MediaSource::Encrypted(Box::new(encrypt_info));

        assert_eq!(None, media_download_command(&source));
    }

    #[test]
    fn reply_context_names_known_sender() {
        let event_id =
            matrix_sdk::ruma::owned_event_id!("$replyevent:example.org");
        let rendered = RenderedEvent {
            message_timestamp: 0,
            prefix: "alice\t".to_owned(),
            content: RenderedContent::new(vec![RenderedLine {
                tags: vec!["matrix_text".to_owned()],
                message: "reply body".to_owned(),
            }]),
        }
        .add_reply_context(&event_id, Some("Alice"));

        assert_eq!(2, rendered.content.lines.len());
        assert_eq!("Reply to Alice", rendered.content.lines[0].message);
        assert!(rendered.content.lines[0]
            .tags
            .contains(&"matrix_reply".to_owned()));
        assert_eq!("reply body", rendered.content.lines[1].message);
    }

    #[test]
    fn reply_context_falls_back_to_event_id() {
        let event_id =
            matrix_sdk::ruma::owned_event_id!("$replyevent:example.org");
        let rendered = RenderedEvent {
            message_timestamp: 0,
            prefix: "alice\t".to_owned(),
            content: RenderedContent::new(vec![RenderedLine {
                tags: vec!["matrix_text".to_owned()],
                message: "reply body".to_owned(),
            }]),
        }
        .add_reply_context(&event_id, None);

        assert_eq!(
            "Reply to $replyevent:example.org",
            rendered.content.lines[0].message
        );
        assert!(rendered.content.lines[0]
            .tags
            .contains(&"matrix_reply".to_owned()));
        assert_eq!("reply body", rendered.content.lines[1].message);
    }

    #[test]
    fn reply_context_uses_formatted_reply_fallback_quote() {
        let event_id =
            matrix_sdk::ruma::owned_event_id!("$replyevent:example.org");
        let body = "\
            <mx-reply>\
                <blockquote>\
                    <a href=\"https://matrix.to/#/!room:example.org/$replyevent:example.org\">In reply to</a> \
                    <a href=\"https://matrix.to/#/@alice:example.org\">@alice:example.org</a>\
                    <br>\
                    <p>Previous <strong>message</strong> text</p>\
                </blockquote>\
            </mx-reply>\
            <p>new message</p>";
        let content = TextMessageEventContent::html("fallback", body);
        let rendered = RenderedEvent {
            message_timestamp: 0,
            prefix: "bob\t".to_owned(),
            content: content.render(&()),
        }
        .add_reply_context(&event_id, Some("Alice"));

        assert_eq!(3, rendered.content.lines.len());
        assert_eq!("Reply to Alice:", rendered.content.lines[0].message);
        assert_eq!(
            "> Previous *message* text",
            rendered.content.lines[1].message
        );
        assert_eq!("new message", rendered.content.lines[2].message);
    }

    #[test]
    fn reply_context_uses_reply_fallback_sender_when_local_sender_is_unknown() {
        let event_id =
            matrix_sdk::ruma::owned_event_id!("$replyevent:example.org");
        let body = "\
            <mx-reply>\
                <blockquote>\
                    <a href=\"https://matrix.to/#/!room:example.org/$replyevent:example.org\">In reply to</a> \
                    <a href=\"https://matrix.to/#/@alice:example.org\">@alice:example.org</a>\
                    <br>\
                    Previous message\
                </blockquote>\
            </mx-reply>\
            <p>new message</p>";
        let content = TextMessageEventContent::html("fallback", body);
        let rendered = RenderedEvent {
            message_timestamp: 0,
            prefix: "bob\t".to_owned(),
            content: content.render(&()),
        }
        .add_reply_context(&event_id, None);

        assert_eq!(
            "Reply to @alice:example.org:",
            rendered.content.lines[0].message
        );
        assert_eq!("> Previous message", rendered.content.lines[1].message);
    }

    #[test]
    fn reply_context_quotes_multiline_reply_fallback() {
        let event_id =
            matrix_sdk::ruma::owned_event_id!("$replyevent:example.org");
        let body = "\
            <mx-reply>\
                <blockquote>\
                    <a href=\"https://matrix.to/#/!room:example.org/$replyevent:example.org\">In reply to</a> \
                    <a href=\"https://matrix.to/#/@alice:example.org\">@alice:example.org</a>\
                    <br>\
                    <p>line one</p><p>line two</p>\
                </blockquote>\
            </mx-reply>\
            <p>new message</p>";
        let content = TextMessageEventContent::html("fallback", body);
        let rendered = RenderedEvent {
            message_timestamp: 0,
            prefix: "bob\t".to_owned(),
            content: content.render(&()),
        }
        .add_reply_context(&event_id, Some("Alice"));

        let messages = rendered
            .content
            .lines
            .iter()
            .map(|line| line.message.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            vec!["Reply to Alice:", "> line one", "> line two", "new message"],
            messages
        );
    }

    #[test]
    fn formatted_body_plain_text_keeps_block_structure() {
        let body = "<blockquote><p>quoted</p><p>text</p></blockquote>\
            <ol start=\"3\"><li>three</li><li>four</li></ol>";

        assert_eq!(
            "> quoted\n> text\n3. three\n4. four",
            formatted_body_to_plain_text(body)
        );
    }

    #[test]
    fn formatted_body_plain_text_marks_code_and_emphasis() {
        let body = "<p><em>try</em> <code>cargo test</code></p>\
            <pre><code class=\"language-rust\">fn main() {}</code></pre>";

        assert_eq!(
            "_try_ `cargo test`\n```\nfn main() {}\n```",
            formatted_body_to_plain_text(body)
        );
    }

    #[test]
    fn formatted_body_plain_text_removes_reply_fallback() {
        let body = "<mx-reply><blockquote>old reply</blockquote></mx-reply>\
            <p>new message</p>";

        assert_eq!("new message", formatted_body_to_plain_text(body));
    }

    #[test]
    fn text_render_uses_formatted_body_when_available() {
        let content =
            TextMessageEventContent::html("fallback", "<p>Hello<br>world</p>");
        let rendered = content.render(&());

        assert_eq!(rendered.lines.len(), 2);
        assert_eq!(rendered.lines[0].message, "Hello");
        assert_eq!(rendered.lines[1].message, "world");
    }

    #[test]
    fn text_render_keeps_plain_body_plain() {
        let content = TextMessageEventContent::plain("literal <b> text");
        let rendered = content.render(&());

        assert_eq!(rendered.lines.len(), 1);
        assert_eq!(rendered.lines[0].message, "literal <b> text");
    }
}

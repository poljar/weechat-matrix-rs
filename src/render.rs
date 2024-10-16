use url::Url;

use matrix_sdk::{
    encryption::verification::{
        SasVerification, Verification, VerificationRequest,
    },
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
                    LocationMessageEventContent, NoticeMessageEventContent,
                    RedactedRoomMessageEventContent,
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

use weechat::{Prefix, Weechat};

use crate::{room::WeechatRoomMember, utils::ToTag};

/// The rendered version of an event.
pub struct RenderedEvent {
    /// The UNIX timestamp of the event.
    pub message_timestamp: i64,
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
            &sender.user_id(),
            &sender.nick(),
            sender.color(),
        );

        for line in &mut content.lines {
            line.tags = tags.clone();
        }

        RenderedEvent {
            prefix,
            message_timestamp: timestamp,
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
        let uuid_tag = format!("matrix_echo_{}", uuid.to_string());

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
        let lines = self
            .body
            .lines()
            .map(|l| RenderedLine {
                message: l.to_owned(),
                tags: self.tags(),
            })
            .collect();
        // TODO: parse and render using the formatted body.
        RenderedContent { lines }
    }
}

impl Render for EmoteMessageEventContent {
    const TAGS: &'static [&'static str] = &["matrix_emote"];
    type RenderContext = WeechatRoomMember;

    fn prefix(&self, _: &WeechatRoomMember) -> String {
        Weechat::prefix(Prefix::Action)
    }

    fn render(&self, sender: &Self::RenderContext) -> RenderedContent {
        // TODO: parse and render using the formatted body.
        // TODO: handle multiple lines in the body.
        let message = format!("{} {}", sender.nick(), self.body);

        let line = RenderedLine {
            message,
            tags: self.tags(),
        };

        RenderedContent { lines: vec![line] }
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

        RenderedContent { lines: vec![line] }
    }
}

impl Render for NoticeMessageEventContent {
    const TAGS: &'static [&'static str] = &["matrix_notice"];
    type RenderContext = WeechatRoomMember;

    fn prefix(&self, _: &WeechatRoomMember) -> String {
        Weechat::prefix(Prefix::Network)
    }

    fn render(&self, sender: &Self::RenderContext) -> RenderedContent {
        // TODO: parse and render using the formatted body.
        let message = format!(
            "{color_notice}Notice\
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

        RenderedContent { lines: vec![line] }
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

        RenderedContent { lines: vec![line] }
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

impl<C: HasUrlOrFile> Render for C {
    type RenderContext = Url;
    const TAGS: &'static [&'static str] = &["matrix_media"];

    fn render(&self, homeserver: &Self::RenderContext) -> RenderedContent {
        // Convert MXC to HTTP(s) or EMXC, but fallback to MXC if unable to.
        let mxc_url = match self.encrypted_file() {
            Some(encrypted_file) => {
                mxc_to_emxc(self.resolve_url(), homeserver, &encrypted_file)
            }
            None => mxc_to_http(self.resolve_url(), homeserver),
        }
        .unwrap_or_else(|_| self.resolve_url().to_string());

        let message = format!(
            "{color_delimiter}<{color_reset}{}{color_delimiter}>\
                [{color_reset}{}{color_delimiter}]{color_reset}",
            self.body(),
            mxc_url,
            color_delimiter = Weechat::color("color_delimiter"),
            color_reset = Weechat::color("reset")
        );

        let line = RenderedLine {
            message,
            tags: self.tags(),
        };

        RenderedContent { lines: vec![line] }
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

        RenderedContent { lines: vec![line] }
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

        RenderedContent { lines: vec![line] }
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
                                    return RenderedContent {
                                        lines: vec![],
                                    }
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
                                    "{} has started an interactive emoji verifiaction \
                                        with you, accept with TODO",
                                    sas.other_device().user_id()
                                )
                            } else {
                                // We auto accept emoji verifications that start
                                // from a verification request, so don't print
                                // anything.
                                return RenderedContent {
                                    lines: vec![],
                                }
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

                RenderedContent {
                    lines: vec![RenderedLine {
                        message,
                        tags: self.tags(),
                    }],
                }
            }
        }
    };
}

render_start_content!(KeyVerificationStartEventContent);
render_start_content!(ToDeviceKeyVerificationStartEventContent);

pub enum VerificationContext {
    Room(WeechatRoomMember, WeechatRoomMember),
    ToDevice(VerificationRequest),
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
                    VerificationContext::Room(own_member, sender) => {
                        if own_member == sender {
                            "You sent a verification request".to_string()
                        } else {
                            format!(
                                "{} has sent a verification request",
                                sender.nick_colored()
                            )
                        }
                    }
                    VerificationContext::ToDevice(_) => {
                        format!("You have requested this device to be verified")
                    }
                };

                RenderedContent {
                    lines: vec![RenderedLine {
                        message,
                        tags: self.tags(),
                    }],
                }
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

                RenderedContent {
                    lines: vec![RenderedLine {
                        message,
                        tags: self.tags(),
                    }],
                }
            }
        }
    };
}

render_ready_content!(KeyVerificationReadyEventContent);
render_ready_content!(ToDeviceKeyVerificationReadyEventContent);

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
                    (
                        "Do the emojis match?".to_string(),
                        format!("{:?}", sas.emoji()),
                    )
                } else {
                    (
                        "Do the decimals match".to_string(),
                        format!("{:?}", sas.decimals()),
                    )
                };

                RenderedContent {
                    lines: vec![
                        RenderedLine {
                            message,
                            tags: self.tags(),
                        },
                        RenderedLine {
                            message: short_auth_string,
                            tags: self.tags(),
                        },
                    ],
                }
            }
        }
    };
}

render_key_content!(KeyVerificationKeyEventContent);
render_key_content!(ToDeviceKeyVerificationKeyEventContent);

/// Trait for message event types that contain an optional formatted body.
/// `resolve_body` will return the formatted body if present, else fallback to
/// the regular body.
trait HasFormattedBody {
    fn body(&self) -> &str;
    fn formatted_body(&self) -> Option<&str>;
    #[inline]
    fn resolve_body(&self) -> &str {
        self.formatted_body().unwrap_or_else(|| self.body())
    }
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
                self.formatted.as_ref().map(|f| f.body.as_ref())
            }
        }
    };
}

/// This trait is implemented for message types that can contain either an URL
/// or an encrypted file. One of these _must_ be present.
pub trait HasUrlOrFile {
    fn url(&self) -> Option<&MxcUri>;

    fn body(&self) -> &str;

    #[inline]
    fn resolve_url(&self) -> &MxcUri {
        match self.source() {
            MediaSource::Plain(s) => &s,
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

            #[inline]
            fn url(&self) -> Option<&MxcUri> {
                match &self.source {
                    MediaSource::Plain(url) => Some(url),
                    _ => None,
                }
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

// this actually implements the trait for different event types
has_formatted_body!(EmoteMessageEventContent);
has_formatted_body!(NoticeMessageEventContent);
has_formatted_body!(TextMessageEventContent);

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
) -> String {
    const _TAGS: &[&str] = &["matrix_membership"];
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

    // TODO: we should return the tags as well.
    match change_op {
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
                            target = event.prev_content().as_ref().map(|p| p.displayname.clone()).flatten().unwrap_or(target_name),
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
    }
}

#[cfg(test)]
mod tests {
    use std::convert::TryFrom;

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
        let mxc_url =
            OwnedMxcUri::try_from("mxc://matrix.org/some-media-id").unwrap();
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
}

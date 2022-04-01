use qrcode::{render::unicode::Dense1x2, QrCode};
use url::Url;

use matrix_sdk::{
    encryption::verification::{CancelInfo, Verification, VerificationRequest},
    ruma::{
        events::{
            key::verification::{
                cancel::CancelCode,
                ready::{
                    KeyVerificationReadyEventContent,
                    ToDeviceKeyVerificationReadyEventContent,
                },
            },
            room::{
                encrypted::RoomEncryptedEventContent,
                member::{MembershipChange, RoomMemberEventContent},
                message::{
                    AudioMessageEventContent, EmoteMessageEventContent,
                    FileMessageEventContent, ImageMessageEventContent,
                    LocationMessageEventContent, NoticeMessageEventContent,
                    RedactedRoomMessageEventContent,
                    ServerNoticeMessageEventContent, TextMessageEventContent,
                    VideoMessageEventContent,
                },
                EncryptedFile, MediaSource,
            },
            RedactedSyncMessageLikeEvent, SyncStateEvent,
        },
        uint, EventId, MilliSecondsSinceUnixEpoch, MxcUri, TransactionId,
        UserId,
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

#[derive(Debug, Default)]
pub struct RenderedContent {
    /// The collection of lines that the event has.
    pub lines: Vec<RenderedLine>,
}

/// Trait allowing events to be rendered for Weechat.
pub trait Render {
    /// The event specific tags that should be attached to the rendered event.
    const TAGS: &'static [&'static str] = &[];

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
        timestamp: &MilliSecondsSinceUnixEpoch,
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
        // TODO parse and render using the formattted body.
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
        // TODO parse and render using the formattted body.
        // TODO handle multiple lines in the body.
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
        // TODO parse and render using the formattted body.
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
    homeserver: &Url,
    encrypted: &EncryptedFile,
) -> Result<String, Box<dyn std::error::Error>> {
    let url = url::Url::parse(encrypted.url.as_str())?;

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
            Some(encrypted_file) => mxc_to_emxc(homeserver, &encrypted_file),
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
            // TODO add tags that allow us decrypt the event at a later point in
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
        // TODO add the redaction reason.
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

macro_rules! render_ready_content {
    ($type: ident) => {
        impl Render for $type {
            const TAGS: &'static [&'static str] = &[];

            type RenderContext = Verification;

            fn prefix(&self, _: &WeechatRoomMember) -> String {
                Weechat::prefix(Prefix::Network)
            }

            fn render(
                &self,
                verification: &Self::RenderContext,
            ) -> RenderedContent {
                // TODO print out a help, how to transition into emoji
                // verification or if we're waiting for a QR code to be scanned.
                let message = if verification.we_started() {
                    format!(
                        "{} has answered the verification request",
                        verification.other_user_id(),
                    )
                } else {
                    "You answered the verification request".to_string()
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

pub enum CancelVerification {
    Request(VerificationRequest),
    Verification(Verification),
}

impl CancelVerification {
    fn is_self_verification(&self) -> bool {
        match self {
            CancelVerification::Request(r) => r.is_self_verification(),
            CancelVerification::Verification(v) => v.is_self_verification(),
        }
    }

    fn other_user_id(&self) -> &UserId {
        match self {
            CancelVerification::Request(r) => r.other_user_id(),
            CancelVerification::Verification(v) => v.other_user_id(),
        }
    }
}

impl From<VerificationRequest> for CancelVerification {
    fn from(v: VerificationRequest) -> Self {
        Self::Request(v)
    }
}

impl From<Verification> for CancelVerification {
    fn from(v: Verification) -> Self {
        Self::Verification(v)
    }
}

pub enum CancelContext {
    ToDevice(CancelVerification),
}

impl CancelContext {
    fn verification(&self) -> &CancelVerification {
        match self {
            CancelContext::ToDevice(v) => &v,
        }
    }

    fn other_users_nick(&self) -> String {
        match self {
            CancelContext::ToDevice(v) => v.other_user_id().to_string(),
        }
    }
}

impl Render for CancelInfo {
    const TAGS: &'static [&'static str] = &[];

    type RenderContext = CancelContext;

    fn prefix(&self, _: &WeechatRoomMember) -> String {
        Weechat::prefix(Prefix::Network)
    }

    fn render(&self, context: &Self::RenderContext) -> RenderedContent {
        let verification = context.verification();

        let message =
            if self.cancelled_by_us() || verification.is_self_verification() {
                if self.cancel_code() == &CancelCode::User {
                    "You cancelled the verification flow".to_owned()
                } else {
                    format!(
                        "The verification flow has been cancelled: {}",
                        self.reason(),
                    )
                }
            } else {
                format!(
                    "{} has cancelled the verification flow: {}",
                    context.other_users_nick(),
                    self.reason(),
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

impl Render for QrCode {
    const TAGS: &'static [&'static str] = &[];

    type RenderContext = ();

    fn render(&self, _: &Self::RenderContext) -> RenderedContent {
        let qr_code = self
            .render::<Dense1x2>()
            .light_color(Dense1x2::Dark)
            .dark_color(Dense1x2::Light)
            .build();

        RenderedContent {
            lines: vec![
                RenderedLine {
                    message: qr_code,
                    tags: self.tags(),
                },
                RenderedLine {
                    message:
                        "Scan the QR code on your other device or switch to \
                         emoji verification using '/verification use-emoji'"
                            .to_string(),
                    tags: self.tags(),
                },
            ],
        }
    }
}

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
    fn resolve_url(&self) -> &MxcUri {
        // the file is either encrypted or not encrypted so either `url` or
        // `file` must exist and unwrapping will never panic
        self.encrypted_file()
            .map(|f| &*f.url)
            .or_else(|| self.url())
            .unwrap()
    }
    fn encrypted_file(&self) -> Option<&EncryptedFile>;
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
                if let MediaSource::Plain(u) = &self.source {
                    Some(u)
                } else {
                    None
                }
            }

            fn encrypted_file(&self) -> Option<&EncryptedFile> {
                if let MediaSource::Encrypted(e) = &self.source {
                    Some(e)
                } else {
                    None
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
    event: &SyncStateEvent<RoomMemberEventContent>,
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

    // TODO we should return the tags as well.
    match change_op {
        ProfileChanged {
            displayname_changed,
            avatar_url_changed,
        } => {
            let new_display_name = &event.content.displayname;

            // TODO: Should we display the new avatar URL?
            // let new_avatar = self.content.avatar_url.as_ref();

            match (displayname_changed, avatar_url_changed) {
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
                            target = event.unsigned.prev_content.as_ref().map(|p| p.displayname.clone()).flatten().unwrap_or(target_name),
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
    use matrix_sdk::ruma::{
        events::room::{EncryptedFileInit, JsonWebKeyInit},
        MxcUri,
    };

    use crate::render::{mxc_to_emxc, mxc_to_http};

    #[test]
    fn test_mxc_to_http() {
        let homeserver = url::Url::parse("https://matrix.org").unwrap();
        let mxc_url = "mxc://matrix.org/some-media-id";
        let expected =
            "https://matrix.org/_matrix/media/r0/download/matrix.org/some-media-id";
        assert_eq!(expected, mxc_to_http(&mxc_url, &homeserver).unwrap());
    }

    #[test]
    fn test_emxc_to_http() {
        use std::collections::BTreeMap;

        let homeserver = url::Url::parse("https://matrix.org").unwrap();
        let mxc_url = "mxc://matrix.org/some-media-id";
        let mut hashes: BTreeMap<String, String> = BTreeMap::new();
        hashes.insert("sha256".to_string(), "some-sha256".to_string());
        let encrypt_info = EncryptedFileInit {
            key: JsonWebKeyInit {
                k: "some-secret-key".to_string(),
                kty: "oct".to_string(),
                key_ops: vec![],
                ext: true,
                alg: "A256CTR".to_string(),
            }
            .into(),
            iv: "some-test-iv".to_string(),
            v: "v2".to_string(),
            url: MxcUri::from("mxc://some-url"),
            hashes,
        }
        .into();
        let expected =
            "emxc://matrix.org:443/_matrix/media/r0/download/matrix.org/some-media-id?key=some-secret-key&hash=some-sha256&iv=some-test-iv";
        assert_eq!(
            expected,
            mxc_to_emxc(&mxc_url, &homeserver, &encrypt_info).unwrap()
        );
    }
}

use std::time::SystemTime;
use url::Url;

use matrix_sdk::{
    events::{
        room::{
            encrypted::EncryptedEventContent,
            member::{MemberEventContent, MembershipChange},
            message::{
                AudioMessageEventContent, EmoteMessageEventContent,
                FileMessageEventContent, ImageMessageEventContent,
                LocationMessageEventContent, NoticeMessageEventContent,
                RedactedMessageEventContent, ServerNoticeMessageEventContent,
                TextMessageEventContent, VideoMessageEventContent,
            },
        },
        RedactedSyncMessageEvent, SyncStateEvent,
    },
    identifiers::{EventId, UserId},
    uuid::Uuid,
};

use weechat::{Prefix, Weechat};

use crate::room::WeechatRoomMember;

/// The rendered version of an event.
#[allow(dead_code)]
pub struct RenderedEvent {
    /// The UNIX timestamp of the event.
    pub message_timestamp: u64,
    pub prefix: String,
    pub content: RenderedContent,
}

pub struct RenderedLine {
    /// The tags of the line.
    pub tags: Vec<String>,
    /// The message of the line.
    pub message: String,
}

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

    fn event_tags(&self, event_id: &EventId, sender: &UserId) -> Vec<String> {
        let mut tags = self.tags();
        let event_tag = format!("matrix_id_{}", event_id.as_str());
        let sender_tag = format!("matrix_sender_{}", sender.as_str());
        tags.push(event_tag);
        tags.push(sender_tag);

        tags
    }

    fn prefix(&self, sender: &WeechatRoomMember) -> String {
        format!(
            "{}{}{}",
            Weechat::color(&sender.color),
            sender.nick.borrow(),
            Weechat::color("reset")
        )
    }

    /// Render the event.
    fn render_with_prefix(
        &self,
        timestamp: &SystemTime,
        event_id: &EventId,
        sender: &WeechatRoomMember,
        context: &Self::RenderContext,
    ) -> RenderedEvent {
        let prefix = self.prefix(sender);
        let mut content = self.render(context);
        let timestamp: u64 = timestamp
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let tags = self.event_tags(event_id, &sender.user_id);

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
        uuid: Uuid,
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
        uuid: Uuid,
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
        Weechat::prefix(Prefix::Action).to_owned()
    }

    fn render(&self, sender: &Self::RenderContext) -> RenderedContent {
        // TODO parse and render using the formattted body.
        // TODO handle multiple lines in the body.
        let message = format!("{} {}", sender.nick.borrow(), self.body);

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
        Weechat::prefix(Prefix::Action).to_owned()
    }

    fn render(&self, sender: &Self::RenderContext) -> RenderedContent {
        let message = format!(
            "{} has shared a location: {color_delimiter}<{color_reset}{}{color_delimiter}>\
            [{color_reset}{}{color_delimiter}]{color_reset}",
            sender.nick.borrow(),
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
        Weechat::prefix(Prefix::Action).to_owned()
    }

    fn render(&self, sender: &Self::RenderContext) -> RenderedContent {
        // TODO parse and render using the formattted body.
        let message = format!(
            "{prefix}{color_notice}Notice\
            {color_delim}({color_reset}{}{color_delim}){color_reset}: {}",
            sender.nick.borrow(),
            self.body,
            prefix = Weechat::prefix(Prefix::Network),
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
        Weechat::prefix(Prefix::Action).to_owned()
    }

    fn render(&self, sender: &Self::RenderContext) -> RenderedContent {
        let message = format!(
            "{prefix}{color_notice}Server notice\
            {color_delim}({color_reset}{}{color_delim}){color_reset}: {}",
            sender.nick.borrow(),
            self.body,
            prefix = Weechat::prefix(Prefix::Network),
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

impl<C: HasUrlOrFile> Render for C {
    type RenderContext = Url;
    const TAGS: &'static [&'static str] = &["matrix_media"];

    fn render(&self, _homeserver: &Self::RenderContext) -> RenderedContent {
        let message = format!(
            "{color_delimiter}<{color_reset}{}{color_delimiter}>\
                [{color_reset}{}{color_delimiter}]{color_reset}",
            self.body(),
            // FIXME this isn't right, the MXID -> URL transformation depends on
            // your homeserver URL.
            self.resolve_url(),
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

impl Render for EncryptedEventContent {
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

impl Render for RedactedSyncMessageEvent<RedactedMessageEventContent> {
    type RenderContext = WeechatRoomMember;
    const TAGS: &'static [&'static str] = &[&"matrix_redacted"];

    fn render(&self, redacter: &Self::RenderContext) -> RenderedContent {
        // TODO add the redaction reason.
        let message = format!(
            "{}<{}Message redacted by: {}{}>{}",
            Weechat::color("chat_delimiters"),
            Weechat::color("logger.color.backlog_line"),
            redacter.nick.borrow(),
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
    fn url(&self) -> Option<&str>;
    fn file(&self) -> Option<&str>;
    fn body(&self) -> &str;
    #[inline]
    fn resolve_url(&self) -> &str {
        // the file is either encrypted or not encrypted so either `url` or
        // `file` must exist and unwrapping will never panic
        self.url().or_else(|| self.file()).unwrap()
    }
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
            fn url(&self) -> Option<&str> {
                self.url.as_deref()
            }

            #[inline]
            fn file(&self) -> Option<&str> {
                self.file.as_ref().map(|f| f.url.as_str())
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
    event: &SyncStateEvent<MemberEventContent>,
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
        NotImplemented => "performed an unimplemented operation",
    };

    fn formatted_name(member: &WeechatRoomMember) -> String {
        match &*member.display_name.borrow() {
            Some(display_name) => {
                format!(
                    "{name} {color_delim}({color_reset}{user_id}{color_delim}){color_reset}",
                    name = display_name,
                    user_id = &member.user_id,
                    color_delim = Weechat::color("chat_delimiters"),
                    color_reset = Weechat::color("reset"))
            }

            Option::None => member.user_id.as_ref().to_string(),
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
                        "{prefix} {target} {color_action}changed their avatar{color_reset}",
                        prefix = Weechat::prefix(prefix),
                        target = target_name,
                        color_action = color_action,
                        color_reset = color_reset
                        ),
                (true, false) => {
                    match new_display_name {
                        Some(name) => format!(
                            "{prefix} {target} {color_action}changed their display name to{color_reset} {new}",
                            prefix = Weechat::prefix(prefix),
                            target = target_name,
                            new = name,
                            color_action = color_action,
                            color_reset = color_reset
                            ),
                        Option::None => format!(
                            "{prefix} {target} {color_action}removed their display name{color_reset}",
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
                            "{prefix} {target} {color_action}changed their avatar \
                            and changed their display name to{color_reset} {new}",
                            prefix = Weechat::prefix(prefix),
                            target = target_name,
                            new = name,
                            color_action = color_action,
                            color_reset = color_reset
                            ),
                        Option::None => format!(
                            "{prefix} {target} {color_action}changed their \
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
        None | Error | Joined | Left | InvitationRejected | NotImplemented => {
            format!(
                "{prefix} {target} {op}",
                prefix = Weechat::prefix(prefix),
                target = target_name,
                op = operation
            )
        }
        Banned | Unbanned | Kicked | Invited | InvitationRevoked
        | KickedAndBanned => format!(
            "{prefix} {target} {op} {sender}",
            prefix = Weechat::prefix(prefix),
            target = target_name,
            op = operation,
            sender = sender_name
        ),
    }
}

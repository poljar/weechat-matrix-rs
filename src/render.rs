use crate::room_buffer::WeechatRoomMember;
use matrix_sdk::{
    events::{
        room::{
            member::{MemberEventContent, MembershipChange},
            message::{
                AudioMessageEventContent, EmoteMessageEventContent,
                FileMessageEventContent, ImageMessageEventContent,
                MessageEventContent, NoticeMessageEventContent,
                TextMessageEventContent, VideoMessageEventContent,
            },
        },
        AnyMessageEventContent, AnyPossiblyRedactedSyncMessageEvent,
        SyncStateEvent,
    },
    PossiblyRedactedExt,
};
use weechat::Weechat;

/// Rendering function for membership events (joins, leaves, bans, profile
/// changes, etc).
// TODO: We should not return raw strings here but something like [Block]
// where Block is (String, [Tags]). Each Block represents one or several lines
// which have the same tags.
pub fn render_membership(
    member: &SyncStateEvent<MemberEventContent>,
    sender: &WeechatRoomMember,
    target: &WeechatRoomMember,
) -> String {
    use MembershipChange::*;
    let change_op = member.membership_change();

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
        _ => "caused an unhandled membership change",
    };

    fn formatted_name(member: &WeechatRoomMember) -> String {
        match &member.display_name {
            Some(display_name) => {
                format!("{name} {color_delim}({color_reset}{user_id}{color_delim}){color_reset}",
                        name = display_name,
                        user_id = &member.user_id,
                        color_delim = Weechat::color("chat_delimiters"),
                        color_reset = Weechat::color("reset"))
            }

            std::option::Option::None => {
                member.user_id.as_ref().to_string()
            }
        }
    }

    let (prefix, color_action) = match change_op {
        Joined => ("join", "green"),
        Banned | ProfileChanged { .. } | Invited => ("network", "magenta"),
        _ => ("quit", "red"),
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

    match change_op {
        ProfileChanged {
            displayname_changed,
            avatar_url_changed,
        } => {
            let new_display_name = &member.content.displayname;

            // TODO: Should we display the new avatar URL?
            // let new_avatar = member.content.avatar_url.as_ref();

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
                        std::option::Option::None => format!(
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
                            "{prefix} {target} {color_action}changed their avatar and changed their display name to{color_reset} {new}",
                            prefix = Weechat::prefix(prefix),
                            target = target_name,
                            new = name,
                            color_action = color_action,
                            color_reset = color_reset
                            ),
                        std::option::Option::None => format!(
                            "{prefix} {target} {color_action}changed their avatar and removed display name{color_reset}",
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

        // This means an unsupported membership change happened so we just print a generic
        // message to indicate this.
        _ => format!(
            "{prefix} {sender} {op}",
            prefix = Weechat::prefix(prefix),
            sender = sender_name,
            op = operation,
        ),
    }
}

/// Rendering function for room messages.
// FIXME: Pass room member
// TODO: We should not return raw strings here but something like [Block]
// where Block is (String, [Tags]). Each Block represents one or several lines
// which have the same tags.
pub fn render_message(
    message: &AnyPossiblyRedactedSyncMessageEvent,
    displayname: String,
) -> String {
    use AnyPossiblyRedactedSyncMessageEvent::*;
    use MessageEventContent::*;

    // TODO: Need to render power level indicators as well.

    // In case it's not clear, self.sender is the MXID. We're basing the nick color on it so
    // that it doesn't change with display name changes.
    let colorname_user =
        Weechat::info_get("nick_color_name", message.sender().as_ref())
            .unwrap_or_else(|| String::from("default"));
    let color_user = Weechat::color(&colorname_user);

    let color_reset = Weechat::color("reset");

    match message {
        Regular(message) => {
            match message.content() {
                AnyMessageEventContent::RoomEncrypted(_) => format!(
                    "{color_user}{}{color_reset}\t{}",
                    displayname,
                    "Unable to decrypt message",
                    color_user = color_user,
                    color_reset = color_reset
                ),

                AnyMessageEventContent::RoomMessage(content) => {
                    match content {
                        // TODO: formatting for inline markdown and so on
                        Text(t) => format!(
                            "{color_user}{}{color_reset}\t{}",
                            displayname,
                            t.resolve_body(),
                            color_user = color_user,
                            color_reset = color_reset
                        ),
                        Emote(e) => format!(
                            "{prefix}\t{color_user}{}{color_reset} {}",
                            displayname,
                            e.resolve_body(),
                            prefix = Weechat::prefix("action"),
                            color_user = color_user,
                            color_reset = color_reset
                        ),
                        Audio(a) => format!(
                            "{color_user}{}{color_reset}\t{}: {}",
                            displayname,
                            a.body,
                            a.resolve_url(),
                            color_user = color_user,
                            color_reset = color_reset
                        ),
                        File(f) => format!(
                            "{color_user}{}{color_reset}\t{}: {}",
                            displayname,
                            f.body,
                            f.resolve_url(),
                            color_user = color_user,
                            color_reset = color_reset
                        ),
                        Image(i) => format!(
                            "{color_user}{}{color_reset}\t{}: {}",
                            displayname,
                            i.body,
                            i.resolve_url(),
                            color_user = color_user,
                            color_reset = color_reset
                        ),
                        Location(l) => format!(
                            "{color_user}{}{color_reset}\t{}: {}",
                            displayname,
                            l.body,
                            l.geo_uri,
                            color_user = color_user,
                            color_reset = color_reset
                        ),
                        Notice(n) => format!(
                            "{prefix}{color_notice}Notice{color_delim}({color_reset}{}{color_delim}){color_reset}: {}",
                            displayname,
                            n.resolve_body(),
                            prefix = Weechat::prefix("network"),
                            color_notice = Weechat::color("irc.color.notice"),
                            color_delim = Weechat::color("chat_delimiters"),
                            color_reset = color_reset
                        ),
                        Video(v) => format!(
                            "{color_user}{}{color_reset}\t{}: {}",
                            displayname,
                            v.body,
                            v.resolve_url(),
                            color_user = color_user,
                            color_reset = color_reset
                        ),
                        ServerNotice(n) => format!(
                            "{prefix}{color_notice}Server notice{color_delim}({color_reset}{}{color_delim}){color_reset}: {}",
                            displayname,
                            n.body,
                            prefix = Weechat::prefix("network"),
                            color_notice = Weechat::color("irc.color.notice"),
                            color_delim = Weechat::color("chat_delimiters"),
                            color_reset = color_reset
                        ),
                        e => format!(
                            "{color_user}{}{color_reset}\tUnknown message type: {:#?}",
                            displayname,
                            e,
                            color_user = color_user,
                            color_reset = color_reset
                        ),
                    }
                }
                _ => {
                    // TODO: Handle rendering of message types other than RoomMessage
                    todo!("Handle rendering of message types other than RoomMessage");
                }
            }
        }

        AnyPossiblyRedactedSyncMessageEvent::Redacted(_message) => {
            // TODO: Handle rendering redacted events
            todo!("Handle rendering redacted events");
        }
    }
}

/// Trait for message event types that contain an optional formatted body. `resolve_body` will
/// return the formatted body if present, else fallback to the regular body.
trait HasFormattedBody {
    fn body(&self) -> &str;
    fn formatted_body(&self) -> Option<&str>;
    #[inline]
    fn resolve_body(&self) -> &str {
        self.formatted_body().unwrap_or_else(|| self.body())
    }
}

// Repeating this for each event type would get boring fast so lets use a simple macro to implement
// the trait for a struct that has a `body` and `formatted_body` field
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

/// This trait is implemented for message types that can contain either an URL or an encrypted
/// file. One of these _must_ be present.
trait HasUrlOrFile {
    fn url(&self) -> Option<&str>;
    fn file(&self) -> Option<&str>;
    #[inline]
    fn resolve_url(&self) -> &str {
        // the file is either encrypted or not encrypted so either `url` or `file` must
        // exist and unwrapping will never panic
        self.url().or_else(|| self.file()).unwrap()
    }
}

// Same as above: a simple macro to implement the trait for structs with `url` and `file` fields.
macro_rules! has_url_or_file {
    ($content: ident) => {
        impl HasUrlOrFile for $content {
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

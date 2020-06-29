use matrix_sdk::{
    events::room::{
        encrypted::EncryptedEvent,
        member::{MemberEvent, MembershipChange, MembershipState},
        message::{
            AudioMessageEventContent, EmoteMessageEventContent,
            FileMessageEventContent, ImageMessageEventContent, MessageEvent,
            MessageEventContent, NoticeMessageEventContent,
            TextMessageEventContent, VideoMessageEventContent,
        },
    },
    identifiers::UserId,
};
use weechat::Weechat;

/// This trait describes events that can be rendered in the weechat UI
pub(crate) trait RenderableEvent {
    /// Convert the event into a string that will be displayed in the UI.
    /// The displayname is taken as a parameter since it cannot be calculated from the event
    /// context alone.
    fn render(&self, displayname: &str) -> String;

    /// Get the sender for this event this can be used to get the displayname.
    fn sender(&self) -> &UserId;
}

impl RenderableEvent for EncryptedEvent {
    // TODO: this is not implemented yet
    fn render(&self, displayname: &str) -> String {
        let color_user = Weechat::color("green"); // TODO: get per-user color
        let color_reset = Weechat::color("reset");
        format!(
            "{color_user}{}{color_reset}\t{}",
            displayname,
            "Unable to decrypt message",
            color_user = color_user,
            color_reset = color_reset
        )
    }

    fn sender(&self) -> &UserId {
        &self.sender
    }
}

impl RenderableEvent for MemberEvent {
    fn render(&self, displayname: &str) -> String {
        use MembershipChange::*;
        let change_op = self.membership_change();
        let operation = match change_op {
            None => "did nothing",
            Error => "caused an error", // must never happen
            Joined => "has joined the room",
            Left => "has left the room",
            Banned => "was banned",
            Unbanned => "was unbanned",
            Kicked => "was kicked from the room",
            Invited => "was invited to the room",
            KickedAndBanned => "was kicked and banned",
            InvitationRejected => "rejected the invitation",
            InvitationRevoked => "had the invitation revoked",
            ProfileChanged => "changed their display name or avatar",
            NotImplemented => "performed an unimplemented operation",
        };

        let (prefix, color_action) = match change_op {
            Joined => ("join", "green"),
            Banned | ProfileChanged | Invited => ("network", "magenta"),
            _ => ("quit", "red"),
        };

        format!(
            "{prefix}{color_user}{name} {color_deli}({color_reset}{state}{color_deli}){color_reset} {color_action}{op}",
            prefix = Weechat::prefix(prefix),
            color_user = Weechat::color("reset"), // TODO
            color_deli = Weechat::color("chat_delimiters"),
            color_action = Weechat::color(color_action),
            color_reset = Weechat::color("reset"),
            name = displayname,
            state = self.state_key,
            op = operation,
        )
    }

    fn sender(&self) -> &UserId {
        &self.sender
    }
}

impl RenderableEvent for MessageEvent {
    fn render(&self, displayname: &str) -> String {
        use MessageEventContent::*;

        // TODO: Need to render power level indicators as well.

        // In case it's not clear, self.sender is the MXID. We're basing the nick color on it so
        // that it doesn't change with display name changes.
        let colorname_user =
            Weechat::info_get("nick_color_name", self.sender.as_ref())
                .unwrap_or(String::from("default"));
        let color_user = Weechat::color(&colorname_user);

        let color_reset = Weechat::color("reset");

        match &self.content {
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
        }
    }

    fn sender(&self) -> &UserId {
        &self.sender
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
                self.formatted_body.as_deref()
            }
        }
    };
}

/// This trait is implemented for message types that can contain either an URL or an encrypted
/// file. One of both _must_ be present.
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

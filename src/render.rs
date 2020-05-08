use matrix_sdk::events::room::member::{MemberEvent, MembershipState};
use matrix_sdk::events::room::message::{
    AudioMessageEventContent, EmoteMessageEventContent,
    FileMessageEventContent, ImageMessageEventContent, MessageEvent,
    MessageEventContent, NoticeMessageEventContent, TextMessageEventContent,
    VideoMessageEventContent,
};

pub(crate) trait Renderable {
    fn render(&self) -> String;
}

impl Renderable for MemberEvent {
    fn render(&self) -> String {
        let operation = match self.content.membership {
            MembershipState::Join => "joined",
            MembershipState::Leave => "left",
            MembershipState::Ban => "banned",
            MembershipState::Invite => "invited",
            MembershipState::Knock => "knocked on", // TODO
        };
        format!(
            "{} ({}) has {} the room",
            self.content.displayname.as_deref().unwrap_or(""),
            self.state_key,
            operation
        )
    }
}

impl Renderable for MessageEvent {
    fn render(&self) -> String {
        use MessageEventContent::*;
        let sender = &self.sender;
        match &self.content {
            Text(t) => format!("{}\t{}", sender, t.resolve_body()),
            Emote(e) => format!("{}\t{}", sender, e.resolve_body()),
            Audio(a) => format!("{}\t{}: {}", sender, a.body, a.resolve_url()),
            File(f) => format!("{}\t{}: {}", sender, f.body, f.resolve_url()),
            Image(i) => format!("{}\t{}: {}", sender, i.body, i.resolve_url()),
            Location(l) => format!("{}\t{}: {}", sender, l.body, l.geo_uri),
            Notice(n) => format!("{}\t{}", sender, n.resolve_body()),
            Video(v) => format!("{}\t{}: {}", sender, v.body, v.resolve_url()),
            ServerNotice(sn) => {
                format!("SERVER\t{}", sn.body) // TODO
            }
        }
    }
}

trait HasFormattedBody {
    fn body(&self) -> &str;
    fn formatted_body(&self) -> Option<&str>;
    #[inline]
    fn resolve_body(&self) -> &str {
        self.formatted_body().unwrap_or_else(|| self.body())
    }
}

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

trait HasUrlOrFile {
    fn url(&self) -> Option<&str>;
    fn file(&self) -> Option<&str>;
    #[inline]
    fn resolve_url(&self) -> &str {
        // the file is either encrypted or not encrypted so either `url` or `file` must
        // exist and unwrapping will never panic
        self.url().or(self.file()).unwrap()
    }
}

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

has_formatted_body!(EmoteMessageEventContent);
has_formatted_body!(NoticeMessageEventContent);
has_formatted_body!(TextMessageEventContent);

has_url_or_file!(AudioMessageEventContent);
has_url_or_file!(FileMessageEventContent);
has_url_or_file!(ImageMessageEventContent);
has_url_or_file!(VideoMessageEventContent);

use std::rc::Rc;

use dashmap::DashMap;
use tokio::runtime::Handle;
use tracing::{error, info};

use matrix_sdk::{
    deserialized_responses::AmbiguityChange,
    room::{Room, RoomMember},
    ruma::{
        events::{
            room::member::{MembershipState, RoomMemberEventContent},
            SyncStateEvent,
        },
        uint, OwnedUserId, UserId,
    },
};

use weechat::{
    buffer::{Buffer, NickSettings},
    Prefix, Weechat,
};

use crate::{render::render_membership, room::buffer::RoomBuffer};

#[derive(Clone)]
pub struct Members {
    room: Room,
    pub(super) runtime: Handle,
    ambiguity_map: Rc<DashMap<OwnedUserId, bool>>,
    nicks: Rc<DashMap<OwnedUserId, String>>,
    buffer: RoomBuffer,
}

#[derive(Clone, Debug)]
pub struct WeechatRoomMember {
    inner: RoomMember,
    color: Rc<String>,
    ambiguous_nick: Rc<bool>,
}

impl PartialEq for WeechatRoomMember {
    fn eq(&self, other: &Self) -> bool {
        self.user_id() == other.user_id()
    }
}

impl Members {
    pub fn new(room: Room, runtime: Handle, buffer: RoomBuffer) -> Self {
        Self {
            room,
            runtime,
            nicks: DashMap::new().into(),
            ambiguity_map: DashMap::new().into(),
            buffer,
        }
    }

    fn add_nick(&self, buffer: &Buffer, member: &WeechatRoomMember) {
        let nick = member.nick();

        let group = buffer
            .search_nicklist_group(member.nicklist_group_name())
            .expect("No group found when adding member");

        let nick_settings = NickSettings::new(&nick)
            .set_color(member.color())
            .set_prefix(member.nicklist_prefix())
            .set_prefix_color(member.prefix_color());

        info!("Inserting nick {} for room {}", nick, buffer.short_name());

        if group.add_nick(nick_settings).is_err() {
            error!(
                "Error adding nick {} ({}) to room {}, already added.",
                nick,
                member.user_id(),
                buffer.short_name()
            );
        };

        self.nicks.insert(member.user_id().to_owned(), nick);
    }

    pub async fn restore_member(&self, user_id: OwnedUserId) {
        let room = self.room.clone();
        let user = user_id.to_owned();

        match self
            .runtime
            .spawn(async move { room.get_member_no_sync(&user).await })
            .await
            .expect("Fetching the room member from the store panicked")
        {
            Ok(Some(member)) => {
                self.ambiguity_map
                    .insert(user_id.to_owned(), member.name_ambiguous());
                self.update_member(&user_id).await;
            }
            Ok(None) => {
                panic!(
                    "Couldn't find member {} in {}",
                    user_id,
                    self.buffer.short_name()
                )
            }
            Err(e) => {
                Weechat::print(&format!(
                    "{}: Error fetching a room member from the store: {}",
                    Weechat::prefix(Prefix::Error),
                    e.to_string(),
                ));
            }
        }
    }

    pub async fn update_member(&self, user_id: &UserId) {
        let buffer = self.buffer.buffer_handle();

        let buffer = if let Ok(b) = buffer.upgrade() {
            b
        } else {
            return;
        };

        if let Some(nick) = self.nicks.get(user_id) {
            buffer.remove_nick(&nick);
        }

        let member = self.get(user_id).await.unwrap_or_else(|| {
            panic!(
                "Couldn't find member {} in {}",
                user_id,
                buffer.short_name()
            )
        });

        self.add_nick(&buffer, &member);
    }

    /// Add a new Weechat room member.
    pub async fn add_or_modify(
        &self,
        user_id: &UserId,
        ambiguity_change: Option<&AmbiguityChange>,
    ) {
        if let Some(change) = ambiguity_change {
            self.ambiguity_map
                .insert(user_id.to_owned(), change.member_ambiguous);

            if let Some(disambiguated) = &change.disambiguated_member {
                self.ambiguity_map.insert(disambiguated.clone(), false);
                self.update_member(disambiguated).await;
            }

            if let Some(ambiguated) = &change.ambiguated_member {
                self.ambiguity_map.insert(ambiguated.clone(), true);
                self.update_member(ambiguated).await;
            }
        }

        self.update_member(user_id).await;
    }

    /// Remove a Weechat room member by user ID.
    ///
    /// Returns either the removed Weechat room member, or an error if the
    /// member does not exist.
    async fn remove(
        &self,
        user_id: &UserId,
        ambiguity_change: Option<&AmbiguityChange>,
    ) {
        self.ambiguity_map.remove(user_id);

        if let Some(change) = ambiguity_change {
            if let Some(disambiguated) = &change.disambiguated_member {
                self.ambiguity_map.insert(disambiguated.clone(), false);
                self.update_member(disambiguated).await;
            }

            if let Some(ambiguated) = &change.ambiguated_member {
                self.ambiguity_map.insert(ambiguated.clone(), true);
                self.update_member(ambiguated).await;
            }
        }

        let buffer = self.buffer.buffer_handle();

        let buffer = if let Ok(b) = buffer.upgrade() {
            b
        } else {
            return;
        };

        if let Some((_, nick)) = self.nicks.remove(user_id) {
            buffer.remove_nick(&nick);
        }
    }

    /// Retrieve a reference to a Weechat room member by user ID.
    pub async fn get(&self, user_id: &UserId) -> Option<WeechatRoomMember> {
        let color = if self.room.own_user_id() == user_id {
            "weechat.color.chat_nick_self".into()
        } else {
            Weechat::info_get("nick_color_name", user_id.as_str())
                .expect("Couldn't get the nick color name")
        };

        let room = self.room.clone();
        let user = user_id.to_owned();

        match self
            .runtime
            .spawn(async move { room.get_member_no_sync(&user).await })
            .await
            .expect("Fetching the room member from the store panicked")
        {
            Ok(m) => m.map(|m| WeechatRoomMember {
                color: Rc::new(color),
                ambiguous_nick: Rc::new(
                    self.ambiguity_map
                        .get(m.user_id())
                        .map(|a| *a)
                        .unwrap_or(false),
                ),
                inner: m,
            }),
            Err(e) => {
                Weechat::print(&format!(
                    "{}: Error fetching a room member from the store: {}",
                    Weechat::prefix(Prefix::Error),
                    e.to_string(),
                ));
                None
            }
        }
    }

    fn room(&self) -> &Room {
        &self.room
    }

    pub async fn handle_membership_event(
        &self,
        event: &SyncStateEvent<RoomMemberEventContent>,
        state_event: bool,
        ambiguity_change: Option<&AmbiguityChange>,
    ) {
        let buffer = self.buffer.buffer_handle();
        let buffer = if let Ok(b) = buffer.upgrade() {
            b
        } else {
            return;
        };

        let event = match event {
            SyncStateEvent::Original(e) => e,
            SyncStateEvent::Redacted(e) => {
                error!("Unhandled redacted event: {e:?}");
                return;
            }
        };

        info!(
            "Handling membership event for room {} {} {:?}",
            buffer.short_name(),
            event.state_key,
            event.content.membership
        );

        let sender_id = event.sender.clone();

        let target_id = if let Ok(t) = UserId::parse(event.state_key.clone()) {
            t
        } else {
            error!(
                "Invalid state key in room {} from sender {}: {}",
                buffer.short_name(),
                event.sender,
                event.state_key,
            );
            return;
        };

        use MembershipState::*;

        // For joins and invites, first we need to check whether a member
        // with some MXID exists. If he does, we have to update *that*
        // member with the new state. Only if they do not exist yet do we
        // create a new one.
        //
        // For leaves and bans we just need to remove the member.
        match event.content.membership {
            Invite | Join => {
                self.add_or_modify(&target_id, ambiguity_change).await
            }
            Leave | Ban => self.remove(&target_id, ambiguity_change).await,
            _ => (),
        };

        // Names of rooms without display names can get affected by the
        // member list so we need to update them.
        self.buffer.update_buffer_name();

        if !state_event {
            let sender = self.get(&sender_id).await;
            let target = self.get(&target_id).await;

            // Display the event message
            let message = match (&sender, &target) {
                (Some(sender), Some(target)) => {
                    render_membership(event, sender, target)
                }

                _ => {
                    if sender.is_none() {
                        error!(
                            "Cannot render event since event sender {} is not a room member",
                            sender_id);
                    }

                    if target.is_none() {
                        error!(
                            "Cannot render event since event target {} is not a room member",
                            target_id);
                    }

                    "ERROR: cannot render event since sender or target are not a room member".into()
                }
            };

            let timestamp: i64 =
                (event.origin_server_ts.0 / uint!(1000)).into();
            buffer.print_date_tags(timestamp as i64, &[], &message);
        }
    }
}

impl WeechatRoomMember {
    pub fn user_id(&self) -> &UserId {
        self.inner.user_id()
    }

    pub fn display_name(&self) -> Option<&str> {
        self.inner.display_name()
    }

    pub fn color(&self) -> &str {
        &self.color
    }

    fn nick_raw(&self) -> &str {
        self.inner.name()
    }

    fn nicklist_group_name(&self) -> &str {
        match self.inner.normalized_power_level() {
            p if p >= 100 => "000|o",
            p if p >= 50 => "001|h",
            p if p > 0 => "002|v",
            _ => "999|...",
        }
    }

    fn nicklist_prefix(&self) -> &str {
        match self.inner.normalized_power_level() {
            p if p >= 100 => "&",
            p if p >= 50 => "@",
            p if p > 0 => "+",
            _ => " ",
        }
    }

    fn prefix(&self) -> &str {
        self.nicklist_prefix().trim()
    }

    fn prefix_color(&self) -> &str {
        match self.prefix() {
            "&" => "lightgreen",
            "@" => "lightmagenta",
            "+" => "yellow",
            _ => "default",
        }
    }

    pub fn nick_colored(&self) -> String {
        if *self.ambiguous_nick {
            // TODO: this should color the parenthesis differently.
            format!(
                "{}{}{} ({})",
                Weechat::color(&self.color()),
                self.nick_raw(),
                Weechat::color("reset"),
                self.user_id(),
            )
        } else {
            format!(
                "{}{}{}{}{}",
                Weechat::color(self.prefix_color()),
                self.prefix(),
                Weechat::color(self.color()),
                self.nick_raw(),
                Weechat::color("reset")
            )
        }
    }

    pub fn nick(&self) -> String {
        if *self.ambiguous_nick {
            format!("{} ({})", self.nick_raw(), self.user_id())
        } else {
            self.nick_raw().to_string()
        }
    }
}

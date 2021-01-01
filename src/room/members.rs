use std::{cell::RefCell, convert::TryFrom, rc::Rc};

use dashmap::{DashMap, DashSet};
use futures::executor::block_on;
use tracing::{error, info};

use matrix_sdk::{
    events::{
        room::member::{MemberEventContent, MembershipState},
        SyncStateEvent,
    },
    identifiers::UserId,
    JoinedRoom, RoomMember,
};

use weechat::{
    buffer::{Buffer, BufferHandle, NickSettings},
    Weechat,
};

use super::BUFFER_CLOSED_ERROR;
use crate::render::render_membership;

#[derive(Clone)]
pub struct Members {
    room: JoinedRoom,
    display_names: Rc<DashMap<String, Rc<DashSet<UserId>>>>,
    nicks: Rc<DashMap<UserId, (String, String)>>,
    pub(super) buffer: Rc<Option<BufferHandle>>,
}

#[derive(Clone, Debug)]
pub struct WeechatRoomMember {
    inner: RoomMember,
    color: Rc<String>,
    ambiguous_nick: Rc<RefCell<bool>>,
}

impl Members {
    pub fn new(room: JoinedRoom) -> Self {
        Self {
            room,
            nicks: DashMap::new().into(),
            display_names: DashMap::new().into(),
            buffer: None.into(),
        }
    }

    fn buffer(&self) -> Buffer<'_> {
        self.buffer
            .as_ref()
            .as_ref()
            .expect("Members struct wasn't initialized properly")
            .upgrade()
            .expect(BUFFER_CLOSED_ERROR)
    }

    pub async fn disambiguate_nicks(
        &self,
        user_id: &UserId,
        current_nick: Option<&str>,
        new_nick: Option<&str>,
    ) -> bool {
        let buffer = self.buffer();

        if let Some(nick) = current_nick {
            let used_names = self
                .display_names
                .entry(nick.to_owned())
                .or_insert_with(|| Rc::new(DashSet::new()))
                .clone();

            used_names.remove(user_id);

            if used_names.len() == 1 {
                for user in used_names.iter() {
                    let member = self.get(&user).await.expect(
                        "Used display names and memeber list out of sync",
                    );
                    member.set_ambiguous(false);
                    self.remove_only(&user).await;
                    self.add_nick(&buffer, &member);
                }
            }
        }

        if let Some(nick) = new_nick {
            let used_names = self
                .display_names
                .entry(nick.to_owned())
                .or_insert_with(|| Rc::new(DashSet::new()))
                .clone();

            used_names.insert(user_id.to_owned());

            if used_names.len() == 2 {
                for user in used_names.iter().filter(|u| u.as_ref() != user_id)
                {
                    let member = self.get(&user).await.expect(
                        "Used display names and memeber list out of sync",
                    );
                    member.set_ambiguous(true);
                    self.remove_only(&user).await;
                    self.add_nick(&buffer, &member);
                }
            }

            used_names.len() > 1
        } else {
            false
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

        if let Err(_) = group.add_nick(nick_settings) {
            error!(
                "Error adding nick {} ({}) to room {}, already addded.",
                nick,
                member.user_id(),
                buffer.short_name()
            );
        };

        self.nicks.insert(
            member.user_id().to_owned(),
            (member.nick_raw().to_string(), nick),
        );
    }

    /// Add a new Weechat room member.
    pub async fn add_or_modify(&self, user_id: &UserId) {
        let buffer = self.buffer();

        let prev_nick = if let Some((_, (display_name, nick))) =
            self.nicks.remove(user_id)
        {
            buffer.remove_nick(&nick);
            Some(display_name)
        } else {
            None
        };

        let member = self.get(user_id).await.unwrap_or_else(|| {
            panic!(
                "Couldn't find member {} in {}",
                user_id,
                buffer.short_name()
            )
        });

        let new_nick = member.nick_raw();

        let ambigous = self
            .disambiguate_nicks(
                member.user_id(),
                prev_nick.as_deref(),
                Some(&new_nick),
            )
            .await;
        member.set_ambiguous(ambigous);

        self.add_nick(&buffer, &member);
    }

    async fn remove_only(&self, user_id: &UserId) {
        let buffer = self.buffer();

        if let Some((_, (_, nick))) = self.nicks.remove(user_id) {
            buffer.remove_nick(&nick);
        }
    }

    /// Remove a Weechat room member by user ID.
    ///
    /// Returns either the removed Weechat room member, or an error if the
    /// member does not exist.
    async fn remove(&self, user_id: &UserId) {
        let buffer = self.buffer();

        if let Some((_, (display_name, nick))) = self.nicks.remove(user_id) {
            buffer.remove_nick(&nick);
            self.disambiguate_nicks(user_id, Some(&display_name), None)
                .await;
        }
    }

    /// Retrieve a reference to a Weechat room member by user ID.
    pub async fn get(&self, user_id: &UserId) -> Option<WeechatRoomMember> {
        let color = if self.room.own_user_id() == user_id {
            "weechat.color.chat_nick_self".into()
        } else {
            Weechat::info_get("nick_color_name", user_id.as_str())
                .expect("Couldn't get the nick color name")
                .into()
        };

        self.room
            .get_member(user_id)
            .await
            .map(|m| WeechatRoomMember {
                color: Rc::new(color),
                ambiguous_nick: Rc::new(RefCell::new(
                    self.display_names
                        .get(m.name())
                        .map(|u| u.len() > 1)
                        .unwrap_or(false),
                )),
                inner: m,
            })
    }

    fn room(&self) -> &JoinedRoom {
        &self.room
    }

    pub fn calculate_buffer_name(&self) -> String {
        let room = self.room();
        let room_name = block_on(room.display_name());

        if room_name == "#" {
            "##".to_owned()
        } else if room_name.starts_with('#') || room.is_direct() {
            room_name
        } else {
            format!("#{}", room_name)
        }
    }

    fn update_buffer_name(&self) {
        let name = self.calculate_buffer_name();
        let buffer = self.buffer();
        buffer.set_short_name(&name)
    }

    pub async fn handle_membership_event(
        &self,
        event: &SyncStateEvent<MemberEventContent>,
        state_event: bool,
    ) {
        let buffer = self.buffer();

        info!(
            "Handling membership event for room {} {} {:?}",
            buffer.short_name(),
            event.state_key,
            event.content.membership
        );

        let sender_id = event.sender.clone();

        let target_id = if let Ok(t) = UserId::try_from(event.state_key.clone())
        {
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
        let target = match event.content.membership {
            Invite | Join => Some(self.add_or_modify(&target_id).await),
            Leave | Ban => self.remove(&target_id).await,
            Knock | _ => None,
        };

        // Names of rooms without display names can get affected by the
        // member list so we need to update them.
        self.update_buffer_name();

        if !state_event {
            let sender = self.get(&sender_id).await;

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

            let timestamp: u64 = event
                .origin_server_ts
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

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

    fn set_ambiguous(&self, ambiguous: bool) {
        *self.ambiguous_nick.borrow_mut() = ambiguous;
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
        if *self.ambiguous_nick.borrow() {
            // TODO this should color the parenthesis differently.
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
        if *self.ambiguous_nick.borrow() {
            format!("{} ({})", self.nick_raw(), self.user_id())
        } else {
            self.nick_raw().to_string()
        }
    }
}

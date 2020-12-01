use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    convert::TryFrom,
    rc::Rc,
};

use futures::executor::block_on;
use tracing::{error, info};

use matrix_sdk::{
    events::{
        room::member::{MemberEventContent, MembershipState},
        SyncStateEvent,
    },
    identifiers::UserId,
    Room,
};

use weechat::{
    buffer::{Buffer, BufferHandle, NickSettings},
    Weechat,
};

use super::BUFFER_CLOSED_ERROR;
use crate::render::render_membership;

#[derive(Clone)]
pub struct Members {
    room: Room,
    inner: Rc<RefCell<HashMap<UserId, WeechatRoomMember>>>,
    display_names: Rc<RefCell<HashMap<String, HashSet<UserId>>>>,
    pub(super) buffer: Rc<Option<BufferHandle>>,
}

#[derive(Clone, Debug)]
pub struct WeechatRoomMember {
    pub user_id: Rc<UserId>,
    pub nick: Rc<RefCell<String>>,
    pub display_name: Rc<Option<String>>,
    pub prefix: Rc<Option<String>>,
    pub color: Rc<str>,
    pub ambiguous_nick: Rc<RefCell<bool>>,
}

impl Members {
    pub fn new(room: Room) -> Self {
        Self {
            room,
            inner: Rc::new(RefCell::new(HashMap::new())),
            display_names: Rc::new(RefCell::new(HashMap::new())),
            buffer: Rc::new(None),
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
        current_nick: &str,
        new_nick: &str,
    ) -> bool {
        let buffer = self.buffer();

        let mut display_names = self.display_names.borrow_mut();

        let used_names = display_names
            .entry(current_nick.to_owned())
            .or_insert_with(HashSet::new);

        used_names.remove(user_id);

        if used_names.len() == 1 {
            for user in used_names.iter() {
                let member = self
                    .get(user)
                    .expect("Used display names and memeber list out of sync");
                buffer.remove_nick(&member.nick());
                member.set_ambiguous(false);
                self.add_nick(&buffer, &member);
            }
        }

        let used_names = display_names
            .entry(new_nick.to_owned())
            .or_insert_with(HashSet::new);

        used_names.insert(user_id.to_owned());

        if used_names.len() > 1 {
            for user in used_names.iter().filter(|u| u != &user_id) {
                let member = self
                    .get(user)
                    .expect("Used display names and memeber list out of sync");
                buffer.remove_nick(&member.nick());
                member.set_ambiguous(true);
                self.add_nick(&buffer, &member);
            }
        }

        used_names.len() > 1
    }

    fn add_nick(&self, buffer: &Buffer, member: &WeechatRoomMember) {
        let nick = member.nick();
        let nick_settings = NickSettings::new(&nick).set_color(&member.color);

        info!("Inserting nick {} for room {}", nick, buffer.short_name());

        if let Err(_) = buffer.add_nick(nick_settings) {
            error!("Error adding nick {}, already addded.", nick);
        };
    }

    /// Add a new Weechat room member.
    pub async fn add_or_modify(&self, user_id: &UserId) -> WeechatRoomMember {
        let buffer = self.buffer();

        if let Some(member) = self.get(user_id) {
            let (new_nick, ambiguous) = {
                buffer.remove_nick(&member.nick());

                let current_nick = member.nick.borrow();
                let new_nick = self.calculate_user_name(&user_id).await;
                let ambiguous = self
                    .disambiguate_nicks(&user_id, &current_nick, &new_nick)
                    .await;

                (new_nick, ambiguous)
            };

            member.update_nick(new_nick, ambiguous);
            self.add_nick(&buffer, &member);
        } else {
            let new_nick = self.calculate_user_name(&user_id).await;
            let display_name = self
                .room()
                .get_member(&user_id)
                .await
                .map(|m| m.display_name().clone())
                .flatten();

            let ambiguous = self
                .disambiguate_nicks(&user_id, &new_nick, &new_nick)
                .await;

            let member = WeechatRoomMember::new(
                user_id,
                new_nick,
                display_name,
                ambiguous,
                user_id == self.room.own_user_id(),
            );

            self.add_nick(&buffer, &member);
            self.inner.borrow_mut().insert(user_id.clone(), member);
        };

        self.get(user_id).expect("Can't get inserted member")
    }

    /// Remove a Weechat room member by user ID.
    ///
    /// Returns either the removed Weechat room member, or an error if the
    /// member does not exist.
    fn remove(&self, user_id: &UserId) -> Option<WeechatRoomMember> {
        let buffer = self.buffer();

        if let Some(member) = self.inner.borrow_mut().remove(user_id) {
            buffer.remove_nick(&member.nick());
            Some(member)
        } else {
            None
        }
    }

    /// Retrieve a reference to a Weechat room member by user ID.
    pub fn get(&self, user_id: &UserId) -> Option<WeechatRoomMember> {
        self.inner.borrow().get(user_id).cloned()
    }

    fn room(&self) -> &Room {
        &self.room
    }

    pub fn calculate_buffer_name(&self) -> String {
        let room = self.room();
        let room_name = block_on(room.display_name());

        if room_name == "#" {
            "##".to_owned()
        } else if room_name.starts_with('#') {
            room_name
        } else {
            // TODO: only do this for non-direct chats
            format!("#{}", room_name)
        }
    }

    fn update_buffer_name(&self) {
        let name = self.calculate_buffer_name();
        let buffer = self.buffer();
        buffer.set_short_name(&name)
    }

    /// Helper method to calculate the display name of a room member from their
    /// UserId.
    ///
    /// If no member with that ID is in the room, the string representation of
    /// the ID will be returned.
    async fn calculate_user_name(&self, user_id: &UserId) -> String {
        let member =
            self.room().get_member(user_id).await.unwrap_or_else(|| {
                panic!(
                    "No such member {} in {}",
                    user_id,
                    self.buffer().short_name()
                )
            });

        // TODO
        if let Some(name) = member.display_name() {
            name.to_owned()
        } else {
            member.disambiguated_name()
        }
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
                "Invalid state key given by the server: {}",
                event.state_key
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
            Leave | Ban => self.remove(&target_id),
            Knock | _ => None,
        };

        // Names of rooms without display names can get affected by the
        // member list so we need to update them.
        self.update_buffer_name();

        if !state_event {
            let sender = self.get(&sender_id);

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
    pub fn new(
        user_id: &UserId,
        nick: String,
        display_name: Option<String>,
        ambigous: bool,
        own_user: bool,
    ) -> Self {
        let color = if own_user {
            "weechat.color.chat_nick_self".to_owned()
        } else {
            Weechat::info_get("nick_color_name", user_id.as_str())
                .expect("Couldn't get the nick color name")
        };

        WeechatRoomMember {
            user_id: Rc::new(user_id.clone()),
            nick: Rc::new(RefCell::new(nick)),
            display_name: Rc::new(display_name),
            prefix: Rc::new(None),
            color: color.into(),
            ambiguous_nick: Rc::new(RefCell::new(ambigous.into())),
        }
    }

    fn update_nick(&self, new_nick: String, ambiguous: bool) {
        *self.nick.borrow_mut() = new_nick;
        *self.ambiguous_nick.borrow_mut() = ambiguous;
    }

    fn set_ambiguous(&self, ambiguous: bool) {
        *self.ambiguous_nick.borrow_mut() = ambiguous;
    }

    pub fn nick_colored(&self) -> String {
        if *self.ambiguous_nick.borrow() {
            // TODO this should color the parenthesis differently.
            format!(
                "{}{}{} ({})",
                Weechat::color(&self.color),
                self.nick.borrow(),
                Weechat::color("reset"),
                self.user_id,
            )
        } else {
            format!(
                "{}{}{}",
                Weechat::color(&self.color),
                self.nick.borrow(),
                Weechat::color("reset")
            )
        }
    }

    pub fn nick(&self) -> String {
        if *self.ambiguous_nick.borrow() {
            format!("{} ({})", self.nick.borrow(), self.user_id)
        } else {
            self.nick.borrow().to_owned()
        }
    }
}

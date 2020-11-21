use std::{cell::RefCell, collections::HashMap, convert::TryFrom, rc::Rc};

use futures::executor::block_on;
use tracing::{debug, error, trace};

use matrix_sdk::{
    events::{
        room::member::{
            MemberEventContent,
            MembershipChange::{
                Banned, InvitationRejected, InvitationRevoked, Invited, Joined,
                Kicked, KickedAndBanned, Left, ProfileChanged,
            },
            MembershipState,
        },
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
    pub(super) buffer: Rc<Option<BufferHandle>>,
}

enum RoomError {
    NonExistentMember(UserId),
}

#[derive(Clone, Debug)]
pub struct WeechatRoomMember {
    pub user_id: Rc<UserId>,
    pub nick: Rc<RefCell<String>>,
    pub display_name: Rc<RefCell<Option<String>>>,
    pub prefix: Rc<RefCell<Option<String>>>,
    #[allow(clippy::rc_buffer)]
    pub color: Rc<String>,
}

impl Members {
    pub fn new(room: Room) -> Self {
        Self {
            room,
            inner: Rc::new(RefCell::new(HashMap::new())),
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

    /// Add a new Weechat room member.
    pub fn add(&self, member: WeechatRoomMember) {
        {
            let buffer = self.buffer();
            let nick = member.nick.borrow();
            let nick_settings = NickSettings::new(&nick);

            if let Err(_) = buffer.add_nick(nick_settings) {
                error!("Error adding nick {}, already addded.", nick);
            };
        }

        self.inner
            .borrow_mut()
            .insert((&*member.user_id).clone(), member);
    }

    /// Remove a Weechat room member by user ID.
    ///
    /// Returns either the removed Weechat room member, or an error if the
    /// member does not exist.
    fn remove(&self, user_id: &UserId) -> Result<WeechatRoomMember, RoomError> {
        let buffer = self.buffer();

        if let Some(member) = self.inner.borrow_mut().remove(user_id) {
            buffer.remove_nick(&member.nick.borrow());
            Ok(member)
        } else {
            error!(
                "{}: Tried removing a non-existent Weechat room member: {}",
                buffer.name(),
                user_id
            );

            Err(RoomError::NonExistentMember(user_id.clone()))
        }
    }

    /// Retrieve a reference to a Weechat room member by user ID.
    pub fn get(&self, user_id: &UserId) -> Option<WeechatRoomMember> {
        self.inner.borrow().get(user_id).cloned()
    }

    /// Change nick of member.
    ///
    /// Returns either the old nick of the member, or an error if the member
    /// does not exist.
    fn rename_member(
        &self,
        user_id: &UserId,
        new_nick: String,
    ) -> Result<String, RoomError> {
        let buffer = self.buffer();

        if let Some(member) = self.inner.borrow().get(user_id) {
            trace!(
                "Renaming member from {} to {}",
                &member.nick.borrow(),
                &new_nick
            );

            buffer.remove_nick(&member.nick.borrow());

            let nick_settings = NickSettings::new(&new_nick);
            buffer
                .add_nick(nick_settings)
                .expect("Can't add nick to nicklist");

            let old_nick = member.nick.replace(new_nick);

            Ok(old_nick)
        } else {
            Err(RoomError::NonExistentMember(user_id.clone()))
        }
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
    ///
    /// # Panics
    ///
    /// This panics if no member with the given user id can be found.
    fn calculate_user_name(&self, user_id: &UserId) -> String {
        self.room()
            .get_member(user_id)
            .unwrap_or_else(|| {
                panic!(
                    "No such member {} in {}",
                    user_id,
                    self.room.room_id().as_str()
                )
            })
            .disambiguated_name()
    }

    /// Process disambiguations received from the SDK.
    ///
    /// Disambiguations are a hashmap of user ID -> bool indicating that this
    /// user is either newly ambiguous (true) or no longer ambiguous (false).
    #[allow(dead_code)]
    fn process_disambiguations(
        &mut self,
        disambiguations: &HashMap<UserId, bool>,
    ) {
        for (affected_member, is_ambiguous) in disambiguations.iter() {
            if *is_ambiguous {
                let new_nick = self
                    .room()
                    .get_member(affected_member)
                    .unwrap()
                    .unique_name();

                match self.rename_member(affected_member, new_nick.clone()) {
                    Ok(old_nick) => debug!(
                        "{}: Disambiguating nick: {} -> {}",
                        self.calculate_buffer_name(),
                        old_nick,
                        &new_nick
                    ),
                    Err(RoomError::NonExistentMember(user_id)) => error!(
                        "{}: Tried disambiguating {} but they are not a member",
                        self.calculate_buffer_name(),
                        user_id
                    ),
                }
            } else {
                let new_nick =
                    self.room().get_member(affected_member).unwrap().name();

                match self.rename_member(affected_member, new_nick.clone()) {
                        Ok(old_nick) => debug!(
                            "{}: No longer disambiguating: {} -> {}",
                            self.calculate_buffer_name(),
                            old_nick,
                            &new_nick),
                        Err(RoomError::NonExistentMember(user_id)) => error!(
                            "{}: Tried removing disambiguation for {} but they are not a member",
                            self.calculate_buffer_name(),
                            user_id),
                    }
            }
        }
    }

    pub fn handle_membership_event(
        &self,
        event: &SyncStateEvent<MemberEventContent>,
        state_event: bool,
    ) {
        let buffer = self.buffer();

        let sender_id = event.sender.clone();
        let target_id;

        if let Ok(t) = UserId::try_from(event.state_key.clone()) {
            target_id = t;
        } else {
            error!(
                "Invalid state key given by the server: {}",
                event.state_key
            );
            return;
        }

        let new_nick = self.calculate_user_name(&target_id);

        if state_event {
            use MembershipState::*;

            // FIXME: Handle gaps (e.g. long disconnects) properly.
            //
            // For joins and invites, first we need to check whether a member
            // with some MXID exists. If he does, we have to update *that*
            // member with the new state. Only if they do not exist yet do we
            // create a new one.
            //
            // For leaves and bans we just need to remove the member.
            match event.content.membership {
                Invite | Join => {
                    // TODO remove this unwrap.
                    let display_name = self
                        .room()
                        .get_member(&target_id)
                        .unwrap()
                        .display_name()
                        .clone();

                    self.add(WeechatRoomMember::new(
                        &target_id,
                        new_nick,
                        display_name,
                    ));
                }
                Leave | Ban => {
                    let _ = self.remove(&target_id);
                }
                _ => (),
            }

            // TODO enable this again once we receive the event via an event
            // emitter.
            // self.process_disambiguations(&disambiguations);

            // Names of rooms without display names can get affected by the
            // member list so we need to update them.
            self.update_buffer_name();
        } else {
            let change_op = event.membership_change();
            let sender;
            let target;

            match change_op {
                Joined | Invited => {
                    debug!(
                        "{}: User {} joining, adding nick {}",
                        self.calculate_buffer_name(),
                        target_id,
                        new_nick
                    );

                    // TODO remove this unwrap
                    let display_name = self
                        .room()
                        .get_member(&target_id)
                        .unwrap()
                        .display_name()
                        .clone();

                    let member = WeechatRoomMember::new(
                        &target_id,
                        new_nick,
                        display_name,
                    );
                    self.add(member.clone());

                    sender = self.get(&sender_id);
                    target = Some(member);
                }

                Left | Banned | Kicked | KickedAndBanned
                | InvitationRejected | InvitationRevoked => {
                    sender = self.get(&sender_id);
                    target = self.get(&target_id);

                    match self.remove(&target_id) {
                        Ok(removed_member) => {
                            debug!(
                                "{}: User {} leaving, removing nick {}",
                                self.calculate_buffer_name(),
                                target_id,
                                removed_member.nick.borrow(),
                            );
                        }

                        Err(RoomError::NonExistentMember(user_id)) => {
                            error!(
                                "{}: User {} leaving, but he's not a member",
                                self.calculate_buffer_name(),
                                user_id
                            );
                        }
                    }
                }

                ProfileChanged {
                    displayname_changed,
                    avatar_url_changed,
                } => {
                    sender = self.get(&sender_id);
                    target = self.get(&target_id);

                    if displayname_changed {
                        match self.rename_member(&target_id, new_nick.clone()) {
                            Ok(old_nick) => debug!(
                                "{}: Profile changed for {}, renaming {} -> {}",
                                self.calculate_buffer_name(),
                                &target_id,
                                old_nick,
                                &new_nick
                            ),

                            Err(RoomError::NonExistentMember(user_id)) => error!(
                                "{}: Profile changed for {} but they are not a member",
                                self.calculate_buffer_name(),
                                user_id
                            ),
                        }

                        // TODO remove this unwrap
                        self.get(&target_id)
                            .unwrap()
                            .display_name
                            .replace(event.content.displayname.clone());
                    }

                    if avatar_url_changed {
                        debug!(
                            "{}: Avatar changed for {}, new avatar {:#?}",
                            self.calculate_buffer_name(),
                            &target_id,
                            event.content.avatar_url
                        );
                    }
                }
                _ => {
                    sender = self.get(&sender_id);
                    target = self.get(&target_id);
                }
            };

            // TODO enable this again once we receive the event via an event
            // emitter.
            // self.process_disambiguations(&disambiguations);

            // Names of rooms without display names can get affected by the member list so we need to
            // update them.
            self.update_buffer_name();

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
    ) -> Self {
        let color = Weechat::info_get("nick_color_name", user_id.as_str())
            .expect("Couldn't get the nick color name");

        WeechatRoomMember {
            user_id: Rc::new(user_id.clone()),
            nick: Rc::new(RefCell::new(nick)),
            display_name: Rc::new(RefCell::new(display_name)),
            prefix: Rc::new(RefCell::new(None)),
            color: Rc::new(color),
        }
    }
}

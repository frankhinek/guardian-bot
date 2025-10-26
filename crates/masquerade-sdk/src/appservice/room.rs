use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::{Arc, Weak};

use async_stream::try_stream;
use futures::Stream;
use futures::future::try_join_all;
use matrix_sdk::ruma::events::AnySyncTimelineEvent;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{EventId, OwnedRoomId, OwnedUserId, RoomId, UserId};
use tokio::sync::RwLock;

use crate::appservice::ApplicationServiceInner;
use crate::appservice::handler::ApplicationServiceReference;
use crate::appservice::http_client::parse_response;
use crate::appservice::user::User;
use crate::{Error, JoinedMembersResponse, MessagesResponse, Result};

pub enum Direction {
    Forward,
    Backward,
}

impl Direction {
    fn as_str(&self) -> &'static str {
        match self {
            Direction::Forward => "f",
            Direction::Backward => "b",
        }
    }
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub struct Room {
    inner: Arc<RoomKind>,
    appservice: Weak<ApplicationServiceInner>,
}

impl ApplicationServiceReference for Room {
    fn appservice(&self) -> Result<Arc<ApplicationServiceInner>> {
        match self.appservice.upgrade() {
            Some(handler) => Ok(handler),
            None => Err(Error::UpgradeError(format!("Room {} has no parent application service", self.id()))),
        }
    }
}

impl Room {
    pub async fn from_homeserver(appservice: &Arc<ApplicationServiceInner>, room_id: OwnedRoomId) -> Result<Arc<Self>> {
        let (is_encrypted, joined_members) = tokio::try_join!(
            Room::get_encryption(Arc::clone(&appservice), &room_id),
            Room::get_joined_members(Arc::clone(&appservice), &room_id),
        )?;

        let room_info = RoomInfo { room_id, joined_members: RwLock::new(HashSet::from_iter(joined_members)) };

        let inner = match is_encrypted {
            true => RoomKind::Encrypted(room_info),
            false => RoomKind::Unencrypted(room_info),
        };

        let room = Self { inner: Arc::new(inner), appservice: Arc::downgrade(appservice) };

        Ok(Arc::new(room))
    }

    pub fn id(&self) -> &RoomId {
        &self.inner.id()
    }

    pub fn kind(&self) -> Arc<RoomKind> {
        Arc::clone(&self.inner)
    }

    pub async fn is_direct(&self) -> bool {
        self.inner.is_direct().await
    }

    pub async fn is_encrypted(&self) -> bool {
        match *self.kind() {
            RoomKind::Encrypted(_) => true,
            RoomKind::Unencrypted(_) => false,
        }
    }

    pub async fn joined_members(&self) -> HashSet<OwnedUserId> {
        self.inner.joined_members().await
    }

    pub async fn get_event(&self, event_id: &EventId) -> Result<AnySyncTimelineEvent> {
        let url = format!("/_matrix/client/v3/rooms/{}/event/{}", self.id(), event_id);
        let response = self.client()?.get(url).send().await?;
        let event = parse_response(response).await?;
        Ok(event)
    }

    pub async fn get_raw_event(&self, event_id: &EventId) -> Result<Raw<AnySyncTimelineEvent>> {
        let url = format!("/_matrix/client/v3/rooms/{}/event/{}", self.id(), event_id);
        let response = self.client()?.get(url).send().await?;
        let event = parse_response(response).await?;
        Ok(event)
    }

    pub async fn get_raw_messages(&self, direction: Direction) -> Result<Vec<Raw<AnySyncTimelineEvent>>> {
        let url = format!("/_matrix/client/v3/rooms/{}/messages", self.id());
        let mut messages = Vec::new();
        let mut next_token = None;

        loop {
            let mut params = vec![("dir".to_string(), direction.to_string())];
            if let Some(token) = next_token {
                params.push(("from".to_string(), token));
            }

            let response = self.client()?.get(&url).query(&params).send().await?;
            let response: MessagesResponse = response.json().await?;

            let chunk = response.chunk.iter().cloned().collect::<Vec<_>>();

            messages.extend(chunk);

            match response.end {
                Some(token) => next_token = Some(token),
                None => break,
            }
        }

        Ok(messages)
    }

    pub fn get_raw_message_stream(
        &self,
        direction: Direction,
    ) -> Pin<Box<dyn Stream<Item = Result<Raw<AnySyncTimelineEvent>>> + Send + '_>> {
        Box::pin(try_stream! {
            let url = format!("/_matrix/client/v3/rooms/{}/messages", self.id());
            let mut next_token = None;

            loop {
                let mut params = vec![("dir".to_string(), direction.to_string())];
                if let Some(token) = next_token {
                    params.push(("from".to_owned(), token));
                }

                let response = self.client()?.get(&url).query(&params).send().await?;
                let response: MessagesResponse = response.json().await?;

                for message in response.chunk.into_iter() {
                    yield message;
                }

                match response.end {
                    Some(token) => next_token = Some(token),
                    None => break,
                }
            }
        })
    }

    pub async fn get_appservice_users(&self) -> Result<Vec<Arc<User>>> {
        let appservice = self.appservice()?;

        let joined_members = self.inner.joined_members().await;
        let user_ids = appservice.user_store().keys().await;

        let futures = user_ids
            .intersection(&joined_members)
            .map(async |mxid| appservice.user_store().get(mxid).await.ok_or(Error::UserNotFound(mxid.to_owned())));

        let users = try_join_all(futures).await?;
        Ok(users)
    }

    async fn get_encryption(appservice: Arc<ApplicationServiceInner>, room_id: &RoomId) -> Result<bool> {
        let url = format!("/_matrix/client/v3/rooms/{}/state/m.room.encryption", room_id);
        let response = appservice.client().get(url).send().await?;

        match response.error_for_status() {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    async fn get_joined_members(
        appservice: Arc<ApplicationServiceInner>,
        room_id: &RoomId,
    ) -> Result<HashSet<OwnedUserId>> {
        let url = format!("/_matrix/client/v3/rooms/{}/joined_members", room_id);
        let response = appservice.client().get(url).send().await?;
        let json: JoinedMembersResponse = parse_response(response).await?;
        let members = json.joined.keys().map(OwnedUserId::to_owned).collect::<HashSet<_>>();
        Ok(members)
    }
}

#[derive(Debug)]
pub struct RoomInfo {
    room_id: OwnedRoomId,
    joined_members: RwLock<HashSet<OwnedUserId>>,
}

impl RoomInfo {
    pub fn id(&self) -> &RoomId {
        &self.room_id
    }

    pub async fn contains(&self, mxid: &UserId) -> bool {
        self.joined_members.read().await.contains(mxid)
    }

    pub async fn joined_members(&self) -> HashSet<OwnedUserId> {
        self.joined_members.read().await.clone()
    }

    pub async fn is_direct(&self) -> bool {
        self.joined_members.read().await.len() == 2
    }

    pub(crate) async fn add_member(&self, joined_member: OwnedUserId) -> bool {
        self.joined_members.write().await.insert(joined_member)
    }

    pub(crate) async fn remove_member(&self, left_member: &UserId) -> bool {
        self.joined_members.write().await.remove(left_member)
    }
}

#[derive(Debug)]
pub enum RoomKind {
    Encrypted(RoomInfo),
    Unencrypted(RoomInfo),
}

impl RoomKind {
    pub fn new_encrypted(room_id: OwnedRoomId, joined_members: impl Into<HashSet<OwnedUserId>>) -> Arc<Self> {
        Arc::new(RoomKind::Encrypted(RoomInfo { room_id, joined_members: RwLock::new(joined_members.into()) }))
    }

    pub fn new_unencrypted(room_id: OwnedRoomId, joined_members: impl Into<HashSet<OwnedUserId>>) -> Arc<Self> {
        Arc::new(RoomKind::Unencrypted(RoomInfo { room_id, joined_members: RwLock::new(joined_members.into()) }))
    }

    fn upgrade(self: &Arc<Self>, appservice: Weak<ApplicationServiceInner>) -> Arc<Room> {
        Arc::new(Room { appservice, inner: Arc::clone(self) })
    }

    pub fn is_encrypted(&self) -> bool {
        matches!(self, RoomKind::Encrypted(_))
    }

    pub async fn is_direct(&self) -> bool {
        match self {
            RoomKind::Encrypted(room_info) | RoomKind::Unencrypted(room_info) => room_info.is_direct().await,
        }
    }

    pub async fn joined_members(&self) -> HashSet<OwnedUserId> {
        match self {
            RoomKind::Encrypted(room_info) | RoomKind::Unencrypted(room_info) => room_info.joined_members().await,
        }
    }

    pub async fn add_member(&self, joined_member: OwnedUserId) -> bool {
        match self {
            RoomKind::Encrypted(room_info) | RoomKind::Unencrypted(room_info) => {
                room_info.add_member(joined_member).await
            }
        }
    }

    pub async fn remove_member(&self, left_member: &UserId) -> bool {
        match self {
            RoomKind::Encrypted(room_info) | RoomKind::Unencrypted(room_info) => {
                room_info.remove_member(left_member).await
            }
        }
    }

    pub fn message_type(&self) -> &str {
        match self {
            RoomKind::Encrypted(_) => "m.room.encrypted",
            RoomKind::Unencrypted(_) => "m.room.message",
        }
    }

    pub fn id(&self) -> &RoomId {
        match self {
            RoomKind::Encrypted(room_info) | RoomKind::Unencrypted(room_info) => room_info.id(),
        }
    }
}

#[derive(Debug)]
pub struct RoomStore {
    appservice: Weak<ApplicationServiceInner>,
    rooms: RwLock<HashMap<OwnedRoomId, Arc<RoomKind>>>,
}

impl ApplicationServiceReference for RoomStore {
    fn appservice(&self) -> Result<Arc<ApplicationServiceInner>> {
        match self.appservice.upgrade() {
            Some(handler) => Ok(handler),
            None => Err(Error::UpgradeError("Room store has no parent application service".to_string())),
        }
    }
}

impl RoomStore {
    pub fn new(appservice: Weak<ApplicationServiceInner>) -> Self {
        Self { appservice, rooms: RwLock::new(HashMap::new()) }
    }

    pub async fn get(&self, room_id: &RoomId) -> Option<Arc<Room>> {
        let rooms = self.rooms.read().await;
        match rooms.get(room_id) {
            Some(inner) => Some(inner.upgrade(Weak::clone(&self.appservice))),
            None => None,
        }
    }

    pub(crate) async fn get_encrypted_members(&self, user: &Arc<User>) -> HashSet<OwnedUserId> {
        let rooms = self.rooms.read().await;
        let mut accumulator = HashSet::from_iter([user.id().to_owned()]);

        for room in rooms.values() {
            if let RoomKind::Encrypted(room_info) = room.as_ref() {
                if room_info.contains(user.id()).await {
                    accumulator.extend(room_info.joined_members().await);
                }
            }
        }

        accumulator
    }

    pub(crate) async fn upgrade_room_encryption(&self, room_id: &RoomId) -> Result<()> {
        let joined_members = {
            let rooms = self.rooms.read().await;
            let Some(room) = rooms.get(room_id) else {
                return Ok(());
            };

            let RoomKind::Unencrypted(room_info) = room.as_ref() else {
                return Ok(());
            };

            room_info.joined_members().await
        };

        let new_room = RoomKind::new_encrypted(room_id.to_owned(), joined_members);
        self.rooms.write().await.insert(room_id.to_owned(), new_room.clone());

        Ok(self.update_tracked_users(&new_room).await?)
    }

    pub(crate) async fn populate_known_rooms(&self, rooms: &[OwnedRoomId]) -> Result<()> {
        let mut known_rooms = self.rooms.write().await;
        let known_ids: HashSet<OwnedRoomId> = HashSet::from_iter(known_rooms.keys().cloned());
        let new_ids: HashSet<OwnedRoomId> = HashSet::from_iter(rooms.iter().cloned());

        for room_id in new_ids.difference(&known_ids) {
            let room = self.appservice()?.create_room(room_id.to_owned()).await?;
            known_rooms.insert(room_id.to_owned(), Arc::clone(&room.inner));
        }

        Ok(())
    }

    pub async fn add_room_member(&self, room_id: &RoomId, mxid: OwnedUserId) -> Result<()> {
        let room = {
            let mut rooms = self.rooms.write().await;
            match rooms.get(room_id) {
                Some(room) => {
                    room.add_member(mxid).await;
                    Arc::clone(room)
                }
                None => {
                    let room = self.appservice()?.create_room(room_id.to_owned()).await?;
                    rooms.insert(room_id.to_owned(), Arc::clone(&room.inner));
                    Arc::clone(&room.inner)
                }
            }
        };

        if !room.is_encrypted() {
            return Ok(());
        }

        Ok(self.update_tracked_users(&room).await?)
    }

    pub async fn remove_room_member(&self, room_id: &RoomId, mxid: &UserId) -> Result<()> {
        let room = {
            let rooms = self.rooms.write().await;
            match rooms.get(room_id) {
                Some(room) => {
                    room.remove_member(mxid).await;
                    if !room.is_encrypted() {
                        return Ok(());
                    }

                    Arc::clone(room)
                }
                None => return Ok(()),
            }
        };

        Ok(self.update_tracked_users(&room).await?)
    }

    async fn update_tracked_users(&self, room: &Arc<RoomKind>) -> Result<()> {
        let full_room = room.upgrade(Weak::clone(&self.appservice));
        let users = full_room.get_appservice_users().await?;

        for user in users {
            let encrypted_members = self.get_encrypted_members(&user).await;
            user.update_tracked_users(&encrypted_members).await?;
        }

        Ok(())
    }
}

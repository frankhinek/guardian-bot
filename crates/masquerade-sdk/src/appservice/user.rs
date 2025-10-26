use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};

use matrix_sdk::ruma::exports::serde_json::json;
use matrix_sdk::ruma::presence::PresenceState;
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId, RoomId, UserId};
use tokio::sync::RwLock;

use crate::appservice::device::{Device, DeviceInner};
use crate::appservice::error::Error;
use crate::appservice::handler::ApplicationServiceReference;
use crate::appservice::http_client::{discard_response, parse_response};
use crate::appservice::types::{JoinedRoomResponse, Profile};
use crate::appservice::{ApplicationServiceInner, Presence};
use crate::{Empty, Result};

#[derive(Debug)]
pub struct UserInner {
    mxid: OwnedUserId,
    device: RwLock<Option<Arc<DeviceInner>>>,
}

impl UserInner {
    async fn new(mxid: OwnedUserId) -> Arc<Self> {
        Arc::new(UserInner { mxid, device: RwLock::new(None) })
    }

    fn upgrade(self: &Arc<Self>, appservice: Weak<ApplicationServiceInner>) -> Arc<User> {
        Arc::new(User { appservice, inner: Arc::clone(self) })
    }
}

pub struct User {
    inner: Arc<UserInner>,
    appservice: Weak<ApplicationServiceInner>,
}

impl ApplicationServiceReference for User {
    fn appservice(&self) -> Result<Arc<ApplicationServiceInner>> {
        match self.appservice.upgrade() {
            Some(appservice) => Ok(appservice),
            None => Err(Error::UpgradeError(format!("User {} has no parent application service", self.id()))),
        }
    }
}

impl User {
    pub async fn new(appservice: &Arc<ApplicationServiceInner>, mxid: OwnedUserId) -> Arc<Self> {
        let inner = UserInner::new(mxid).await;
        appservice.user_store().insert(Arc::clone(&inner)).await;

        Arc::new(Self { appservice: Arc::downgrade(appservice), inner })
    }

    pub fn id(&self) -> &UserId {
        &self.inner.mxid
    }

    pub async fn get_device(self: &Arc<Self>) -> Option<Arc<Device>> {
        let inner_option = self.inner.device.read().await;
        match inner_option.as_ref() {
            Some(inner) => Some(inner.upgrade(self)),
            None => None,
        }
    }

    pub async fn create_device(self: &Arc<Self>, device_id: Option<&str>) -> Result<Arc<Device>> {
        let inner = DeviceInner::new(self, device_id).await?;
        let mut lock = self.inner.device.write().await;
        *lock = Some(Arc::clone(&inner));

        Ok(inner.upgrade(self))
    }

    pub(crate) async fn populate_known_rooms(&self) -> Result<()> {
        let joined_rooms = self.get_joined_rooms().await?;
        self.appservice()?.room_store().populate_known_rooms(&joined_rooms).await?;

        Ok(())
    }

    pub(crate) async fn update_tracked_users(self: &Arc<Self>, users: &HashSet<OwnedUserId>) -> Result<()> {
        match self.get_device().await {
            Some(device) => device.encryption().update_tracked_users(users).await,
            None => Ok(()),
        }
    }

    pub async fn get_joined_rooms(&self) -> Result<Vec<OwnedRoomId>> {
        tracing::info!("Retrieving room membership of {}", &self.id());
        let url = "/_matrix/client/v3/joined_rooms";
        let response = self.client()?.get(url).query(&[("user_id", self.id().to_owned())]).send().await?;

        let json: JoinedRoomResponse = parse_response(response).await?;
        Ok(json.joined_rooms)
    }

    pub async fn register(&self) -> Result<Empty> {
        tracing::info!("Registering user {}", self.id());
        let url = "/_matrix/client/v3/register";
        let body = json!({
            "type": "m.login.application_service",
            "inhibit_login": true,
            "username": self.id().localpart(),
        });

        let response = self.client()?.post(url).json(&body).send().await?;
        parse_response(response).await
    }

    pub async fn join_room(&self, room_id: &RoomId) -> Result<()> {
        let url = format!("/_matrix/client/v3/rooms/{}/join", room_id.as_str());
        let response = self.client()?.post(&url).query(&[("user_id", self.id().to_owned())]).send().await?;

        discard_response(response).await
    }

    pub async fn get_devices(&self) -> Result<Vec<matrix_sdk::ruma::api::client::device::Device>> {
        tracing::info!("Fetching devices of user {}", self.id());
        let url = "/_matrix/client/v3/devices";
        let response = self.client()?.get(url).send().await?;

        parse_response(response).await
    }

    pub async fn get_profile(&self) -> Result<Profile> {
        tracing::info!("Fetching profile of user {}", self.id());
        let url = format!("/_matrix/client/v3/profile/{}", self.id());
        let response = self.client()?.get(&url).send().await?;
        parse_response(response).await
    }

    pub async fn set_displayname(&self, displayname: &str) -> Result<Empty> {
        tracing::info!("Updating display name of {} to {}", self.id(), displayname);
        let url = format!("/_matrix/client/v3/profile/{}/displayname", self.id());
        let body = json!({"displayname": displayname});
        let response = self.client()?.put(&url).json(&body).send().await?;

        parse_response(response).await
    }

    pub async fn set_presence(&self, state: PresenceState, message: Option<String>) -> Result<Empty> {
        tracing::debug!("Updating presence for {}", self.id());
        let url = format!("/_matrix/client/v3/presence/{}/status", self.id());
        let body = Presence { presence: state, status_msg: message };
        let response = self.client()?.put(&url).json(&body).send().await?;
        parse_response(response).await
    }
}

#[derive(Debug)]
pub struct UserStore {
    appservice: Weak<ApplicationServiceInner>,
    users: RwLock<HashMap<OwnedUserId, Arc<UserInner>>>,
}

impl ApplicationServiceReference for UserStore {
    fn appservice(&self) -> Result<Arc<ApplicationServiceInner>> {
        match self.appservice.upgrade() {
            Some(handler) => Ok(handler),
            None => Err(Error::UpgradeError("Room store has no parent application service".to_string())),
        }
    }
}

impl UserStore {
    pub fn new(appservice: Weak<ApplicationServiceInner>) -> Self {
        Self { appservice, users: RwLock::new(HashMap::new()) }
    }

    pub async fn insert(&self, user: Arc<UserInner>) {
        let mut users = self.users.write().await;
        users.insert(user.mxid.to_owned(), user);
    }

    pub async fn get(&self, mxid: &UserId) -> Option<Arc<User>> {
        let users = self.users.read().await;
        match users.get(mxid) {
            Some(inner) => Some(inner.upgrade(Weak::clone(&self.appservice))),
            None => None,
        }
    }

    pub async fn keys(&self) -> HashSet<OwnedUserId> {
        self.users.read().await.keys().cloned().collect()
    }
}

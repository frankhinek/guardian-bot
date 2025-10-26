use std::sync::{Arc, Weak};
use std::time::Duration;

use matrix_sdk::crypto::types::events::room::encrypted::EncryptedEvent;
use matrix_sdk::deserialized_responses::DecryptedRoomEvent;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::ruma::exports::serde_json::json;
use matrix_sdk::ruma::presence::PresenceState;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{DeviceId, EventId, OwnedDeviceId, OwnedEventId, RoomId, TransactionId};
use tokio::sync::Mutex;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;

use crate::appservice::ApplicationServiceInner;
use crate::appservice::encryption::{Encryption, EncryptionInner, OwnedEncryptionSyncChanges};
use crate::appservice::error::Error;
use crate::appservice::handler::ApplicationServiceReference;
use crate::appservice::http_client::parse_response;
use crate::appservice::room::RoomKind;
use crate::appservice::types::CreateDeviceRequest;
use crate::appservice::user::User;
use crate::{Empty, Result, SendResponse};

#[derive(Debug)]
pub struct DeviceInner {
    device_id: OwnedDeviceId,
    encryption: Arc<EncryptionInner>,
    token: Mutex<Option<CancellationToken>>,
    sender: Sender<OwnedEncryptionSyncChanges>,
    receiver: Mutex<Option<Receiver<OwnedEncryptionSyncChanges>>>,
}

impl DeviceInner {
    pub async fn new(user: &User, device_id: Option<&str>) -> Result<Arc<DeviceInner>> {
        let device_id = match device_id {
            Some(device_id) => device_id,
            None => &uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, user.id().as_bytes()).to_string(),
        };
        let device_id = OwnedDeviceId::from(device_id);

        let (sender, receiver) = tokio::sync::mpsc::channel(50);
        let inner = Arc::new(Self {
            device_id: device_id.clone(),
            encryption: EncryptionInner::new(user, &device_id).await?,
            token: Mutex::new(None),
            sender,
            receiver: Mutex::new(Some(receiver)),
        });

        Ok(inner)
    }

    pub(crate) fn upgrade(self: &Arc<Self>, user: &Arc<User>) -> Arc<Device> {
        Arc::new(Device { user: Arc::downgrade(user), inner: Arc::clone(self) })
    }
}

pub struct Device {
    inner: Arc<DeviceInner>,
    user: Weak<User>,
}

impl ApplicationServiceReference for Device {
    fn appservice(&self) -> Result<Arc<ApplicationServiceInner>> {
        self.user()?.appservice()
    }
}

impl Device {
    pub fn id(&self) -> &DeviceId {
        &self.inner.device_id
    }

    pub fn user(&self) -> Result<Arc<User>> {
        match self.user.upgrade() {
            Some(user) => Ok(user),
            None => Err(Error::UpgradeError(format!("Device {} has no parent user", self.id()))),
        }
    }

    pub(crate) fn encryption(self: &Arc<Self>) -> Encryption {
        self.inner.encryption.upgrade(self)
    }

    pub(crate) fn token(self: &Arc<Self>) -> &Mutex<Option<CancellationToken>> {
        &self.inner.token
    }

    pub async fn is_running(&self) -> bool {
        self.inner.token.lock().await.is_some()
    }

    pub async fn register(&self, displayname: Option<String>) -> Result<Empty> {
        let user = self.user()?;
        tracing::info!("Registering device {} for user {}", &self.id(), user.id());

        let url = format!("/_matrix/client/v3/devices/{}", self.id());
        let body = CreateDeviceRequest { display_name: displayname };
        let response = self.client()?.put(&url).query(&[("user_id", user.id())]).json(&body).send().await?;

        parse_response(response).await
    }

    pub(crate) async fn send_sync_changes(&self, changes: OwnedEncryptionSyncChanges) -> Result<()> {
        Ok(self.inner.sender.send(changes).await?)
    }

    async fn take_receiver(&self) -> Result<Receiver<OwnedEncryptionSyncChanges>> {
        let mut lock = self.inner.receiver.lock().await;
        lock.take().ok_or(Error::MultipleSync(format!(
            "Running multiple sync loops for device {}. This is not allowed",
            self.id()
        )))
    }

    async fn return_receiver(&self, receiver: Receiver<OwnedEncryptionSyncChanges>) {
        let mut lock = self.inner.receiver.lock().await;
        *lock = Some(receiver)
    }

    pub async fn run(self: &Arc<Self>) -> Result<()> {
        let user = self.user()?;
        tracing::info!("Running sync loop for {} with device {}...", user.id(), self.id());

        user.populate_known_rooms().await?;
        let tracked_users = user.appservice()?.room_store().get_encrypted_members(&user).await;
        self.encryption().update_tracked_users(&tracked_users).await?;

        let mut sleep_interval = tokio::time::interval(Duration::from_secs(2));
        let mut presence_interval = tokio::time::interval(Duration::from_secs(30));

        let token = CancellationToken::new();
        {
            let mut lock = self.token().lock().await;
            *lock = Some(token.clone());
        }

        let device = Arc::clone(self);
        let mut receiver = self.take_receiver().await?;
        let result = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        tracing::info!("Sync loop terminated for device {}", device.id());
                        device.return_receiver(receiver).await;

                        break
                    },
                    Some(changes) = receiver.recv() => {
                        if let Err(error) = device.encryption().sync(changes.as_ref()).await {
                            tracing::error!("Failed to sync changes for device {}: {}", device.id(), error);
                        }
                    },
                    _ = sleep_interval.tick() => {
                        if let Err(error) = device.encryption().send_outgoing_requests().await {
                            tracing::error!("Failed to send outgoing requests for device {}: {}", device.id(), error)
                        }
                    },
                    _ = presence_interval.tick() => {
                        if let Err(error) = user.set_presence(PresenceState::Online, None).await {
                            tracing::warn!("Failed to set presence for device {}: {}", device.id(), error);
                        }
                    },
                }
            }
        })
        .await;
        Ok(result?)
    }

    pub async fn stop(self: &Arc<Self>) -> Result<()> {
        let mut lock = self.token().lock().await;
        match lock.as_ref() {
            Some(token) => {
                token.cancel();
                *lock = None;
            }
            None => (),
        }

        Ok(())
    }

    pub async fn decrypt_event(
        self: &Arc<Self>,
        event: Raw<EncryptedEvent>,
        room_id: &RoomId,
    ) -> Result<DecryptedRoomEvent> {
        Ok(self.encryption().decrypt_event(event, room_id).await?)
    }

    pub async fn send_receipt(&self, room_id: &RoomId, event_id: &EventId) -> Result<Empty> {
        let user = self.user()?;
        let url = format!("/_matrix/client/v3/rooms/{}/receipt/m.read/{}", room_id, event_id);

        let response = self.client()?.post(url).json(&json!({})).query(&[("user_id", user.id())]).send().await?;

        parse_response(response).await
    }

    pub async fn send_typing(&self, room_id: &RoomId, is_typing: bool) -> Result<Empty> {
        let user = self.user()?;
        let url = format!("/_matrix/client/v3/rooms/{}/typing/{}", room_id, user.id());

        let body = json!({
            "typing": is_typing,
            "timeout": 30000
        });

        let response = self.client()?.put(url).json(&body).query(&[("user_id", user.id())]).send().await?;

        parse_response(response).await
    }

    pub async fn send_message(
        self: &Arc<Self>,
        room_id: &RoomId,
        content: RoomMessageEventContent,
    ) -> Result<OwnedEventId> {
        let appservice = self.user()?.appservice()?;
        let room = match appservice.get_room(room_id).await {
            Some(room) => room.kind(),
            None => return Err(Error::RoomNotFound(room_id.to_owned())),
        };

        let encoded_room_id: String = url::form_urlencoded::byte_serialize(room_id.as_bytes()).collect();
        let txn_id = TransactionId::new().to_string();

        let payload = match room.as_ref() {
            RoomKind::Encrypted(_) => {
                let encrypted = self.encryption().encrypt_event(room_id, content).await?;
                serde_json::to_value(encrypted)?
            }
            RoomKind::Unencrypted(_) => serde_json::to_value(content)?,
        };

        let url = format!(" /_matrix/client/v3/rooms/{}/send/{}/{}", encoded_room_id, room.message_type(), txn_id);

        let response = self.client()?.put(&url).json(&payload).send().await?;
        let send_response = parse_response::<SendResponse>(response).await?;
        Ok(send_response.event_id)
    }
}

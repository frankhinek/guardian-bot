use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Weak};

use bytes::Bytes;
use matrix_sdk::SqliteCryptoStore;
use matrix_sdk::crypto::types::events::room::encrypted::{EncryptedEvent, RoomEncryptedEventContent};
use matrix_sdk::crypto::types::requests::{AnyOutgoingRequest, OutgoingRequest};
use matrix_sdk::crypto::{DecryptionSettings, EncryptionSettings, EncryptionSyncChanges, OlmMachine, TrustRequirement};
use matrix_sdk::deserialized_responses::DecryptedRoomEvent;
use matrix_sdk::ruma::api::client::keys::claim_keys::v3::Response as RumaKeysClaimResponse;
use matrix_sdk::ruma::api::client::keys::get_keys::v3::{
    Request as RumaKeysQueryRequest,
    Response as RumaKeysQueryResponse,
};
use matrix_sdk::ruma::api::client::keys::upload_keys::v3::Response as RumaKeysUploadResponse;
use matrix_sdk::ruma::api::client::keys::upload_signatures::v3::Response as RumaUploadSignaturesResponse;
use matrix_sdk::ruma::api::client::message::send_message_event::v3::{
    Request as RumaSendMessageRequest,
    Response as RumaSendMessageResponse,
};
use matrix_sdk::ruma::api::client::sync::sync_events::DeviceLists;
use matrix_sdk::ruma::api::client::to_device::send_event_to_device::v3::{
    Request as RumaToDeviceRequest,
    Response as RumaToDeviceResponse,
};
use matrix_sdk::ruma::api::{IncomingResponse, MatrixVersion, SendAccessToken};
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::ruma::events::{AnyToDeviceEvent, EventContent};
use matrix_sdk::ruma::exports::serde_json::json;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{OneTimeKeyAlgorithm, OwnedDeviceId, OwnedUserId, RoomId, UInt, assign};

use crate::appservice::ApplicationServiceInner;
use crate::appservice::device::Device;
use crate::appservice::handler::ApplicationServiceReference;
use crate::appservice::room::RoomKind;
use crate::appservice::user::User;
use crate::{Error, Result};

pub struct OwnedEncryptionSyncChanges {
    pub to_device_events: Vec<Raw<AnyToDeviceEvent>>,
    pub changed_devices: DeviceLists,
    pub one_time_keys_counts: BTreeMap<OneTimeKeyAlgorithm, UInt>,
    pub unused_fallback_keys: Vec<OneTimeKeyAlgorithm>,
    pub next_batch_token: Option<String>,
}

impl OwnedEncryptionSyncChanges {
    pub fn as_ref(&self) -> EncryptionSyncChanges<'_> {
        EncryptionSyncChanges {
            to_device_events: self.to_device_events.clone(),
            changed_devices: &self.changed_devices,
            one_time_keys_counts: &self.one_time_keys_counts,
            unused_fallback_keys: if !self.unused_fallback_keys.is_empty() {
                Some(&self.unused_fallback_keys)
            } else {
                None
            },
            next_batch_token: self.next_batch_token.clone(),
        }
    }
}

pub struct Encryption {
    inner: Arc<EncryptionInner>,
    device: Weak<Device>,
}

#[derive(Debug)]
pub struct EncryptionInner {
    olm: OlmMachine,
}

impl EncryptionInner {
    pub async fn new(user: &User, device_id: &OwnedDeviceId) -> Result<Arc<Self>> {
        let db_path = Path::new(&user.appservice()?.config().database.path).join(format!("{}.db", device_id,));
        let store = SqliteCryptoStore::open(&db_path, Some(&user.appservice()?.config().database.passphrase)).await?;
        let olm = OlmMachine::with_store(user.id(), device_id, store, None).await?;

        Ok(Arc::new(Self { olm }))
    }

    pub(crate) fn upgrade(self: &Arc<Self>, device: &Arc<Device>) -> Encryption {
        Encryption { device: Arc::downgrade(device), inner: Arc::clone(self) }
    }
}

impl ApplicationServiceReference for Encryption {
    fn appservice(&self) -> Result<Arc<ApplicationServiceInner>> {
        self.device()?.user()?.appservice()
    }
}

impl Encryption {
    pub fn olm(&self) -> &OlmMachine {
        &self.inner.olm
    }

    pub fn device(&self) -> Result<Arc<Device>> {
        match self.device.upgrade() {
            Some(device) => Ok(device),
            None => Err(Error::UpgradeError(format!("Encryption has no parent device"))),
        }
    }

    pub async fn sync(&self, changes: EncryptionSyncChanges<'_>) -> Result<()> {
        self.olm().receive_sync_changes(changes).await?;
        Ok(())
    }

    pub async fn get_missing_session(&self, room_id: &RoomId) -> Result<()> {
        let members = self
            .appservice()?
            .get_room(room_id)
            .await
            .ok_or(Error::RoomNotFound(room_id.to_owned()))?
            .joined_members()
            .await;

        let mut tracked_members = self.olm().tracked_users().await?;
        if !members.is_subset(&tracked_members) {
            tracked_members.extend(members.clone());
            self.update_tracked_users(&members).await?;
        }

        let has_keys = self.olm().get_missing_sessions(members.iter().map(|u| u.as_ref())).await?;

        if let Some(key_claim_request) = has_keys {
            let (txn_id, http_request) = key_claim_request;
            let http_response = self.send(http_request).await?;
            let response = RumaKeysClaimResponse::try_from_http_response(http_response)?;
            self.olm().mark_request_as_sent(&txn_id, &response).await?;
        }

        Ok(())
    }

    pub async fn share_room_key(&self, room_id: &RoomId) -> Result<()> {
        let members = self
            .appservice()?
            .get_room(room_id)
            .await
            .ok_or(Error::RoomNotFound(room_id.to_owned()))?
            .joined_members()
            .await;

        let encryption_settings = EncryptionSettings::default();
        let to_device_requests =
            self.olm().share_room_key(room_id, members.iter().map(|u| u.as_ref()), encryption_settings).await?;

        for to_device_request in to_device_requests {
            let url = format!(
                "/_matrix/client/v3/sendToDevice/{}/{}",
                &to_device_request.event_type, &to_device_request.txn_id
            );
            let body = json!({"messages": &to_device_request.messages});
            let response: reqwest::Response = self
                .client()?
                .put(url)
                .query(&[
                    ("org.matrix.msc3202.device_id", self.device()?.id().as_str()),
                    ("user_id", self.device()?.user()?.id().as_str()),
                ])
                .json(&body)
                .send()
                .await?;

            if let Ok(response) = response.error_for_status() {
                let http_response = http::Response::builder().body(response.bytes().await?)?;
                let ruma_response = RumaToDeviceResponse::try_from_http_response(http_response)?;
                self.olm().mark_request_as_sent(&to_device_request.txn_id, &ruma_response).await?;
            }
        }

        Ok(())
    }

    pub async fn encrypt_event(
        &self,
        room_id: &RoomId,
        content: RoomMessageEventContent,
    ) -> Result<Raw<RoomEncryptedEventContent>> {
        let appservice = self.appservice()?;
        let room_kind = match appservice.get_room(room_id).await {
            Some(room) => room.kind(),
            None => return Err(Error::RoomNotFound(room_id.to_owned())),
        };

        if let RoomKind::Unencrypted(_) = room_kind.as_ref() {
            return Err(Error::RoomNotEncrypted(room_id.to_owned()));
        }

        self.get_missing_session(room_id).await?;
        self.share_room_key(room_id).await?;
        let encrypted = self.olm().encrypt_room_event(room_id, content).await?;

        Ok(encrypted)
    }

    pub async fn decrypt_event(&self, event: Raw<EncryptedEvent>, room_id: &RoomId) -> Result<DecryptedRoomEvent> {
        let decryption_settings = DecryptionSettings { sender_device_trust_requirement: TrustRequirement::Untrusted };

        Ok(self.olm().decrypt_room_event(&event.cast(), &room_id, &decryption_settings).await?)
    }

    pub async fn update_tracked_users(&self, users: &HashSet<OwnedUserId>) -> Result<()> {
        Ok(self.olm().update_tracked_users(users.iter().map(OwnedUserId::as_ref)).await?)
    }

    pub async fn send_outgoing_requests(&self) -> Result<()> {
        let outgoing_requests = self.olm().outgoing_requests().await?;
        for request in outgoing_requests {
            self.process_outgoing_request(request).await?;
        }

        Ok(())
    }

    pub async fn send<T>(&self, ruma_request: T) -> Result<http::Response<Bytes>>
    where
        T: matrix_sdk::ruma::api::OutgoingRequest,
    {
        // Ugh, I hate this, but ok
        let http_request = ruma_request.try_into_http_request::<Vec<u8>>(
            Default::default(),
            SendAccessToken::Appservice(&self.appservice()?.config().appservice.as_token),
            &[MatrixVersion::V1_14],
        )?;

        let request = self
            .client()?
            .request(http_request.method().clone(), http_request.uri().to_string())
            .query(&[
                ("org.matrix.msc3202.device_id", self.device()?.id().as_str()),
                ("user_id", self.device()?.user()?.id().as_str()),
            ])
            .headers(http_request.headers().clone())
            .body(http_request.body().clone())
            .send()
            .await?;

        let http_response = http::Response::builder().body(request.bytes().await?)?;
        Ok(http_response)
    }

    pub async fn process_outgoing_request(&self, outgoing_request: OutgoingRequest) -> Result<()> {
        match outgoing_request.request() {
            AnyOutgoingRequest::KeysQuery(request) => {
                tracing::info!("Device {} is requesting keys from the homeserver", &self.device()?.id());
                let device_keys = request.device_keys.clone();
                let keys_query_request = assign!(RumaKeysQueryRequest::new(), { device_keys });

                let http_response = self.send(keys_query_request).await?;
                let response = RumaKeysQueryResponse::try_from_http_response(http_response)?;
                self.olm().mark_request_as_sent(outgoing_request.request_id(), &response).await?;
                // TODO Verification status? No way to verify anyway.
            }
            AnyOutgoingRequest::KeysUpload(request) => {
                tracing::info!("Device {} is uploading keys to the homeserver", &self.device()?.id());
                let http_response = self.send(request.clone()).await?;
                let response = RumaKeysUploadResponse::try_from_http_response(http_response)?;
                self.olm().mark_request_as_sent(outgoing_request.request_id(), &response).await?;
            }
            AnyOutgoingRequest::ToDeviceRequest(request) => {
                tracing::info!("Device {} is sending a to device message", &self.device()?.id());
                let to_device_request = RumaToDeviceRequest::new_raw(
                    request.event_type.clone(),
                    request.txn_id.clone(),
                    request.messages.clone(),
                );

                let http_response = self.send(to_device_request).await?;
                let response = RumaToDeviceResponse::try_from_http_response(http_response)?;
                self.olm().mark_request_as_sent(outgoing_request.request_id(), &response).await?;
            }
            AnyOutgoingRequest::SignatureUpload(request) => {
                tracing::info!("Device {} is uploading a signature to the homeserver", &self.device()?.id());
                let http_response = self.send(request.clone()).await?;
                let response = RumaUploadSignaturesResponse::try_from_http_response(http_response)?;
                self.olm().mark_request_as_sent(outgoing_request.request_id(), &response).await?;
            }
            AnyOutgoingRequest::RoomMessage(request) => {
                tracing::info!("Device {} is sending a room message", &self.device()?.id());
                let content = Raw::new(&*request.content.clone())?;
                let txn_id = request.txn_id.clone();
                let room_id = request.room_id.clone();

                let send_message_request: RumaSendMessageRequest =
                    RumaSendMessageRequest::new_raw(room_id, txn_id, request.content.event_type(), content);
                let http_response = self.send(send_message_request).await?;
                let response = RumaSendMessageResponse::try_from_http_response(http_response)?;
                self.olm().mark_request_as_sent(outgoing_request.request_id(), &response).await?;
            }
            AnyOutgoingRequest::KeysClaim(request) => {
                tracing::info!("Device {} is claiming one time keys", &self.device()?.id());
                let http_response = self.send(request.clone()).await?;
                let response = RumaKeysClaimResponse::try_from_http_response(http_response)?;
                self.olm().mark_request_as_sent(outgoing_request.request_id(), &response).await?;
            }
        }

        Ok(())
    }
}

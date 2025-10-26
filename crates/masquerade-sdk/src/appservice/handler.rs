use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Weak};

use axum::Json;
use matrix_sdk::ruma::events::{AnySyncEphemeralRoomEvent, AnySyncTimelineEvent, AnyToDeviceEvent};
use matrix_sdk::ruma::exports::serde_json::{Value, json};
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{OwnedRoomId, OwnedTransactionId, RoomId, TransactionId, UserId};
use reqwest::StatusCode;
use serde::Deserialize;

use crate::appservice::device::Device;
use crate::appservice::encryption::OwnedEncryptionSyncChanges;
use crate::appservice::event_handler::EventHandlerStore;
use crate::appservice::http_client::{Client, parse_response};
use crate::appservice::room::{Room, RoomStore};
use crate::appservice::transaction::TransactionLog;
use crate::appservice::types::{Config, Ping, Transaction};
use crate::appservice::user::{User, UserStore};
use crate::appservice::{ApplicationServiceInner, EventContext};
use crate::{Error, PingResponse, Result};

pub trait ApplicationServiceReference {
    fn appservice(&self) -> Result<Arc<ApplicationServiceInner>>;
    fn client(&self) -> Result<Arc<Client>> {
        Ok(self.appservice()?.client())
    }
}

impl ApplicationServiceInner {
    pub async fn new(config: Config) -> Result<Arc<Self>> {
        let mxid = UserId::parse(format!("@{}:{}", &config.appservice.username, &config.homeserver.server_name))?;

        let client = Arc::new(Client::new(&config)?);
        let inner = Arc::new_cyclic(|weak_ref| Self {
            mxid: mxid.clone(),
            config,
            client,
            user_store: UserStore::new(Weak::clone(weak_ref)),
            room_store: RoomStore::new(Weak::clone(weak_ref)),
            handler_store: EventHandlerStore::new(),
            transaction_log: TransactionLog::new(),
        });

        Ok(inner)
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn client(&self) -> Arc<Client> {
        Arc::clone(&self.client)
    }

    pub fn room_store(&self) -> &RoomStore {
        &self.room_store
    }

    pub fn user_store(&self) -> &UserStore {
        &self.user_store
    }

    pub fn handler_store(&self) -> &EventHandlerStore {
        &self.handler_store
    }

    pub fn transaction_log(&self) -> &TransactionLog {
        &self.transaction_log
    }

    pub async fn run(self: &Arc<Self>) -> Result<()> {
        self.ping().await?;

        tracing::info!("Initializing user {}", &self.mxid);
        let bot_user = self.create_user(self.mxid.as_str()).await?;
        let bot_device = bot_user.create_device(None).await?;

        let displayname = self.config.appservice.displayname.clone();
        match bot_user.get_profile().await {
            Ok(profile) => {
                if let Some(current_displayname) = profile.displayname
                    && current_displayname != displayname
                {
                    bot_user.set_displayname(&displayname).await?;
                }
            }
            Err(_) => {
                bot_user.register().await?;
                bot_user.set_displayname(&displayname).await?;
                bot_device.register(Some(self.config.appservice.displayname.clone())).await?;
            }
        }

        if let Err(error) = bot_device.run().await {
            tracing::error!("Device sync loop for {} failed: {}", bot_device.id(), error);
            return Err(error);
        }

        Ok(())
    }

    pub async fn ping(&self) -> Result<()> {
        tracing::info!("Pinging homeserver...");
        let url = format!("/_matrix/client/v1/appservice/{}/ping", self.config.appservice.id);
        let body = Ping { transaction_id: TransactionId::new().to_string() };
        let response = self.client.post(&url).json(&body).send().await?;
        let json: PingResponse = parse_response(response).await?;

        tracing::info!("Homeserver {} responded in {} ms", self.config().homeserver.server_name, json.duration_ms);
        Ok(())
    }

    pub async fn handle_ping(&self, _: Ping) -> (StatusCode, Json<Value>) {
        (StatusCode::OK, Json(json!({})))
    }

    pub async fn handle_transaction(
        self: &Arc<Self>,
        txn_id: &OwnedTransactionId,
        transaction: Transaction,
    ) -> (StatusCode, Json<Value>) {
        let (events, ephemeral_events) = match self.extract_sync_tasks(transaction).await {
            Ok(result) => result,
            Err(error) => {
                tracing::error!("Error while extracting sync events: {}", error);
                return self.create_error_response(StatusCode::INTERNAL_SERVER_ERROR);
            }
        };

        let message = if (events.len(), ephemeral_events.len()) == (0, 0) {
            String::from("Processing synchronization tasks")
        } else {
            format!("Processing {} event(s), {} ephemeral event(s)", events.len(), ephemeral_events.len())
        };
        tracing::info!("Received transaction {} from homeserver. {}", txn_id, message);

        // TODO Ephemeral events
        // for event in ephemeral_events {
        //     if let Err(error) = self.handle_event(event.into()).await {
        //         tracing::error!("Error while handling received ephemeral event: {}", error);
        //         return self.create_error_response(StatusCode::INTERNAL_SERVER_ERROR);
        //     }
        // }

        for event in events {
            if let Err(error) = self.handle_event(event).await {
                tracing::error!("Error while handling received event: {}", error);
                return self.create_error_response(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }

        (StatusCode::OK, Json(json!({})))
    }

    pub async fn handle_event<'a>(&self, event: Raw<AnySyncTimelineEvent>) -> Result<()> {
        #[derive(Deserialize)]
        struct ExtractType<'a> {
            #[serde(borrow, rename = "type")]
            event_type: Cow<'a, str>,
            #[serde(borrow)]
            room_id: Cow<'a, str>,
            #[serde(borrow)]
            sender: Cow<'a, str>,
        }

        let extracted = event.deserialize_as::<ExtractType<'_>>()?;
        if let Some(handlers) = self.handler_store().get(&extracted.event_type).await {
            let context =
                EventContext { room_id: RoomId::parse(extracted.room_id)?, sender: UserId::parse(extracted.sender)? };

            for handler in handlers {
                handler.handle(event.clone(), context.clone()).await;
            }
        }

        Ok(())
    }

    async fn extract_sync_tasks(
        self: &Arc<Self>,
        transaction: Transaction,
    ) -> Result<(Vec<Raw<AnySyncTimelineEvent>>, Vec<Raw<AnySyncEphemeralRoomEvent>>)> {
        let key_counts = transaction.device_one_time_keys_count.unwrap_or_default();
        let fallback_keys = transaction.device_unused_fallback_key_types.unwrap_or_default();
        let device_lists = transaction.device_lists.unwrap_or_default();

        let mut to_device_idx: HashMap<String, HashMap<String, Vec<Raw<AnyToDeviceEvent>>>> = HashMap::new();
        for event in &transaction.to_device {
            let Some(mxid) = event.get_field::<String>("to_user_id")? else {
                continue;
            };
            let Some(device_id) = event.get_field::<String>("to_device_id")? else {
                continue;
            };

            to_device_idx.entry(mxid).or_default().entry(device_id).or_default().push(event.to_owned());
        }

        for (mxid, device_map) in key_counts {
            for (device_id, algo_map) in device_map {
                let picked_events =
                    to_device_idx.get(&mxid).and_then(|devices| devices.get(&device_id)).cloned().unwrap_or_default();

                let picked_fallback_keys =
                    fallback_keys.get(&mxid).and_then(|devices| devices.get(&device_id)).cloned().unwrap_or_default();

                let device = self.ensure_device(&mxid, &device_id).await?;
                let sync_changes = OwnedEncryptionSyncChanges {
                    to_device_events: picked_events,
                    changed_devices: device_lists.clone(),
                    one_time_keys_counts: algo_map,
                    unused_fallback_keys: picked_fallback_keys,
                    next_batch_token: None,
                };

                device.send_sync_changes(sync_changes).await?;
            }
        }

        Ok((transaction.events, transaction.ephemeral))
    }

    pub async fn get_room(&self, room_id: &RoomId) -> Option<Arc<Room>> {
        self.room_store().get(room_id).await
    }

    pub async fn get_user(&self, mxid: &str) -> Option<Arc<User>> {
        match UserId::parse(mxid) {
            Ok(user_id) => self.user_store.get(&user_id).await,
            Err(_) => None,
        }
    }

    pub async fn get_bot(&self) -> Result<Arc<User>> {
        let mxid = UserId::parse_with_server_name(
            self.config().appservice.username.clone(),
            &self.config().homeserver.server_name,
        )?;

        let bot = self.get_user(mxid.as_str()).await.ok_or(Error::UserNotFound(mxid))?;

        Ok(bot)
    }

    pub async fn ensure_device(self: &Arc<Self>, mxid: &str, device_id: &str) -> Result<Arc<Device>> {
        let device = match self.get_user(mxid).await {
            Some(user) => match user.get_device().await {
                Some(device) => device,
                None => user.create_device(Some(device_id)).await?,
            },
            None => {
                let user = self.create_user(mxid).await?;
                let device = user.create_device(Some(device_id)).await?;

                tokio::spawn({
                    let device = Arc::clone(&device);
                    async move {
                        if let Err(error) = device.run().await {
                            tracing::error!("Error in main loop for device {}: {}", device.id(), error)
                        }
                    }
                });

                device
            }
        };

        Ok(device)
    }

    pub async fn init_bot(self: &Arc<Self>) -> Result<()> {
        let mxid = format!("@{}:{}", &self.config.appservice.username, &self.config.homeserver.server_name);

        let bot_user = self.create_user(&mxid).await?;
        bot_user.create_device(None).await?;

        Ok(())
    }

    pub async fn create_user(self: &Arc<Self>, mxid: &str) -> Result<Arc<User>> {
        let mxid = UserId::parse(mxid)?;
        let user = User::new(self, mxid.clone()).await;

        Ok(user)
    }

    pub async fn create_room(self: &Arc<Self>, room_id: OwnedRoomId) -> Result<Arc<Room>> {
        Ok(Room::from_homeserver(self, room_id).await?)
    }

    pub fn is_autorized(&self, token: &str) -> bool {
        token == self.config.appservice.hs_token
    }

    pub fn create_error_response(&self, code: StatusCode) -> (StatusCode, Json<Value>) {
        let error_message = code.canonical_reason().unwrap_or("Unknown error code");
        (
            code,
            Json(json!(
                {
                    "errcode": format!(
                        "NL.SPACEBASED.{}_{}",
                        &self.config.appservice.id.to_uppercase(),
                        str::replace(error_message, " ", "_").to_uppercase()
                    )
                }
            )),
        )
    }
}

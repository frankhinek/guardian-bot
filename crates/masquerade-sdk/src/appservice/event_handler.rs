use core::result::Result as StdResult;
use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::marker::PhantomData;
use std::sync::Arc;

use futures::future::BoxFuture;
use matrix_sdk::event_handler::SyncEvent;
use matrix_sdk::ruma::events::AnySyncTimelineEvent;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId};
use serde::de::DeserializeOwned;
use tokio::sync::RwLock;

use crate::{ApplicationService, Error, Result};

// #[derive(EventContent)]
// #[ruma_event(type = "nl.spacedbased.device_ready", kind = State, state_key_type = EmptyStateKey)]
// pub struct DeviceReadyContent {

// }

pub type EventHandlerMap = BTreeMap<&'static str, Vec<Arc<dyn EventHandler>>>;

pub struct EventHandlerStore {
    event_handlers: RwLock<EventHandlerMap>,
}

impl EventHandlerStore {
    pub fn new() -> Self {
        Self { event_handlers: RwLock::new(BTreeMap::new()) }
    }

    pub async fn insert<Ev, H, Fut, Err>(&self, handler: Arc<TypedEventHandler<Ev, H>>) -> Result<()>
    where
        Ev: SyncEvent + DeserializeOwned + Send + Sync + 'static,
        H: Fn(Ev, EventContext) -> Fut + Clone + Send + Sync + 'static,
        Fut: Future<Output = StdResult<(), Err>> + Send + 'static,
        Err: Into<Box<dyn StdError + Send + Sync>> + 'static,
    {
        let mut handlers = self.event_handlers.write().await;
        handlers.entry(handler.get_type()?).or_default().push(handler);

        Ok(())
    }

    pub async fn get(&self, event_type: &str) -> Option<Vec<Arc<dyn EventHandler>>> {
        let handlers = self.event_handlers.read().await;
        handlers.get(event_type).cloned()
    }
}

#[derive(Clone)]
pub struct EventContext {
    pub room_id: OwnedRoomId,
    pub sender: OwnedUserId,
}
pub trait EventHandler: Send + Sync {
    fn handle(&self, raw: Raw<AnySyncTimelineEvent>, context: EventContext) -> BoxFuture<'static, ()>;
}
pub struct TypedEventHandler<Ev, H> {
    handler: H,
    _phantom: PhantomData<Ev>,
}

impl<Ev, H, Fut, Err> EventHandler for TypedEventHandler<Ev, H>
where
    Ev: DeserializeOwned + Send + Sync + 'static,
    H: Fn(Ev, EventContext) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = StdResult<(), Err>> + Send + 'static,
    Err: Into<Box<dyn StdError + Send + Sync>> + 'static,
{
    fn handle(&self, raw: Raw<AnySyncTimelineEvent>, context: EventContext) -> BoxFuture<'static, ()> {
        let maybe_event = raw.deserialize_as::<Ev>();
        let handler = self.handler.clone();

        Box::pin(async move {
            match maybe_event {
                Ok(event) => {
                    if let Err(error) = handler(event, context).await {
                        tracing::error!("Error handling event: {}", error.into());
                    }
                }
                Err(error) => {
                    tracing::error!("Failed to deserialize event: {}", error);
                }
            }
        })
    }
}

impl<Ev, H> TypedEventHandler<Ev, H>
where
    Ev: SyncEvent,
{
    fn get_type(&self) -> Result<&'static str> {
        Ev::TYPE.ok_or(Error::EventType("Error adding event handler, invalid event type".to_string()))
    }
}

impl<S: Send + Sync + Clone + 'static> ApplicationService<S> {
    pub(crate) async fn add_base_handlers(&self) -> Result<()> {
        self.add_event_handler(Self::on_stripped_room_member).await?;
        self.add_event_handler(Self::on_room_encryption).await?;
        self.add_event_handler(Self::on_encrypted_message).await?;

        Ok(())
    }

    pub async fn add_event_handler<Ev, H, Fut, Err>(&self, event_handler: H) -> Result<&Self>
    where
        Ev: SyncEvent + DeserializeOwned + Send + Sync + 'static,
        H: Fn(Ev, ApplicationService<S>, EventContext) -> Fut + Send + Sync + Clone + 'static,
        Fut: Future<Output = StdResult<(), Err>> + Send + 'static,
        Err: Into<Box<dyn StdError + Send + Sync>> + 'static,
    {
        let lifted_handler = {
            let appservice = self.clone();
            move |event: Ev, ctx: EventContext| event_handler(event, appservice.clone(), ctx)
        };

        let handler = Arc::new(TypedEventHandler::<Ev, _> { handler: lifted_handler, _phantom: PhantomData });

        self.inner.handler_store().insert(handler).await?;
        Ok(self)
    }
}

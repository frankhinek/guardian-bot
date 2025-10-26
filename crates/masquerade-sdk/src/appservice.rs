use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Request, State as AppState};
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use axum_extra::TypedHeader;
use axum_extra::headers::Authorization;
use axum_extra::headers::authorization::Bearer;
use matrix_sdk::ruma::events::AnySyncTimelineEvent;
use matrix_sdk::ruma::events::room::encrypted::OriginalSyncRoomEncryptedEvent;
use matrix_sdk::ruma::events::room::encryption::StrippedRoomEncryptionEvent;
use matrix_sdk::ruma::events::room::member::{MembershipChange, StrippedRoomMemberEvent};
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{OwnedTransactionId, OwnedUserId, RoomId, UserId};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;

mod builder;
mod device;
mod encryption;
mod error;
mod event_handler;
mod handler;
mod http_client;
mod room;
mod transaction;
pub mod types;
mod user;

pub use self::builder::ApplicationServiceBuilder;
pub use self::device::Device;
pub use self::error::{Error, Result};
pub use self::event_handler::EventContext;
pub use self::room::{Direction, Room};
pub use self::types::*;
pub use self::user::User;
use crate::appservice::event_handler::EventHandlerStore;
use crate::appservice::http_client::Client;
use crate::appservice::room::RoomStore;
use crate::appservice::transaction::TransactionLog;
use crate::appservice::user::UserStore;

pub struct ApplicationServiceInner {
    mxid: OwnedUserId,
    config: Config,
    client: Arc<Client>,
    room_store: RoomStore,
    user_store: UserStore,
    handler_store: EventHandlerStore,
    transaction_log: TransactionLog,
}

#[derive(Clone)]
pub struct ApplicationService<S = NoState> {
    inner: Arc<ApplicationServiceInner>,
    state: S,
}

impl ApplicationService<NoState> {
    pub async fn new(config: Config) -> Result<Self> {
        let inner = ApplicationServiceInner::new(config).await?;
        let appservice = Self { inner, state: NoState };

        appservice.add_base_handlers().await?;
        Ok(appservice)
    }

    pub async fn from_file(config_path: &str) -> Result<Self> {
        let file = std::fs::File::open(config_path).map_err(|error| {
            tracing::error!("Unable to open file {config_path}: {error}");
            error
        })?;

        let config = serde_yaml::from_reader::<_, Config>(file).map_err(|error| {
            tracing::error!("Unable to parse configuration file: {error}");
            error
        })?;

        Ok(Self::new(config).await?)
    }

    pub fn with_state<S: Send + Sync + Clone + 'static>(&self, state: S) -> ApplicationService<State<S>> {
        ApplicationService { inner: Arc::clone(&self.inner), state: State(state) }
    }
}

impl<S: Send + Sync + Clone + 'static> ApplicationService<State<S>> {
    async fn new_stateful(config: Config, state: S) -> Result<Self> {
        let inner = ApplicationServiceInner::new(config).await?;
        let appservice = Self { inner, state: State(state) };

        appservice.add_base_handlers().await?;
        Ok(appservice)
    }

    pub fn state(&self) -> &S {
        &self.state.0
    }
}

impl<S: 'static> ApplicationService<S> {
    pub async fn run(&self) -> Result<()> {
        let app = Router::new()
            .route("/_matrix/app/v1/transactions/{txn_id}", put(Self::handle_transaction))
            .route("/_matrix/app/v1/ping", post(Self::handle_ping))
            .route("/_matrix/app/v1/users/{user_id}", get(Self::todo))
            .route("/_matrix/app/v1/rooms/{room_alias}", get(Self::todo))
            .route("/_matrix/app/v1/thirdparty/location", get(Self::todo))
            .route("/_matrix/app/v1/thirdparty/location/{protocol}", get(Self::todo))
            .route("/_matrix/app/v1/thirdparty/protocol/{protocol}", get(Self::todo))
            .route("/_matrix/app/v1/thirdparty/user", get(Self::todo))
            .route("/_matrix/app/v1/thirdparty/user/{protocol}", get(Self::todo))
            .fallback(Self::fallback)
            .with_state(Arc::clone(&self.inner))
            .layer(axum::middleware::from_fn_with_state(Arc::clone(&self.inner), Self::authorize));

        let listen_address =
            SocketAddr::new(self.inner.config().appservice.bind_ip, self.inner.config().appservice.port);
        let listener = tokio::net::TcpListener::bind(listen_address).await?;

        tracing::info!("Starting HTTP listener on {}", listen_address.to_string());
        tokio::select! {
            serve_result = axum::serve(listener, app) => serve_result?,
            handler_result = self.inner.run() => handler_result?,
        }

        Ok(())
    }

    async fn authorize(
        TypedHeader(Authorization(credentials)): TypedHeader<Authorization<Bearer>>,
        AppState(inner): AppState<Arc<ApplicationServiceInner>>,
        request: Request,
        next: Next,
    ) -> impl IntoResponse {
        match inner.is_autorized(credentials.token()) {
            true => Ok(next.run(request).await),
            false => Err(inner.create_error_response(StatusCode::UNAUTHORIZED)),
        }
    }

    async fn handle_transaction(
        Path(txn_id): Path<OwnedTransactionId>,
        AppState(inner): AppState<Arc<ApplicationServiceInner>>,
        Json(body): Json<Transaction>,
    ) -> impl IntoResponse {
        inner.transaction_log().lock_while(txn_id.clone(), || inner.handle_transaction(&txn_id, body)).await
    }

    async fn handle_ping(
        AppState(inner): AppState<Arc<ApplicationServiceInner>>,
        Json(body): Json<Ping>,
    ) -> impl IntoResponse {
        inner.handle_ping(body).await
    }

    async fn fallback(AppState(inner): AppState<Arc<ApplicationServiceInner>>) -> impl IntoResponse {
        inner.create_error_response(StatusCode::NOT_FOUND)
    }

    async fn todo(AppState(inner): AppState<Arc<ApplicationServiceInner>>) -> impl IntoResponse {
        inner.create_error_response(StatusCode::NOT_IMPLEMENTED)
    }
}

impl<S> ApplicationService<S> {
    pub fn config(&self) -> &Config {
        &self.inner.config()
    }

    pub fn client(&self) -> Arc<Client> {
        Arc::clone(&self.inner.client)
    }

    pub async fn get_bot(&self) -> Result<Arc<User>> {
        Ok(self.inner.get_bot().await?)
    }

    pub async fn get_user(&self, mxid: &str) -> Option<Arc<User>> {
        self.inner.get_user(mxid).await
    }

    pub async fn get_room(&self, room_id: &RoomId) -> Option<Arc<Room>> {
        self.inner.get_room(room_id).await
    }

    pub fn generate_registration(&self) -> Result<String> {
        let mut appservice_url = self.config().appservice.url.clone();
        appservice_url.set_port(Some(self.config().appservice.port))?;

        let mxid = UserId::parse(format!(
            "@{}:{}",
            &self.config().appservice.username,
            &self.config().homeserver.server_name
        ))?;

        let registration = Registration {
            id: self.config().appservice.id.clone(),
            url: appservice_url,
            as_token: self.config().appservice.as_token.clone(),
            hs_token: self.config().appservice.hs_token.clone(),
            sender_localpart: self.config().appservice.username.clone(),
            rate_limited: Some(false),
            protocols: None,
            namespaces: Namespaces {
                users: vec![NamespaceEntry { exclusive: true, regex: format!("^{}$", regex::escape(mxid.as_str())) }],
                aliases: vec![],
                rooms: vec![],
            },
            receive_ephemeral: Some(true),
            device_masquerading: Some(true),
            device_management: Some(true),
        };

        let yaml = serde_yaml::to_string(&registration)?;
        Ok(yaml)
    }

    pub fn get_user_fields<T>(&self) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let mapping = self.config().clone().user_fields.into_iter().collect();
        Ok(serde_yaml::from_value(serde_yaml::Value::Mapping(mapping))?)
    }

    pub async fn dispatch_event(&self, event: Raw<AnySyncTimelineEvent>) -> Result<()> {
        self.inner.handle_event(event).await
    }

    pub async fn ping_homeserver(&self) -> Result<()> {
        self.inner.ping().await
    }

    async fn on_stripped_room_member(
        event: StrippedRoomMemberEvent,
        appservice: ApplicationService<S>,
        context: EventContext,
    ) -> Result<()> {
        match event.membership_change(None) {
            MembershipChange::Joined => {
                appservice.inner.room_store().add_room_member(&context.room_id, event.state_key).await?;
            }
            MembershipChange::Left => {
                appservice.inner.room_store().remove_room_member(&context.room_id, &event.state_key).await?;
            }
            _ => (),
        };

        Ok(())
    }

    async fn on_room_encryption(
        _: StrippedRoomEncryptionEvent,
        appservice: ApplicationService<S>,
        context: EventContext,
    ) -> Result<()> {
        appservice.inner.room_store().upgrade_room_encryption(&context.room_id).await?;

        Ok(())
    }

    async fn on_encrypted_message(
        event: Raw<OriginalSyncRoomEncryptedEvent>,
        appservice: ApplicationService<S>,
        context: EventContext,
    ) -> Result<()> {
        let room = appservice.get_room(&context.room_id).await.ok_or(Error::RoomNotFound(context.room_id.clone()))?;

        let users = room.get_appservice_users().await?;
        for user in users {
            if let Some(device) = user.get_device().await {
                let decrypted = device.encryption().decrypt_event(event.clone().cast(), &context.room_id).await;
                if let Ok(decrypted) = decrypted {
                    appservice.dispatch_event(decrypted.event.cast()).await?;
                    return Ok(());
                }
            }
        }

        Err(Error::DecryptEvent(format!("Unable to decrypt event in room {}", context.room_id)))
    }
}

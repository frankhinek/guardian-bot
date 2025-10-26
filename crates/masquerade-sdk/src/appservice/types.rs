use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;

use matrix_sdk::ServerName;
use matrix_sdk::ruma::api::client::device::Device;
use matrix_sdk::ruma::api::client::sync::sync_events::DeviceLists;
use matrix_sdk::ruma::events::{AnySyncEphemeralRoomEvent, AnySyncTimelineEvent, AnyToDeviceEvent};
use matrix_sdk::ruma::presence::PresenceState;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{OneTimeKeyAlgorithm, OwnedEventId, OwnedRoomId, OwnedUserId, UInt};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Copy, Clone)]
pub struct NoState;

#[derive(Clone)]
pub struct State<S>(pub S);

#[derive(Deserialize)]
pub struct Empty {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Homeserver {
    pub server_name: Box<ServerName>,
    pub url: Url,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Appservice {
    pub url: Url,
    pub bind_ip: IpAddr,
    pub port: u16,
    pub id: String,
    pub username: String,
    pub displayname: String,
    pub as_token: String,
    pub hs_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Database {
    pub path: String,
    pub passphrase: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub homeserver: Homeserver,
    pub appservice: Appservice,
    pub database: Database,
    #[serde(flatten)]
    pub(crate) user_fields: HashMap<serde_yaml::Value, serde_yaml::Value>,
}

#[derive(Debug, Serialize)]
pub struct Registration {
    pub id: String,
    pub url: Url,
    pub as_token: String,
    pub hs_token: String,
    pub sender_localpart: String,
    pub namespaces: Namespaces,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limited: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocols: Option<Vec<String>>,
    #[serde(rename = "de.sorunome.msc2409.push_ephemeral")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receive_ephemeral: Option<bool>,
    #[serde(rename = "org.matrix.msc3202")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_masquerading: Option<bool>,
    #[serde(rename = "io.element.msc4190")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_management: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct Namespaces {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub users: Vec<NamespaceEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<NamespaceEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rooms: Vec<NamespaceEntry>,
}

#[derive(Debug, Serialize)]
pub struct NamespaceEntry {
    pub exclusive: bool,
    pub regex: String,
}

#[derive(Debug, Deserialize)]
pub struct DeviceList {
    pub changed: Vec<String>,
    pub left: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct JoinedMembersResponse {
    pub joined: HashMap<OwnedUserId, Profile>,
}

#[derive(Debug, Deserialize)]
pub struct SendResponse {
    pub event_id: OwnedEventId,
}

#[derive(Debug, Deserialize)]
pub struct Transaction {
    pub events: Vec<Raw<AnySyncTimelineEvent>>,
    #[serde(alias = "de.sorunome.msc2409.ephemeral")]
    pub ephemeral: Vec<Raw<AnySyncEphemeralRoomEvent>>,
    #[serde(alias = "de.sorunome.msc2409.to_device")]
    pub to_device: Vec<Raw<AnyToDeviceEvent>>,
    #[serde(alias = "org.matrix.msc3202.device_lists")]
    pub device_lists: Option<DeviceLists>,
    #[serde(alias = "org.matrix.msc3202.device_one_time_keys_count")]
    pub device_one_time_keys_count: Option<HashMap<String, HashMap<String, BTreeMap<OneTimeKeyAlgorithm, UInt>>>>,
    #[serde(alias = "org.matrix.msc3202.device_unused_fallback_key_types")]
    pub device_unused_fallback_key_types: Option<HashMap<String, HashMap<String, Vec<OneTimeKeyAlgorithm>>>>,
}

#[derive(Debug, Deserialize)]
pub struct JoinedRoomResponse {
    pub joined_rooms: Vec<OwnedRoomId>,
}

#[derive(Debug, Deserialize)]
pub struct MessagesResponse {
    pub chunk: Vec<Raw<AnySyncTimelineEvent>>,
    pub end: Option<String>,
    pub start: String,
    pub state: Option<Vec<Raw<AnySyncTimelineEvent>>>,
}

#[derive(Debug, Deserialize)]
pub struct Devices {
    pub devices: Vec<Device>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Ping {
    pub transaction_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PingResponse {
    pub duration_ms: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Presence {
    pub presence: PresenceState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_msg: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateDeviceRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Profile {
    pub avatar_url: Option<Url>,
    #[serde(alias = "display_name")]
    pub displayname: Option<String>,
}

use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId};
use reqwest::StatusCode;
use serde_json::Value;

use crate::appservice::encryption::OwnedEncryptionSyncChanges;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Error running sync loop: {0}")]
    Async(#[from] tokio::task::JoinError),

    #[error("Error sending synchronization task: {0}")]
    Send(#[from] tokio::sync::mpsc::error::SendError<OwnedEncryptionSyncChanges>),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Error while parsing configuration file: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("Error while parsing event: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Invalid header value: {0}")]
    InvalidHeader(#[from] reqwest::header::InvalidHeaderValue),

    #[error("Matrix error: {0}")]
    Matrix(#[from] matrix_sdk::ruma::api::error::MatrixError),

    #[error("Error occurred with the crypto store: {0}")]
    CryptoStore(#[from] matrix_sdk::crypto::CryptoStoreError),

    #[error("Error occurred within the olm machine: {0}")]
    Olm(#[from] matrix_sdk::encryption::OlmError),

    #[error("Error occurred while decrypting event: {0}")]
    Megolm(#[from] matrix_sdk::encryption::MegolmError),

    #[error("Id parse error")]
    ParseId(#[from] matrix_sdk::IdParseError),

    #[error("Error while opening Sqlite database: {0}")]
    Sqlite(#[from] matrix_sdk_sqlite::OpenStoreError),

    #[error("Error occurred in Axum: {0}")]
    Axum(#[from] axum::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] http::Error),

    #[error("HTTP error: {0}")]
    ReqwestHttp(#[from] reqwest::Error),

    #[error("HTTP error: {0}")]
    RumaFromHttp(#[from] matrix_sdk::ruma::api::error::FromHttpResponseError<matrix_sdk::ruma::api::client::Error>),

    #[error("HTTP error: {0}")]
    RumaIntoHttp(#[from] matrix_sdk::ruma::api::error::IntoHttpError),

    #[error("Unexpected response: {0}")]
    UnexpectedStatus(StatusCode, Value),

    #[error("File not found {0}")]
    FileNotFound(String),

    #[error("No such user: {0}")]
    UserNotFound(OwnedUserId),

    #[error("No device for user: {0}")]
    NoDevice(OwnedUserId),

    #[error("No such room: {0}")]
    RoomNotFound(OwnedRoomId),

    #[error("Cannot encrypt event. Room {0} is not encrypted")]
    RoomNotEncrypted(OwnedRoomId),

    #[error("Error added event handler, unknown type: {0}")]
    EventType(String),

    #[error("Parent container not found")]
    UpgradeError(String),

    #[error("Unable to decrypted incoming event: {0}")]
    DecryptEvent(String),

    #[error("Attempting to run multiple sync loops. This is not allowed: {0}")]
    MultipleSync(String),

    #[error("Unexpected error: {0}")]
    Other(String),
}

impl From<()> for Error {
    fn from(_: ()) -> Self {
        Error::Other("Unit".to_string())
    }
}

impl Error {
    // pub fn to_matrix_error(&self) -> MatrixError {
    //     match self {
    //         Error::Matrix(error) => error.clone(),
    //         // Error::Http(error) =>  MatrixError { status_code: error.status().unwrap(), body: () },
    //         Error::UnexpectedStatus(status, body) => MatrixError {
    //             status_code: *status,
    //             body: MatrixErrorBody::Json(body.clone()),
    //         },
    //     }
    // }

    // pub async fn error_for_status(response: reqwest::Response) -> Result<()> {
    //     if let Err(_) = response.error_for_status_ref() {
    //         let status_code = response.status();
    //         tracing::info!("{:?}", status_code);
    //         let body = response.json().await?;
    //         return Err(Error::UnexpectedStatus(status_code, body).into());
    //     }

    //     Ok(())
    // }
}

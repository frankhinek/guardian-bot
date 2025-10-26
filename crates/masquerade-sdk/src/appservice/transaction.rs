use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use matrix_sdk::ruma::OwnedTransactionId;
use matrix_sdk::ruma::exports::serde_json::Value;
use reqwest::StatusCode;
use tokio::sync::{Mutex, OnceCell};

#[derive(Debug)]
pub struct TransactionLog {
    inner: Mutex<HashMap<OwnedTransactionId, Arc<OnceCell<(StatusCode, Json<Value>)>>>>,
}

impl TransactionLog {
    pub fn new() -> Self {
        Self { inner: Mutex::new(HashMap::new()) }
    }

    pub async fn lock_while<F, Fut>(&self, txn_id: OwnedTransactionId, op: F) -> (StatusCode, Json<Value>)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = (StatusCode, Json<Value>)>,
    {
        let cell = {
            let mut lock = self.inner.lock().await;
            let value = lock.entry(txn_id).or_insert_with(|| Arc::new(OnceCell::new()));
            Arc::clone(value)
        };

        cell.get_or_init(op).await.clone()
    }
}

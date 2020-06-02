use std::convert::TryInto;
use std::iter;
use std::sync::Arc;

use async_trait::async_trait;
use futures::lock::Mutex;
use futures::{future, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::internal::chain::{Chain, Mutation};
use crate::internal::file::*;
use crate::internal::Dir;
use crate::transaction::{Transact, TxnId};
use crate::value::TCResult;

#[derive(Clone, Deserialize, Serialize)]
struct HistoryObject<O> {
    object: O,
}

impl<O: Clone + DeserializeOwned + Serialize + Send + Sync> Mutation for HistoryObject<O> {}

pub struct History<O: Clone + DeserializeOwned + Serialize + Send + Sync> {
    chain: Mutex<Chain<HistoryObject<O>>>,
}

impl<O: Clone + DeserializeOwned + Serialize + Send + Sync + 'static> History<O> {
    pub async fn new(txn_id: TxnId, context: Arc<Dir>) -> TCResult<Arc<History<O>>> {
        let store = context.create_store(&txn_id, "chain".parse()?).await?;
        let chain = Chain::new(txn_id, store).await;

        Ok(Arc::new(History {
            chain: Mutex::new(chain),
        }))
    }

    pub async fn at(&self, txn_id: TxnId) -> Option<O> {
        self.chain
            .lock()
            .await
            .stream_into(txn_id)
            .fold(None, |_, s| future::ready(Some(s)))
            .await
            .map(|e| e.object)
    }

    pub async fn put(&self, txn_id: TxnId, object: O) -> TCResult<()> {
        self.chain
            .lock()
            .await
            .put(txn_id, iter::once(HistoryObject { object }))
            .await
    }
}

#[async_trait]
impl<O: Clone + DeserializeOwned + Serialize + Send + Sync + 'static> File for History<O> {
    async fn copy_into(&self, txn_id: TxnId, copier: &mut FileCopier) {
        copier.write_file(
            "chain".parse().unwrap(),
            Box::new(self.chain.lock().await.stream_bytes(txn_id).boxed()),
        );
    }

    async fn copy_from(copier: &mut FileCopier, txn_id: &TxnId, dest: Arc<Dir>) -> Arc<History<O>> {
        let (path, blocks) = copier.next().await.unwrap();

        let chain: Chain<HistoryObject<O>> = Chain::copy_from(
            blocks,
            txn_id.clone(),
            dest.create_store(txn_id, path.try_into().unwrap())
                .await
                .unwrap(),
        )
        .await;

        Arc::new(History {
            chain: Mutex::new(chain),
        })
    }

    async fn from_dir(txn_id: &TxnId, dir: Arc<Dir>) -> Arc<History<O>> {
        Arc::new(History {
            chain: Mutex::new(
                Chain::from_store(
                    txn_id,
                    dir.get_store(txn_id, &"chain".parse().unwrap())
                        .await
                        .unwrap(),
                )
                .await
                .unwrap(),
            ),
        })
    }
}

#[async_trait]
impl<O: Clone + DeserializeOwned + Serialize + Send + Sync> Transact for History<O> {
    async fn commit(&self, txn_id: &TxnId) {
        self.chain.lock().await.commit(txn_id).await
    }

    async fn rollback(&self, txn_id: &TxnId) {
        self.chain.lock().await.rollback(txn_id).await
    }
}
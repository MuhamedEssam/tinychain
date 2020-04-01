use std::sync::Arc;

use async_trait::async_trait;

use crate::context::*;
use crate::drive::Drive;
use crate::error;
use crate::transaction::Transaction;

#[derive(Hash)]
pub struct Block {
    path: Link,
}

impl Block {
    fn new(path: Link) -> Arc<Block> {
        Arc::new(Block { path })
    }
}

impl TCContext for Block {}

pub struct BlockContext {
    drive: Arc<Drive>,
}

impl BlockContext {
    pub fn new(drive: Arc<Drive>) -> Arc<BlockContext> {
        Arc::new(BlockContext { drive })
    }
}

#[async_trait]
impl TCContext for BlockContext {
    async fn post(self: Arc<Self>, txn: Arc<Transaction>, method: Link) -> TCResult<Arc<TCState>> {
        if method.as_str() != "/new" {
            return Err(error::bad_request(
                "BlockContext has no such method",
                method,
            ));
        }

        Ok(TCState::from_block(Block::new(txn.context())))
    }
}

use std::collections::HashMap;
use std::convert::{TryFrom, TryInto};
use std::hash::Hash;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use futures_locks::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use tokio::fs;

use error::*;
use transact::fs::BlockData;

use super::io_err;
use crate::chain::ChainBlock;
use futures::TryFutureExt;

#[derive(Clone)]
pub enum CacheBlock {
    Chain(CacheLock<ChainBlock>),
}

impl CacheBlock {
    async fn into_bytes(self) -> Bytes {
        match self {
            Self::Chain(block) => block.read().await.clone().into(),
        }
    }

    async fn into_size(self) -> usize {
        self.into_bytes().await.len()
    }
}

impl From<CacheLock<ChainBlock>> for CacheBlock {
    fn from(lock: CacheLock<ChainBlock>) -> CacheBlock {
        Self::Chain(lock)
    }
}

impl TryFrom<CacheBlock> for CacheLock<ChainBlock> {
    type Error = TCError;

    fn try_from(block: CacheBlock) -> TCResult<Self> {
        match block {
            CacheBlock::Chain(block) => Ok(block),
        }
    }
}

pub struct CacheLock<T> {
    ref_count: Arc<std::sync::RwLock<usize>>,
    lock: RwLock<T>,
}

impl<T> CacheLock<T> {
    fn new(value: T) -> Self {
        Self {
            ref_count: Arc::new(std::sync::RwLock::new(0)),
            lock: RwLock::new(value),
        }
    }

    pub async fn read(&self) -> RwLockReadGuard<T> {
        self.lock.read().await
    }

    pub async fn write(&self) -> RwLockWriteGuard<T> {
        self.lock.write().await
    }
}

impl<T> Clone for CacheLock<T> {
    fn clone(&self) -> Self {
        *self.ref_count.write().unwrap() += 1;

        Self {
            ref_count: self.ref_count.clone(),
            lock: self.lock.clone(),
        }
    }
}

impl<T> Drop for CacheLock<T> {
    fn drop(&mut self) {
        *self.ref_count.write().unwrap() -= 1;
    }
}

struct Inner {
    size: usize,
    max_size: usize,
    entries: HashMap<PathBuf, CacheBlock>,
    lfu: LFU<PathBuf>,
}

#[derive(Clone)]
pub struct Cache {
    inner: RwLock<Inner>,
}

impl Cache {
    pub fn new(max_size: usize) -> Self {
        Self {
            inner: RwLock::new(Inner {
                size: 0,
                max_size,
                entries: HashMap::new(),
                lfu: LFU::new(),
            }),
        }
    }

    pub async fn read<B: BlockData>(&self, path: &PathBuf) -> TCResult<Option<CacheLock<B>>>
    where
        CacheLock<B>: TryFrom<CacheBlock, Error = TCError>,
        CacheBlock: From<CacheLock<B>>,
    {
        let mut inner = self.inner.write().await;
        if let Some(lock) = inner.entries.get(path) {
            let lock = lock.clone().try_into()?;
            inner.lfu.bump(path);
            return Ok(Some(lock));
        } else {
            log::info!("cache miss");
        }

        let block = match fs::read(path).await {
            Ok(block) => Bytes::from(block),
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(io_err(err, path)),
        };

        let size = block.len();
        let block = B::try_from(block)?;
        let lock = CacheLock::new(block);
        let cached = CacheBlock::from(lock.clone());

        inner.size += size;
        inner.lfu.insert(path.clone());
        inner.entries.insert(path.clone(), cached);
        Ok(Some(lock))
    }

    pub async fn write<B: BlockData>(&self, path: PathBuf, block: B) -> TCResult<CacheLock<B>>
    where
        CacheBlock: From<CacheLock<B>>,
    {
        let size = {
            let as_bytes: Bytes = block.clone().into();
            as_bytes.len()
        };

        let mut inner = self.inner.write().await;

        if let Some(old_block) = inner.entries.remove(&path) {
            let old_size = old_block.into_size().await;
            if old_size > inner.size {
                inner.size -= old_size;
            }
        } else {
            inner.lfu.insert(path.clone());
        }

        let block = CacheLock::new(block);
        inner.lfu.bump(&path);
        inner.entries.insert(path, block.clone().into());
        inner.size += size;
        if inner.size > inner.max_size {
            log::warn!("cache overflowing but eviction is not yet implemented!");
        }

        Ok(block)
    }

    pub async fn remove(&self, path: PathBuf) {
        let mut inner = self.inner.write().await;
        if let Some(old_block) = inner.entries.remove(&path) {
            let old_size = old_block.into_size().await;
            if inner.size > old_size {
                inner.size -= old_size;
            }

            inner.lfu.remove(&path);
        }
    }

    pub async fn sync(&self, path: PathBuf) -> TCResult<()> {
        let inner = self.inner.read().await;
        if let Some(block) = inner.entries.get(&path) {
            let as_bytes = block.clone().into_bytes().await;
            fs::write(&path, as_bytes)
                .map_err(|e| io_err(e, &path))
                .await?;
        }

        Ok(())
    }
}

struct LFU<T: Hash> {
    entries: HashMap<T, usize>,
    priority: Vec<T>,
}

impl<T: Clone + Eq + Hash> LFU<T> {
    fn new() -> Self {
        LFU {
            entries: HashMap::new(),
            priority: Vec::new(),
        }
    }

    fn bump(&mut self, id: &T) {
        let (r_id, r) = self.entries.remove_entry(id).unwrap();
        if r == 0 {
            self.entries.insert(r_id, r);
        } else {
            let (l_id, l) = self.entries.remove_entry(&self.priority[r - 1]).unwrap();
            self.priority.swap(l, r);
            self.entries.insert(l_id, r);
            self.entries.insert(r_id, l);
        }
    }

    fn insert(&mut self, id: T) {
        assert!(!self.entries.contains_key(&id));

        self.entries.insert(id.clone(), self.priority.len());
        self.priority.push(id.clone());
    }

    fn remove(&mut self, id: &T) {
        if let Some(i) = self.entries.remove(id) {
            self.priority.remove(i);
        }
    }
}
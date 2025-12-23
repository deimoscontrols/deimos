//! Shared buffer pool utilities.

use std::sync::Arc;

use crossbeam::channel::{Receiver, Sender, bounded};

/// Default buffer length for socket receive buffers.
pub const SOCKET_BUFFER_LEN: usize = 1522;

/// Sized socket buffer type.
pub type SocketBuffer = Box<[u8; SOCKET_BUFFER_LEN]>;

pub struct BufferPool<T> {
    inner: Arc<BufferPoolInner<T>>,
}

struct BufferPoolInner<T> {
    tx: Sender<T>,
    rx: Receiver<T>,
}

impl<T> BufferPool<T> {
    /// Create an empty pool with the specified capacity.
    pub fn new(capacity: usize) -> Self {
        let (tx, rx) = bounded(capacity);
        Self {
            inner: Arc::new(BufferPoolInner { tx, rx }),
        }
    }

    /// Create a pool prefilled using the provided factory.
    pub fn with_factory<F>(capacity: usize, mut make: F) -> Self
    where
        F: FnMut() -> T,
    {
        let pool = Self::new(capacity);
        for _ in 0..capacity {
            let _ = pool.inner.tx.try_send(make());
        }
        pool
    }

    /// Try to take an item from the pool.
    pub fn try_get(&self) -> Option<T> {
        self.inner.rx.try_recv().ok()
    }

    /// Return an item to the pool.
    pub fn put(&self, item: T) -> Result<(), T> {
        self.inner.tx.try_send(item).map_err(|e| e.into_inner())
    }

    /// Lease an item from the pool if one is available.
    pub fn lease(&self) -> Option<BufferLease<T>> {
        self.try_get()
            .map(|item| BufferLease::new(self.clone(), item))
    }

    /// Lease an item, creating a new one if the pool is empty.
    pub fn lease_or_create<F>(&self, make: F) -> BufferLease<T>
    where
        F: FnOnce() -> T,
    {
        match self.try_get() {
            Some(item) => BufferLease::new(self.clone(), item),
            None => BufferLease::new(self.clone(), make()),
        }
    }

    /// Clone the pool sender for external producers.
    pub fn sender(&self) -> Sender<T> {
        self.inner.tx.clone()
    }

    /// Clone the pool receiver for external consumers.
    pub fn receiver(&self) -> Receiver<T> {
        self.inner.rx.clone()
    }
}

pub struct BufferLease<T> {
    pool: BufferPool<T>,
    item: Option<T>,
}

impl<T> BufferLease<T> {
    fn new(pool: BufferPool<T>, item: T) -> Self {
        Self {
            pool,
            item: Some(item),
        }
    }

    pub fn as_ref(&self) -> &T {
        self.item
            .as_ref()
            .expect("BufferLease missing item")
    }

    pub fn as_mut(&mut self) -> &mut T {
        self.item
            .as_mut()
            .expect("BufferLease missing item")
    }

    pub fn into_inner(mut self) -> T {
        self.item.take().expect("BufferLease missing item")
    }
}

impl<T> Drop for BufferLease<T> {
    fn drop(&mut self) {
        if let Some(item) = self.item.take() {
            let _ = self.pool.put(item);
        }
    }
}

impl<T> std::fmt::Debug for BufferPool<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferPool").finish()
    }
}

impl<T> Clone for BufferPool<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

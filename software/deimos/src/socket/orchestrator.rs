//! Socket orchestration for single-thread polling or worker fan-in.
//!
//! The orchestrator picks a backend based on the controller loop method:
//! a round-robin single-thread poller for performant loops, or a worker
//! pool that fan-ins packets via a channel for efficient loops.

use std::time::Duration;

use crossbeam::channel::{Receiver, RecvTimeoutError, unbounded};
use tracing::error;

use crate::controller::context::ControllerCtx;
use crate::socket::worker::{SocketWorkerCommand, SocketWorkerEvent, SocketWorkerHandle};
use crate::socket::{Socket, SocketAddrToken, SocketId, SocketRecvMeta};
use deimos_shared::peripherals::PeripheralId;

enum Backend {
    /// Use single-threaded nonblocking polling
    /// to receive packets from sockets.
    SingleThreadPoller {
        sockets: Vec<(String, Box<dyn Socket>)>,
        next_idx: usize,
    },

    /// Use a threaded worker pool wtih OS scheduling to
    /// receive packets from sockets.
    WorkerPool {
        workers: Vec<(String, SocketWorkerHandle)>,
        events: Receiver<SocketWorkerEvent>,
    },
}

/// Unified polling interface for multiple sockets in either
/// * Performant loop method: a single-threaded round-robin poller, or
/// * Efficient loop method:  multithreaded N:1 fan-in via worker threads.
pub struct SocketOrchestrator {
    backend: Backend,
}

impl SocketOrchestrator {
    /// Construct a new orchestrator and open any sockets that are not already open.
    pub fn new(
        mut sockets: Vec<(String, Box<dyn Socket>)>,
        ctx: &ControllerCtx,
        worker_timeout: Duration,
    ) -> Result<Self, String> {
        // Ensure sockets are open in the current context.
        for (_, sock) in sockets.iter_mut() {
            if !sock.is_open() {
                sock.open(ctx)?;
            }
        }

        let backend = match ctx.loop_method {
            crate::LoopMethod::Performant => Backend::SingleThreadPoller {
                sockets,
                next_idx: 0,
            },
            crate::LoopMethod::Efficient => {
                let (event_tx, event_rx) = unbounded();
                let mut workers = Vec::with_capacity(sockets.len());
                for (sid, (name, socket)) in sockets.into_iter().enumerate() {
                    workers.push((
                        name,
                        SocketWorkerHandle::spawn(
                            sid,
                            socket,
                            ctx.clone(),
                            worker_timeout,
                            event_tx.clone(),
                        ),
                    ));
                }
                drop(event_tx);
                Backend::WorkerPool {
                    workers,
                    events: event_rx,
                }
            }
        };

        Ok(Self { backend })
    }

    #[cold]
    pub fn socket_count(&self) -> usize {
        match &self.backend {
            Backend::SingleThreadPoller { sockets, .. } => sockets.len(),
            Backend::WorkerPool { workers, .. } => workers.len(),
        }
    }

    #[inline]
    /// Receive a packet into `buf`, returning metadata when available.
    ///
    /// Returns `Ok(None)` on timeout. For the single-threaded backend, sockets
    /// are polled round-robin with nonblocking reads and a final sleep to honor
    /// the timeout. For the worker backend, this waits on the worker event queue.
    pub fn recv(
        &mut self,
        buf: &mut [u8],
        timeout: Duration,
    ) -> Result<Option<SocketRecvMeta>, String> {
        match &mut self.backend {
            Backend::SingleThreadPoller { sockets, next_idx } => {
                if sockets.is_empty() {
                    return Ok(None);
                }
                let n = sockets.len();
                for _ in 0..n {
                    let idx = *next_idx;
                    *next_idx = (*next_idx + 1) % n;
                    if let Some(meta) = sockets[idx].1.recv(buf, Duration::ZERO) {
                        return Ok(Some(SocketRecvMeta {
                            socket_id: idx,
                            pid: meta.pid,
                            token: meta.token,
                            time: meta.time,
                            size: meta.size,
                        }));
                    }
                }
                if timeout.is_zero() {
                    return Ok(None);
                }
                std::thread::sleep(timeout);
                Ok(None)
            }
            Backend::WorkerPool { workers, events } => match events.recv_timeout(timeout) {
                Ok(SocketWorkerEvent::Packet {
                    socket_id,
                    meta,
                    payload,
                }) => {
                    if payload.len() > buf.len() {
                        return Err(format!(
                            "Recv buffer too small: {} > {}",
                            payload.len(),
                            buf.len()
                        ));
                    }
                    let size = meta.size.min(payload.len());
                    buf[..size].copy_from_slice(&payload[..size]);
                    Ok(Some(SocketRecvMeta {
                        socket_id,
                        pid: meta.pid,
                        token: meta.token,
                        time: meta.time,
                        size,
                    }))
                }
                Ok(SocketWorkerEvent::Error { socket_id, error }) => {
                    let socket_name = workers.get(socket_id).map(|(name, _)| name.as_str());
                    if let Some(name) = socket_name {
                        Err(format!("Socket worker {socket_id} ({name}) error: {error}"))
                    } else {
                        Err(format!("Socket worker {socket_id} error: {error}"))
                    }
                }
                Ok(SocketWorkerEvent::Closed { socket_id }) => {
                    let socket_name = workers.get(socket_id).map(|(name, _)| name.as_str());
                    if let Some(name) = socket_name {
                        Err(format!("Socket worker {socket_id} ({name}) closed"))
                    } else {
                        Err(format!("Socket worker {socket_id} closed"))
                    }
                }
                Err(RecvTimeoutError::Timeout) => Ok(None),
                Err(RecvTimeoutError::Disconnected) => {
                    Err("Socket worker channel disconnected".to_string())
                }
            },
        }
    }

    #[inline]
    /// Send a payload to a specific socket/peripheral pair.
    pub fn send(
        &mut self,
        socket_id: SocketId,
        id: PeripheralId,
        payload: &[u8],
    ) -> Result<(), String> {
        match &mut self.backend {
            Backend::SingleThreadPoller { sockets, .. } => {
                let (name, sock) = sockets
                    .get_mut(socket_id)
                    .ok_or_else(|| format!("Socket index {socket_id} out of range"))?;
                sock.send(id, payload)
                    .map_err(|e| format!("Unable to send on socket {socket_id} ({name}): {e}"))
            }
            Backend::WorkerPool { workers, .. } => {
                let (name, worker) = workers
                    .get(socket_id)
                    .ok_or_else(|| format!("Socket worker index {socket_id} out of range"))?;
                worker
                    .cmd_tx
                    .send(SocketWorkerCommand::Send {
                        id,
                        payload: payload.to_vec(),
                    })
                    .map_err(|e| format!("Unable to send on socket {socket_id} ({name}): {e}"))
            }
        }
    }

    #[cold]
    /// Broadcast a payload on the target socket.
    pub fn broadcast(&mut self, socket_id: SocketId, payload: &[u8]) -> Result<(), String> {
        match &mut self.backend {
            Backend::SingleThreadPoller { sockets, .. } => {
                let (name, sock) = sockets
                    .get_mut(socket_id)
                    .ok_or_else(|| format!("Socket index {socket_id} out of range"))?;
                sock.broadcast(payload)
                    .map_err(|e| format!("Unable to broadcast on socket {socket_id} ({name}): {e}"))
            }
            Backend::WorkerPool { workers, .. } => {
                let (name, worker) = workers
                    .get(socket_id)
                    .ok_or_else(|| format!("Socket worker index {socket_id} out of range"))?;
                worker
                    .cmd_tx
                    .send(SocketWorkerCommand::Broadcast {
                        payload: payload.to_vec(),
                    })
                    .map_err(|e| format!("Unable to broadcast on socket {socket_id} ({name}): {e}"))
            }
        }
    }

    #[cold]
    /// Update the address mapping for a peripheral on the target socket.
    pub fn update_map(
        &mut self,
        socket_id: SocketId,
        id: PeripheralId,
        token: SocketAddrToken,
    ) -> Result<(), String> {
        match &mut self.backend {
            Backend::SingleThreadPoller { sockets, .. } => {
                let (name, sock) = sockets
                    .get_mut(socket_id)
                    .ok_or_else(|| format!("Socket index {socket_id} out of range"))?;
                sock.update_map(id, token)
                    .map_err(|e| format!("Unable to update socket {socket_id} ({name}) map: {e}"))
            }
            Backend::WorkerPool { workers, .. } => {
                let (name, worker) = workers
                    .get(socket_id)
                    .ok_or_else(|| format!("Socket worker index {socket_id} out of range"))?;
                worker
                    .cmd_tx
                    .send(SocketWorkerCommand::UpdateMap { id, token })
                    .map_err(|e| format!("Unable to update socket {socket_id} ({name}) map: {e}"))
            }
        }
    }

    #[cold]
    /// Close all sockets and return ownership of the underlying socket objects.
    pub fn close(mut self) -> Vec<(String, Box<dyn Socket>)> {
        match &mut self.backend {
            Backend::SingleThreadPoller { sockets, .. } => {
                for (_, socket) in sockets.iter_mut() {
                    socket.close();
                }
                std::mem::take(sockets)
            }
            Backend::WorkerPool { workers, .. } => {
                for (_, worker) in workers.iter() {
                    let _ = worker.cmd_tx.send(SocketWorkerCommand::Close);
                }

                let mut sockets = Vec::with_capacity(workers.len());
                for (socket_id, (name, worker)) in workers.drain(..).enumerate() {
                    match worker.join() {
                        Ok(socket) => sockets.push((name, socket)),
                        Err(err) => {
                            error!("Socket worker {socket_id} ({name}) join failed: {err}");
                        }
                    }
                }
                sockets
            }
        }
    }
}

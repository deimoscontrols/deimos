//! Socket orchestration for single-thread polling or worker fan-in.

use std::time::Duration;

use crossbeam::channel::{Receiver, RecvTimeoutError, unbounded};
use tracing::error;

use crate::controller::context::ControllerCtx;
use crate::socket::worker::{SocketWorkerCommand, SocketWorkerEvent, SocketWorkerHandle};
use crate::socket::{Socket, SocketAddrToken, SocketId};
use deimos_shared::peripherals::PeripheralId;

enum Backend {
    /// Use single-threaded nonblocking polling
    /// to receive packets from sockets.
    SingleThreadPoller {
        sockets: Vec<Box<dyn Socket>>,
        next_idx: usize,
    },

    /// Use a threaded worker pool wtih OS scheduling to
    /// receive packets from sockets.
    WorkerPool {
        workers: Vec<SocketWorkerHandle>,
        events: Receiver<SocketWorkerEvent>,
    },
}

/// Unified polling interface for multiple sockets in either
/// * Performant loop method: a single-threaded 1:1 configuration, or
/// * Efficient loop method:  multithreaded N:1 fan-in configuration.
pub struct SocketOrchestrator {
    backend: Backend,
}

impl SocketOrchestrator {
    pub fn new(
        mut sockets: Vec<Box<dyn Socket>>,
        ctx: &ControllerCtx,
        worker_timeout: Duration,
    ) -> Result<Self, String> {
        // Ensure sockets are open in the current context.
        for sock in sockets.iter_mut() {
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
                for (sid, socket) in sockets.into_iter().enumerate() {
                    workers.push(SocketWorkerHandle::spawn(
                        sid,
                        socket,
                        ctx.clone(),
                        worker_timeout,
                        event_tx.clone(),
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

    pub fn socket_count(&self) -> usize {
        match &self.backend {
            Backend::SingleThreadPoller { sockets, .. } => sockets.len(),
            Backend::WorkerPool { workers, .. } => workers.len(),
        }
    }

    pub fn recv_event(&mut self, timeout: Duration) -> Result<Option<SocketWorkerEvent>, String> {
        match &mut self.backend {
            Backend::SingleThreadPoller { sockets, next_idx } => {
                if sockets.is_empty() {
                    return Ok(None);
                }
                let n = sockets.len();
                for _ in 0..n {
                    let idx = *next_idx;
                    *next_idx = (*next_idx + 1) % n;
                    if let Some(packet) = sockets[idx].recv(Duration::ZERO) {
                        return Ok(Some(SocketWorkerEvent::Packet {
                            socket_id: idx,
                            packet,
                        }));
                    }
                }
                if timeout.is_zero() {
                    return Ok(None);
                }
                std::thread::sleep(timeout);
                Ok(None)
            }
            Backend::WorkerPool { events, .. } => match events.recv_timeout(timeout) {
                Ok(event) => Ok(Some(event)),
                Err(RecvTimeoutError::Timeout) => Ok(None),
                Err(RecvTimeoutError::Disconnected) => {
                    Err("Socket worker channel disconnected".to_string())
                }
            },
        }
    }

    pub fn send(
        &mut self,
        socket_id: SocketId,
        id: PeripheralId,
        buffer: crate::buffer_pool::BufferLease<crate::buffer_pool::SocketBuffer>,
        size: usize,
    ) -> Result<(), String> {
        match &mut self.backend {
            Backend::SingleThreadPoller { sockets, .. } => {
                let sock = sockets
                    .get_mut(socket_id)
                    .ok_or_else(|| format!("Socket index {socket_id} out of range"))?;
                let payload = buffer.as_ref();
                sock.send(id, &payload[..size])
            }
            Backend::WorkerPool { workers, .. } => workers
                .get(socket_id)
                .ok_or_else(|| format!("Socket worker index {socket_id} out of range"))?
                .cmd_tx
                .send(SocketWorkerCommand::Send { id, buffer, size })
                .map_err(|e| format!("Unable to send on socket {socket_id}: {e}")),
        }
    }

    pub fn broadcast(
        &mut self,
        socket_id: SocketId,
        buffer: crate::buffer_pool::BufferLease<crate::buffer_pool::SocketBuffer>,
        size: usize,
    ) -> Result<(), String> {
        match &mut self.backend {
            Backend::SingleThreadPoller { sockets, .. } => {
                let sock = sockets
                    .get_mut(socket_id)
                    .ok_or_else(|| format!("Socket index {socket_id} out of range"))?;
                let payload = buffer.as_ref();
                sock.broadcast(&payload[..size])
            }
            Backend::WorkerPool { workers, .. } => workers
                .get(socket_id)
                .ok_or_else(|| format!("Socket worker index {socket_id} out of range"))?
                .cmd_tx
                .send(SocketWorkerCommand::Broadcast { buffer, size })
                .map_err(|e| format!("Unable to broadcast on socket {socket_id}: {e}")),
        }
    }

    pub fn update_map(
        &mut self,
        socket_id: SocketId,
        id: PeripheralId,
        token: SocketAddrToken,
    ) -> Result<(), String> {
        match &mut self.backend {
            Backend::SingleThreadPoller { sockets, .. } => {
                let sock = sockets
                    .get_mut(socket_id)
                    .ok_or_else(|| format!("Socket index {socket_id} out of range"))?;
                sock.update_map(id, token)
            }
            Backend::WorkerPool { workers, .. } => workers
                .get(socket_id)
                .ok_or_else(|| format!("Socket worker index {socket_id} out of range"))?
                .cmd_tx
                .send(SocketWorkerCommand::UpdateMap { id, token })
                .map_err(|e| format!("Unable to update socket {socket_id} map: {e}")),
        }
    }

    pub fn close(mut self) -> Vec<Box<dyn Socket>> {
        match &mut self.backend {
            Backend::SingleThreadPoller { sockets, .. } => {
                for socket in sockets.iter_mut() {
                    socket.close();
                }
                std::mem::take(sockets)
            }
            Backend::WorkerPool { workers, .. } => {
                for worker in workers.iter() {
                    let _ = worker.cmd_tx.send(SocketWorkerCommand::Close);
                }

                let mut sockets = Vec::with_capacity(workers.len());
                for worker in workers.drain(..) {
                    match worker.join() {
                        Ok(socket) => sockets.push(socket),
                        Err(err) => {
                            error!("{err}");
                            continue;
                        }
                    }
                }
                sockets
            }
        }
    }
}

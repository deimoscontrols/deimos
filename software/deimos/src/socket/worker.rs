//! Socket worker that runs a socket on its own thread.

use std::thread::{JoinHandle, spawn};
use std::time::Duration;

use crossbeam::channel::{Receiver, Sender, TryRecvError, unbounded};
use tracing::warn;

use crate::buffer_pool::{BufferLease, SocketBuffer};
use crate::controller::context::ControllerCtx;
use deimos_shared::peripherals::PeripheralId;

use super::{Socket, SocketAddrToken, SocketId, SocketPacket};

pub enum SocketWorkerCommand {
    Send {
        id: PeripheralId,
        buffer: BufferLease<SocketBuffer>,
        size: usize,
    },
    Broadcast {
        buffer: BufferLease<SocketBuffer>,
        size: usize,
    },
    UpdateMap {
        id: PeripheralId,
        token: SocketAddrToken,
    },
    Close,
}

pub enum SocketWorkerEvent {
    Packet {
        socket_id: SocketId,
        packet: SocketPacket,
    },
    Error {
        socket_id: SocketId,
        error: String,
    },
    Closed {
        socket_id: SocketId,
    },
}

pub struct SocketWorker {
    socket_id: SocketId,
    socket: Box<dyn Socket>,
    ctx: ControllerCtx,
    recv_timeout: Duration,
    cmd_rx: Receiver<SocketWorkerCommand>,
    event_tx: Sender<SocketWorkerEvent>,
}

impl SocketWorker {
    pub fn new(
        socket_id: SocketId,
        socket: Box<dyn Socket>,
        ctx: ControllerCtx,
        recv_timeout: Duration,
        cmd_rx: Receiver<SocketWorkerCommand>,
        event_tx: Sender<SocketWorkerEvent>,
    ) -> Self {
        Self {
            socket_id,
            socket,
            ctx,
            recv_timeout,
            cmd_rx,
            event_tx,
        }
    }

    pub fn run(mut self) -> Box<dyn Socket> {
        // Set core affinity
        let core_ids = core_affinity::get_core_ids().unwrap_or_default();
        if let Some(core_id) = core_ids.get(3) {
            if !core_affinity::set_for_current(*core_id) {
                warn!("Failed to set core affinity for socket worker");
            }
        }

        // Open socket
        if !self.socket.is_open() {
            if let Err(err) = self.socket.open(&self.ctx) {
                let _ = self.event_tx.send(SocketWorkerEvent::Error {
                    socket_id: self.socket_id,
                    error: err,
                });
                let _ = self.event_tx.send(SocketWorkerEvent::Closed {
                    socket_id: self.socket_id,
                });
                return self.socket;
            }
        }

        // I/O loop
        loop {
            // Process all queued incoming messages from the controller,
            // sending packets to socket workers if requested
            let (keep_running, handled_commands) = self.drain_commands();
            if !keep_running {
                // If the inner socket is disconnected,
                // exit the receive loop.
                break;
            }

            // Check for incoming packets from peripherals
            if handled_commands {
                if let Some(packet) = self.socket.recv(Duration::ZERO) {
                    if self
                        .event_tx
                        .send(SocketWorkerEvent::Packet {
                            socket_id: self.socket_id,
                            packet,
                        })
                        .is_err()
                    {
                        // If we're unable to send, that's because the channel has closed
                        // and we are shutting down.
                        break;
                    }
                }
                continue;
            }

            if let Some(packet) = self.socket.recv(self.recv_timeout) {
                if self
                    .event_tx
                    .send(SocketWorkerEvent::Packet {
                        socket_id: self.socket_id,
                        packet,
                    })
                    .is_err()
                {
                    // If we're unable to send, that's because the channel has closed
                    // and we are shutting down.
                    break;
                }
            }
        }

        // Exit the loop and close the socket.
        self.socket.close();
        let _ = self.event_tx.send(SocketWorkerEvent::Closed {
            socket_id: self.socket_id,
        });

        self.socket
    }

    // Process incoming messages from the controller
    fn drain_commands(&mut self) -> (bool, bool) {
        let mut handled_any = false;
        loop {
            match self.cmd_rx.try_recv() {
                Ok(command) => {
                    handled_any = true;
                    if !self.handle_command(command) {
                        return (false, handled_any);
                    }
                }
                Err(TryRecvError::Empty) => return (true, handled_any),
                Err(TryRecvError::Disconnected) => return (false, handled_any),
            }
        }
    }

    // Process a specific incoming message from the controller
    fn handle_command(&mut self, command: SocketWorkerCommand) -> bool {
        let result = match command {
            SocketWorkerCommand::Send { id, buffer, size } => {
                let payload = buffer.as_ref();
                if size > payload.len() {
                    return self
                        .event_tx
                        .send(SocketWorkerEvent::Error {
                            socket_id: self.socket_id,
                            error: format!(
                                "Socket worker send buffer too small: {size} > {}",
                                payload.len()
                            ),
                        })
                        .is_ok();
                }
                self.socket.send(id, &payload[..size])
            }
            SocketWorkerCommand::Broadcast { buffer, size } => {
                let payload = buffer.as_ref();
                if size > payload.len() {
                    return self
                        .event_tx
                        .send(SocketWorkerEvent::Error {
                            socket_id: self.socket_id,
                            error: format!(
                                "Socket worker broadcast buffer too small: {size} > {}",
                                payload.len()
                            ),
                        })
                        .is_ok();
                }
                self.socket.broadcast(&payload[..size])
            }
            SocketWorkerCommand::UpdateMap { id, token } => self.socket.update_map(id, token),
            SocketWorkerCommand::Close => return false,
        };

        if let Err(err) = result {
            return self
                .event_tx
                .send(SocketWorkerEvent::Error {
                    socket_id: self.socket_id,
                    error: err,
                })
                .is_ok();
        }

        true
    }
}

pub struct SocketWorkerHandle {
    pub cmd_tx: Sender<SocketWorkerCommand>,
    thread: JoinHandle<Box<dyn Socket>>,
}

impl SocketWorkerHandle {
    pub fn spawn(
        socket_id: SocketId,
        socket: Box<dyn Socket>,
        ctx: ControllerCtx,
        recv_timeout: Duration,
        event_tx: Sender<SocketWorkerEvent>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = unbounded();
        let worker = SocketWorker::new(socket_id, socket, ctx, recv_timeout, cmd_rx, event_tx);
        let thread = spawn(move || worker.run());
        Self { cmd_tx, thread }
    }

    pub fn join(self) -> Result<Box<dyn Socket>, String> {
        self.thread
            .join()
            .map_err(|_| "Socket worker thread panicked".to_string())
    }
}

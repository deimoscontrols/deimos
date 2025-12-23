//! Socket worker that runs a socket on its own thread.

use std::thread::{JoinHandle, spawn};
use std::time::Duration;

use crossbeam::channel::{Receiver, Sender, TryRecvError, unbounded};

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

        loop {
            if !self.drain_commands() {
                break;
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
                    break;
                }
            }
        }

        self.socket.close();
        let _ = self.event_tx.send(SocketWorkerEvent::Closed {
            socket_id: self.socket_id,
        });

        self.socket
    }

    fn drain_commands(&mut self) -> bool {
        loop {
            match self.cmd_rx.try_recv() {
                Ok(command) => {
                    if !self.handle_command(command) {
                        return false;
                    }
                }
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
    }

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

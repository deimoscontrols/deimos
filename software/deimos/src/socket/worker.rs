//! Socket worker that runs a socket on its own thread.
//!
//! The worker owns a socket, receives controller commands over a channel, and
//! forwards inbound packets to the orchestrator as events.

use std::thread::{Builder, JoinHandle};
use std::time::Duration;

use crossbeam::channel::{Receiver, Sender, TryRecvError, unbounded};

use crate::SOCKET_BUFFER_LEN;
use crate::controller::context::ControllerCtx;
use deimos_shared::peripherals::PeripheralId;

use super::{Socket, SocketAddrToken, SocketId, SocketPacketMeta};

/// Commands sent to a socket worker by the orchestrator.
pub enum SocketWorkerCommand {
    /// Send a payload to a peripheral ID.
    Send { id: PeripheralId, payload: Vec<u8> },
    /// Broadcast a payload on the socket.
    Broadcast { payload: Vec<u8> },
    /// Update the socket's address map for a peripheral.
    UpdateMap {
        id: PeripheralId,
        token: SocketAddrToken,
    },
    /// Shut down the worker.
    Close,
}

/// Events emitted by a socket worker to the orchestrator.
pub enum SocketWorkerEvent {
    /// Incoming packet data and metadata.
    Packet {
        socket_id: SocketId,
        meta: SocketPacketMeta,
        payload: Vec<u8>,
    },
    /// Error from the worker's socket I/O path.
    Error { socket_id: SocketId, error: String },
    /// Worker has closed its socket and is exiting.
    Closed { socket_id: SocketId },
}

/// Worker instance that runs a socket on a dedicated thread.
pub struct SocketWorker {
    socket_id: SocketId,
    socket: Box<dyn Socket>,
    ctx: ControllerCtx,
    recv_timeout: Duration,
    cmd_rx: Receiver<SocketWorkerCommand>,
    event_tx: Sender<SocketWorkerEvent>,
}

impl SocketWorker {
    /// Construct a new worker with its socket, channels, and timing configuration.
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

    /// Run the worker loop until shutdown, returning the owned socket.
    pub fn run(mut self) -> Box<dyn Socket> {
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
                let mut payload = vec![0_u8; SOCKET_BUFFER_LEN];
                if let Some(meta) = self.socket.recv(&mut payload, Duration::ZERO) {
                    payload.truncate(meta.size);
                    if self
                        .event_tx
                        .send(SocketWorkerEvent::Packet {
                            socket_id: self.socket_id,
                            meta,
                            payload,
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

            let mut payload = vec![0_u8; SOCKET_BUFFER_LEN];
            if let Some(meta) = self.socket.recv(&mut payload, self.recv_timeout) {
                payload.truncate(meta.size);
                if self
                    .event_tx
                    .send(SocketWorkerEvent::Packet {
                        socket_id: self.socket_id,
                        meta,
                        payload,
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

    /// Drain queued commands from the controller.
    ///
    /// Returns `(keep_running, handled_any)` where `keep_running` indicates
    /// whether the worker should continue and `handled_any` tracks if a command
    /// was processed in this cycle.
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

    /// Process a single controller command.
    ///
    /// Returns `false` when the worker should shut down.
    fn handle_command(&mut self, command: SocketWorkerCommand) -> bool {
        let result = match command {
            SocketWorkerCommand::Send { id, payload } => self.socket.send(id, payload.as_slice()),
            SocketWorkerCommand::Broadcast { payload } => self.socket.broadcast(payload.as_slice()),
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

/// Transmission channel and join handle for a running socket worker.
pub struct SocketWorkerHandle {
    pub cmd_tx: Sender<SocketWorkerCommand>,
    thread: JoinHandle<Box<dyn Socket>>,
}

impl SocketWorkerHandle {
    /// Spin up a new socket worker on its own thread.
    pub fn spawn(
        socket_id: SocketId,
        socket: Box<dyn Socket>,
        ctx: ControllerCtx,
        recv_timeout: Duration,
        event_tx: Sender<SocketWorkerEvent>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = unbounded();
        let worker = SocketWorker::new(socket_id, socket, ctx, recv_timeout, cmd_rx, event_tx);
        let thread = Builder::new()
            .name(format!("socket-worker-{socket_id}"))
            .spawn(move || worker.run())
            .expect("Failed to spawn socket worker thread");
        Self { cmd_tx, thread }
    }

    /// Wait for the socket worker thread to complete.
    pub fn join(self) -> Result<Box<dyn Socket>, String> {
        self.thread
            .join()
            .map_err(|_| "Socket worker thread panicked".to_string())
    }
}

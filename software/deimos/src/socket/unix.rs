//! Implementation of Socket trait for stdlib unix datagram socket,
//! which provides inter-process communication for peripherals that are
//! defined in software, or a bridge to an arbitrary data source.

use std::collections::BTreeMap;
use std::os::unix::net; //{SocketAddr, UnixDatagram};
use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::*;
use deimos_shared::peripherals::PeripheralId;

/// Implementation of Socket trait for stdlib UDP socket on IPV4
#[derive(Serialize, Deserialize, Default)]
pub struct UnixSocket {
    /// The name of the socket will be combined with the op directory
    /// to make a socket address like {op_dir}/sock/{name} .
    /// Peripheral sockets are expected in {op_dir}/sock/per/* .
    ///
    /// Because unix sockets have a maximum path length of 94-108 characters
    /// depending on platform, the name of the socket should be as short
    /// as possible, like `ctrl`, to prevent errors when attempting to run
    /// the controller in different folder structures.
    name: String,

    #[serde(skip)]
    socket: Option<net::UnixDatagram>,
    #[serde(skip)]
    rxbuf: Vec<u8>,
    #[serde(skip)]
    addrs: BTreeMap<PeripheralId, PathBuf>,
    #[serde(skip)]
    pids: BTreeMap<PathBuf, PeripheralId>,
    #[serde(skip)]
    last_received_addr: Option<PathBuf>,
    #[serde(skip)]
    ctx: ControllerCtx,
}

impl UnixSocket {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            rxbuf: vec![0; 1522],
            socket: None,
            addrs: BTreeMap::new(),
            pids: BTreeMap::new(),
            last_received_addr: None,
            ctx: ControllerCtx::default(),
        }
    }

    /// Socket name, which is used to build the socket address.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The path to the socket, at {op_dir}/sock/{name}
    pub fn path(&self) -> PathBuf {
        self.ctx.op_dir.join("sock").join(&self.name)
    }

    /// A unix socket address made from the socket's name and the op directory.
    ///
    /// # Errors
    ///
    /// * If socket path length exceeds platform maximum characters
    ///   for a unix socket (about 94-108 depending on platform)
    pub fn addr(&self) -> Result<net::SocketAddr, String> {
        net::SocketAddr::from_pathname(self.path())
            .map_err(|e| format!("Unable to form socket address for `{}`: {}", self.name, e))
    }

    /// Directory where peripheral sockets are expected
    pub fn peripheral_socket_dir(&self) -> PathBuf {
        self.ctx.op_dir.join("sock").join("per")
    }
}

#[typetag::serde]
impl Socket for UnixSocket {
    fn is_open(&self) -> bool {
        self.socket.is_some()
    }

    fn open(&mut self, ctx: &ControllerCtx) -> Result<(), String> {
        if self.socket.is_none() {
            self.ctx = ctx.clone();
            // Create the socket folders if they don't already exist
            std::fs::create_dir_all(self.ctx.op_dir.join("sock"))
                .map_err(|e| format!("Unable to create socket folders: {e}"))?;
            std::fs::create_dir_all(self.peripheral_socket_dir())
                .map_err(|e| format!("Unable to create socket folders: {e}"))?;

            // Bind the socket
            let socket = net::UnixDatagram::bind(self.path())
                .map_err(|e| format!("Unable to bind unix socket: {e}"))?;
            socket
                .set_nonblocking(true)
                .map_err(|e| format!("Unable to set unix socket to nonblocking mode: {e}"))?;
            self.socket = Some(socket);
        } else {
            return Err("Socket already open".to_string());
        }

        Ok(())
    }

    fn close(&mut self) {
        // Drop inner socket, releasing port
        let path = self.path();
        self.socket = None;
        self.addrs.clear();
        self.pids.clear();
        self.last_received_addr = None;
        self.ctx = ControllerCtx::default();
        // Attempt to delete socket file so that it is not left dangling.
        // This may fail on permissions.
        let _ = std::fs::remove_file(path);
    }

    fn send(&mut self, id: PeripheralId, msg: &[u8]) -> Result<(), String> {
        // Get the IP address
        let addr = self
            .addrs
            .get(&id)
            .ok_or(format!("Peripheral not present in address map: {id:?}"))?;

        // Get socket
        let sock = self
            .socket
            .as_mut()
            .ok_or("Unable to send before socket is bound".to_string())?;

        // Send unicast
        sock.send_to(msg, addr)
            .map_err(|e| format!("Failed to send packet: {e}"))?;

        Ok(())
    }

    fn recv(&mut self) -> Option<(Option<PeripheralId>, Instant, &[u8])> {
        // Check if there is anything to receive,
        // and filter out packets from unexpected source port
        let (size, src_path, time) = match self.socket.as_mut() {
            Some(sock) => match sock.recv_from(&mut self.rxbuf).ok() {
                Some((size, addr)) => {
                    // Mark the time ASAP
                    let now = Instant::now();

                    if let Some(src_path) = addr.as_pathname() {
                        // TODO: eliminate allocation here by copying into a reusable buffer
                        let src_path = src_path.to_owned();
                        (size, src_path, now)
                    } else {
                        return None;
                    }
                }
                None => return None,
            },
            None => return None,
        };

        self.last_received_addr = Some(src_path.to_owned());

        // Check if we already know which peripheral this is
        let pid = self.pids.get(&src_path).copied();

        Some((pid, time, &self.rxbuf[..size]))
    }

    fn broadcast(&mut self, msg: &[u8]) -> Result<(), String> {
        // Figure out where to find peripheral sockets
        let dir = self.peripheral_socket_dir();

        // Get socket
        let sock = self
            .socket
            .as_mut()
            .ok_or("Unable to send before socket is bound".to_string())?;

        // Collect sockets in {op_dir}/sock/per/*
        if dir.exists() {
            // Paths that may be a dir or file
            let paths = std::fs::read_dir(dir)
                .map_err(|e| format!("Unable to read peripheral socket dir: {e}"))?;
            // Files that may or may not be unix sockets
            let files = paths.filter_map(|entry| {
                if let Ok(entry) = entry {
                    let p = entry.path();
                    // Sockets are neither a file nor a directory
                    match p.is_dir() || p.is_file() {
                        true => None,
                        false => Some(p),
                    }
                } else {
                    None
                }
            });

            // Try to send to each file, since we don't have a rigorous way to check
            // which ones are unix sockets and which ones are not
            for f in files {
                sock.send_to(msg, &f)
                    .map_err(|e| format!("Failed to send unix socket packet: {e}"))?;
            }
        }

        Ok(())
    }

    fn update_map(&mut self, id: PeripheralId) -> Result<(), String> {
        if let Some(addr) = &self.last_received_addr {
            self.addrs.insert(id, addr.clone());
            self.pids.insert(addr.clone(), id);

            if self.addrs.len() != self.pids.len() {
                return Err(format!(
                    "Duplicate addresses or peripheral IDs detected.\nAddress map: {:?}\nPeripheral ID map: {:?}",
                    &self.addrs, &self.pids
                ));
            }
        }

        Ok(())
    }
}

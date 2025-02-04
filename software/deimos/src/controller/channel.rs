//! Serializable thread channel to allow user sideloading
//! of comms between the controller's appendages

use crossbeam::channel::{bounded, Receiver, Sender};
use serde::{Deserialize, Serialize};

/// A basic set of message types that can be passed along a user channel
#[derive(Debug)]
#[non_exhaustive]
pub enum Msg {
    Val(f64),
    Arr(Vec<f64>),
    Str(String),
}

/// Default-able one-way channel with 10-message buffer
#[derive(Clone, Debug)]
struct ChannelInner {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

impl ChannelInner {
    fn from_handles(tx: Sender<Msg>, rx: Receiver<Msg>) -> Self {
        Self { tx, rx }
    }
}

impl Default for ChannelInner {
    fn default() -> Self {
        let (tx, rx) = bounded(10);
        Self { tx, rx }
    }
}

/// A multiple-producer, multiple-consumer (MPMC) bidirectional message pipe
/// that will be reinitialized (but not reconnected to any particular
/// endpoints) when deserialized.
///
/// The channel buffers hold a maximum of 10 messages.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Channel {
    #[serde(skip)]
    ch0: ChannelInner,
    #[serde(skip)]
    ch1: ChannelInner,
}

impl Channel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a handle for sources,
    /// which can to send to sinks and receive from sinks
    pub fn source_endpoint(&self) -> Endpoint {
        Endpoint::new(self.ch0.tx.clone(), self.ch1.rx.clone())
    }

    /// Get a handle for sinks,
    /// which can send to sources and receive from sources
    pub fn sink_endpoint(&self) -> Endpoint {
        Endpoint::new(self.ch1.tx.clone(), self.ch0.rx.clone())
    }
}

/// Channel endpoint for either a source or sink.
///
/// The channel buffers hold a maximum of 10 messages.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Endpoint {
    #[serde(skip)]
    ch: ChannelInner,
}

impl Endpoint {
    pub fn new(tx: Sender<Msg>, rx: Receiver<Msg>) -> Self {
        Self {
            ch: ChannelInner::from_handles(tx, rx),
        }
    }

    /// Get a sender handle
    pub fn tx(&self) -> &Sender<Msg> {
        &self.ch.tx
    }

    /// Get a receiver handle
    pub fn rx(&self) -> &Receiver<Msg> {
        &self.ch.rx
    }
}

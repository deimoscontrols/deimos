//! Fixed-storage, cycle-budgeted Modbus/TCP request handling.
//!
//! References:
//!   \[1\] Modbus Organization, *MODBUS Application Protocol Specification
//!   V1.1b3*, 2012.
//!   \[2\] Modbus Organization, *MODBUS Messaging on TCP/IP Implementation
//!   Guide V1.0b*, 2006.

use deimos_shared::peripherals::deimos_daq_rev7::{
    ModbusInitialConfig, OperatingSnapshot,
    modbus::{
        HOLDING_REGISTER_COUNT, MAX_HOLDING_WRITE_REGISTERS, MODBUS_MAX_READ_REGISTERS,
        MODBUS_MAX_WRITE_REGISTERS, SNAPSHOT_INPUT_BYTE_COUNT, SNAPSHOT_INPUT_REGISTER_COUNT,
        apply_holding_write, holding_registers, snapshot_input_registers,
        write_snapshot_input_register_bytes,
    },
};
use rmodbus::{
    ErrorKind, ModbusProto, VectorTrait,
    consts::{ModbusErrorCode, ModbusFunction},
    server::ModbusFrame,
};

use super::subsystems::net::Net;

/// Largest ADU accepted by the selected no-alloc rmodbus codec.
const MODBUS_ADU_CAPACITY: usize = 256;
/// Bytes required before the MBAP length field can define the frame boundary.
const MBAP_PREFIX_LEN: usize = 6;
/// Lowest legal MBAP length: one unit byte and one function byte.
const MIN_MBAP_LENGTH: usize = 2;
/// Highest MBAP length accepted by rmodbus's fixed frame representation.
const MAX_MBAP_LENGTH: usize = MODBUS_ADU_CAPACITY - MBAP_PREFIX_LEN;
/// Maximum socket receive calls in one publishing cycle.
const MAX_RX_CALLS_PER_CYCLE: u8 = 2;
/// Maximum socket transmit calls in one publishing cycle.
const MAX_TX_CALLS_PER_CYCLE: u8 = 2;

/// Remaining socket operations available to one binding or operating cycle.
pub(super) struct ModbusSocketBudget {
    /// Remaining TCP receive calls in this publishing cycle.
    rx_calls: u8,
    /// Remaining TCP transmit calls in this publishing cycle.
    tx_calls: u8,
}

impl ModbusSocketBudget {
    /// Build the fixed two-receive/two-transmit per-cycle allowance.
    pub(super) fn new() -> Self {
        Self {
            rx_calls: MAX_RX_CALLS_PER_CYCLE,
            tx_calls: MAX_TX_CALLS_PER_CYCLE,
        }
    }
}

/// Result of receiving at most one staged TCP ADU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReceiveStatus {
    /// No complete request is available yet.
    Incomplete,
    /// Exactly one complete request is retained for processing.
    Complete,
    /// The MBAP header cannot define a valid bounded frame.
    Malformed,
    /// The peer closed or invalidated the TCP receive half.
    Disconnected,
}

/// Result of processing one complete application request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RequestOutcome {
    /// Whether the request was supported and passed all map/value checks.
    pub(super) accepted: bool,
    /// Complete retained configuration after applying an accepted write.
    pub(super) config: ModbusInitialConfig,
    /// Transaction identifier echoed in the generated response.
    pub(super) transaction_id: u16,
}

/// Fixed-capacity byte vector implementing rmodbus's response abstraction.
struct FixedResponse {
    /// Response ADU storage with shape `(MODBUS_ADU_CAPACITY,)`.
    bytes: [u8; MODBUS_ADU_CAPACITY],
    /// Initialized prefix length within `bytes`, in bytes.
    len: usize,
}

impl FixedResponse {
    /// Construct empty fixed-capacity response storage.
    const fn new() -> Self {
        Self {
            bytes: [0; MODBUS_ADU_CAPACITY],
            len: 0,
        }
    }
}

impl VectorTrait<u8> for FixedResponse {
    fn push(&mut self, value: u8) -> Result<(), ErrorKind> {
        if self.len == self.bytes.len() {
            return Err(ErrorKind::OOB);
        }
        self.bytes[self.len] = value;
        self.len += 1;
        Ok(())
    }

    fn extend(&mut self, other: &[u8]) -> Result<(), ErrorKind> {
        let end = self.len.checked_add(other.len()).ok_or(ErrorKind::OOB)?;
        if end > self.bytes.len() {
            return Err(ErrorKind::OOB);
        }
        self.bytes[self.len..end].copy_from_slice(other);
        self.len = end;
        Ok(())
    }

    fn len(&self) -> usize {
        self.len
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn clear(&mut self) {
        self.len = 0;
    }

    fn cut_end(&mut self, len_to_cut: usize, _value: u8) {
        self.len = self.len.saturating_sub(len_to_cut);
    }

    fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.bytes[..self.len]
    }

    fn resize(&mut self, new_len: usize, value: u8) -> Result<(), ErrorKind> {
        if new_len > self.bytes.len() {
            return Err(ErrorKind::OOB);
        }
        if new_len > self.len {
            self.bytes[self.len..new_len].fill(value);
        }
        self.len = new_len;
        Ok(())
    }

    fn replace(&mut self, index: usize, value: u8) {
        debug_assert!(index < self.len);
        self.bytes[index] = value;
    }
}

/// Board-owned Modbus framing state which survives state-machine re-entry.
pub(super) struct ModbusTcpServer {
    /// One staged request ADU with shape `(MODBUS_ADU_CAPACITY,)`.
    request: [u8; MODBUS_ADU_CAPACITY],
    /// Number of initialized request bytes.
    request_len: usize,
    /// Complete ADU length obtained from the validated MBAP prefix.
    expected_len: Option<usize>,
    /// One response retained until it has entered the smoltcp TX ring.
    response: FixedResponse,
    /// First response byte not yet copied into the smoltcp TX ring.
    response_offset: usize,
    /// Defaults overlaid by the first accepted request during Binding.
    binding_config: Option<ModbusInitialConfig>,
    /// Rate-change configuration retained until its response is enqueued.
    reentry_config: Option<ModbusInitialConfig>,
}

impl ModbusTcpServer {
    /// Construct empty fixed-capacity request and response state.
    pub(super) const fn new() -> Self {
        Self {
            request: [0; MODBUS_ADU_CAPACITY],
            request_len: 0,
            expected_len: None,
            response: FixedResponse::new(),
            response_offset: 0,
            binding_config: None,
            reentry_config: None,
        }
    }

    /// Discard all application framing state when the TCP socket is reset.
    pub(super) fn reset(&mut self) {
        self.reset_request();
        self.response.clear();
        self.response_offset = 0;
        self.binding_config = None;
        self.reentry_config = None;
    }

    /// Return whether a response still needs to be copied into smoltcp's TX queue.
    pub(super) fn response_pending(&self) -> bool {
        self.response_offset < self.response.len
    }

    /// Return whether one complete request is retained at the head of the stream.
    pub(super) fn request_complete(&self) -> bool {
        self.expected_len == Some(self.request_len)
    }

    /// Receive at most two socket slices and at most one complete ADU.
    ///
    /// The first call reads only through the six-byte MBAP prefix. Once its
    /// length is validated, a second call reads no farther than that ADU's
    /// declared end, leaving any pipelined request in the socket ring.
    ///
    /// Args:
    ///   net: Board network subsystem owning the one TCP socket.
    ///   budget: Remaining socket-call allowance for the current cycle.
    ///
    /// Returns:
    ///   Current bounded receive status.
    pub(super) fn receive(
        &mut self,
        net: &mut Net<'_>,
        budget: &mut ModbusSocketBudget,
    ) -> ReceiveStatus {
        if self.request_complete() {
            return ReceiveStatus::Complete;
        }
        if self.response_pending() {
            return ReceiveStatus::Incomplete;
        }

        for _ in 0..MAX_RX_CALLS_PER_CYCLE {
            if budget.rx_calls == 0 {
                break;
            }
            if self.parse_mbap_prefix().is_err() {
                return ReceiveStatus::Malformed;
            }

            let target_len = self.expected_len.unwrap_or(MBAP_PREFIX_LEN);
            if self.request_len == target_len {
                return ReceiveStatus::Complete;
            }

            budget.rx_calls -= 1;
            match net.tcp_recv(&mut self.request[self.request_len..target_len]) {
                Ok(0) => break,
                Ok(received) => self.request_len += received,
                Err(_) => return ReceiveStatus::Disconnected,
            }
        }

        if self.parse_mbap_prefix().is_err() {
            return ReceiveStatus::Malformed;
        }

        if self.request_complete() {
            ReceiveStatus::Complete
        } else {
            ReceiveStatus::Incomplete
        }
    }

    /// Copy a pending response into smoltcp with at most two send calls.
    ///
    /// Args:
    ///   net: Board network subsystem owning the one TCP socket.
    ///   budget: Remaining socket-call allowance for the current cycle.
    ///
    /// Returns:
    ///   `Ok(())` after exhausting the call allowance without a socket error;
    ///   [`Self::response_pending`] reports whether backpressure retained bytes.
    pub(super) fn send_response(
        &mut self,
        net: &mut Net<'_>,
        budget: &mut ModbusSocketBudget,
    ) -> Result<(), ()> {
        for _ in 0..MAX_TX_CALLS_PER_CYCLE {
            if !self.response_pending() || budget.tx_calls == 0 {
                break;
            }
            budget.tx_calls -= 1;
            match net.tcp_send(&self.response.bytes[self.response_offset..self.response.len]) {
                Ok(0) => break,
                Ok(sent) => self.response_offset += sent,
                Err(_) => return Err(()),
            }
        }
        if !self.response_pending() {
            self.response.clear();
            self.response_offset = 0;
        }
        Ok(())
    }

    /// Validate the first request and retain accepted traffic for operating entry.
    ///
    /// Rejected but structurally framed requests consume the ADU and retain a
    /// standard exception response for binding to transmit. Accepted requests
    /// retain their bytes and discard the provisional response so the first
    /// operating cycle can answer from its newly published snapshot.
    pub(super) fn inspect_binding_request(&mut self) -> Result<RequestOutcome, ()> {
        let defaults = ModbusInitialConfig::default();
        let outcome = self.process_request(&OperatingSnapshot::default(), defaults, 0, false)?;
        if outcome.accepted {
            self.binding_config = Some(outcome.config);
        }
        Ok(outcome)
    }

    /// Process and consume one complete request against the current board state.
    ///
    /// Args:
    ///   snapshot: Latest immutable engineering snapshot.
    ///   config: Complete retained Modbus configuration before this request.
    ///   loss_of_contact_counter: Current unanswered-cycle count.
    ///
    /// Returns:
    ///   Request acceptance and resulting state, or an internal codec error.
    pub(super) fn process_operating_request(
        &mut self,
        snapshot: &OperatingSnapshot,
        config: ModbusInitialConfig,
        loss_of_contact_counter: u16,
    ) -> Result<RequestOutcome, ()> {
        self.process_request(snapshot, config, loss_of_contact_counter, true)
    }

    /// Return the configuration selected by the accepted binding request.
    pub(super) fn take_binding_config(&mut self) -> Option<ModbusInitialConfig> {
        self.binding_config.take()
    }

    /// Defer rate-change re-entry until its response is fully enqueued.
    pub(super) fn set_reentry_config(&mut self, config: ModbusInitialConfig) {
        self.reentry_config = Some(config);
    }

    /// Return whether an accepted rate change is waiting for response enqueue.
    pub(super) fn reentry_pending(&self) -> bool {
        self.reentry_config.is_some()
    }

    /// Take a pending rate-change configuration after response enqueue completes.
    pub(super) fn take_reentry_config(&mut self) -> Option<ModbusInitialConfig> {
        self.reentry_config.take()
    }

    /// Parse the staged MBAP prefix exactly once when all six bytes are present.
    ///
    /// Returns:
    ///   `Ok(())` while the prefix is incomplete or after recording a valid ADU
    ///   length; `Err(())` for a nonzero protocol ID or unsupported ADU length.
    fn parse_mbap_prefix(&mut self) -> Result<(), ()> {
        if self.expected_len.is_some() || self.request_len != MBAP_PREFIX_LEN {
            return Ok(());
        }
        let protocol = u16::from_be_bytes([self.request[2], self.request[3]]);
        let mbap_length = usize::from(u16::from_be_bytes([self.request[4], self.request[5]]));
        if protocol != 0 || !(MIN_MBAP_LENGTH..=MAX_MBAP_LENGTH).contains(&mbap_length) {
            return Err(());
        }
        self.expected_len = Some(MBAP_PREFIX_LEN + mbap_length);
        Ok(())
    }

    /// Validate, answer, and optionally consume the one staged request.
    ///
    /// This bounded dispatcher resides in ITCM because continuous full-snapshot
    /// requests are the worst supported communication workload at 500 Hz.
    ///
    /// Args:
    ///   snapshot: Latest immutable engineering snapshot.
    ///   config: Complete retained configuration before this request.
    ///   loss_of_contact_counter: Current unanswered-cycle count.
    ///   consume_accepted: Whether an accepted request should be removed now;
    ///     Binding leaves it staged for the first operating publication.
    ///
    /// Returns:
    ///   Acceptance state, resulting configuration, and transaction ID, or an
    ///   internal codec error which requires resetting the connection.
    #[inline(never)]
    #[unsafe(link_section = ".itcm.modbus_request")]
    fn process_request(
        &mut self,
        snapshot: &OperatingSnapshot,
        config: ModbusInitialConfig,
        loss_of_contact_counter: u16,
        consume_accepted: bool,
    ) -> Result<RequestOutcome, ()> {
        if !self.request_complete() || self.request_len < 8 {
            return Err(());
        }
        let transaction_id = u16::from_be_bytes([self.request[0], self.request[1]]);
        let unit_id = self.request[6];
        let function = self.request[7];

        if let Err(error) = validate_supported_pdu(&self.request[..self.request_len]) {
            self.queue_exception(unit_id, function, error);
            self.reset_request();
            return Ok(RequestOutcome {
                accepted: false,
                config,
                transaction_id,
            });
        }

        // rmodbus treats unit identifiers 0 and 255 as non-responsive serial
        // broadcasts. Modbus/TCP on this board accepts every identifier, so use
        // an internal non-broadcast value for codec processing and restore the
        // exact request identifier in the generated response afterward.
        self.request[6] = 1;
        self.response.clear();
        self.response_offset = 0;
        let processed = process_with_rmodbus(
            &self.request[..self.request_len],
            &mut self.response,
            snapshot,
            config,
            loss_of_contact_counter,
        );
        self.request[6] = unit_id;
        let (accepted, updated_config) = processed.map_err(|_| ())?;
        if self.response.len > 6 {
            self.response.bytes[6] = unit_id;
        }

        if accepted && !consume_accepted {
            self.response.clear();
            self.response_offset = 0;
        } else {
            self.reset_request();
        }

        Ok(RequestOutcome {
            accepted,
            config: updated_config,
            transaction_id,
        })
    }

    /// Replace any prior response with one standard Modbus exception ADU.
    ///
    /// Args:
    ///   unit_id: Unit Identifier copied from the request.
    ///   function: Rejected request function code.
    ///   error: Standard Modbus exception represented by rmodbus.
    fn queue_exception(&mut self, unit_id: u8, function: u8, error: ErrorKind) {
        self.response_offset = 0;
        set_exception_response(
            &self.request[..self.request_len],
            &mut self.response,
            unit_id,
            function,
            error,
        );
    }

    /// Discard the staged ADU without changing response or configuration state.
    fn reset_request(&mut self) {
        self.request_len = 0;
        self.expected_len = None;
    }
}

/// Validate supported request lengths and protocol-defined register counts.
///
/// The caller has already required an eight-byte minimum ADU, so indexing the
/// unit and function bytes is safe. FC16 byte iteration is consequently bounded
/// by the fixed 256-byte request buffer.
///
/// Args:
///   request: Complete request ADU with shape `(request_len,)`.
///
/// Returns:
///   `Ok(())` for a supported, structurally valid PDU, or the standard Modbus
///   exception represented by rmodbus.
fn validate_supported_pdu(request: &[u8]) -> Result<(), ErrorKind> {
    match request[7] {
        0x03 | 0x04 => {
            if request.len() != 12 {
                return Err(ErrorKind::IllegalDataValue);
            }
            let count = u16::from_be_bytes([request[10], request[11]]);
            if count == 0 || count > MODBUS_MAX_READ_REGISTERS {
                return Err(ErrorKind::IllegalDataValue);
            }
            Ok(())
        }
        0x10 => {
            if request.len() < 13 {
                return Err(ErrorKind::IllegalDataValue);
            }
            let count = u16::from_be_bytes([request[10], request[11]]);
            let byte_count = usize::from(request[12]);
            if count == 0
                || count > MODBUS_MAX_WRITE_REGISTERS
                || byte_count != usize::from(count) * 2
                || request.len() != 13 + byte_count
            {
                return Err(ErrorKind::IllegalDataValue);
            }
            Ok(())
        }
        _ => Err(ErrorKind::IllegalFunction),
    }
}

/// Parse one ADU with rmodbus and apply the rev7 fixed register map.
///
/// rmodbus parsing contains no request-dependent loop. The only subsequent
/// loops are bounded by the fixed snapshot and holding-register array lengths.
///
/// Args:
///   request: Complete, normalized-unit request ADU with shape `(request_len,)`.
///   response: Fixed response storage.
///   snapshot: Latest immutable engineering snapshot.
///   config: Complete retained configuration before the request.
///   loss_of_contact_counter: Current unanswered-cycle count.
///
/// Returns:
///   Whether the request was accepted and the resulting complete configuration,
///   or an internal rmodbus parsing/storage error.
fn process_with_rmodbus(
    request: &[u8],
    response: &mut FixedResponse,
    snapshot: &OperatingSnapshot,
    config: ModbusInitialConfig,
    loss_of_contact_counter: u16,
) -> Result<(bool, ModbusInitialConfig), ErrorKind> {
    let (function, address, count) = {
        let mut frame = ModbusFrame::new(1, request, ModbusProto::TcpUdp, response);
        frame.parse()?;
        (frame.func, frame.reg, frame.count)
    };

    match function {
        ModbusFunction::GetInputs => {
            if queue_snapshot_response(request, response, snapshot, address, count).is_err() {
                set_exception_response(
                    request,
                    response,
                    request[6],
                    function.byte(),
                    ErrorKind::IllegalDataAddress,
                );
                Ok((false, config))
            } else {
                Ok((true, config))
            }
        }
        ModbusFunction::GetHoldings => {
            let registers = holding_registers(&config, loss_of_contact_counter);
            if queue_read_response(request, response, &registers, address, count).is_err() {
                set_exception_response(
                    request,
                    response,
                    request[6],
                    function.byte(),
                    ErrorKind::IllegalDataAddress,
                );
                Ok((false, config))
            } else {
                Ok((true, config))
            }
        }
        ModbusFunction::SetHoldingsBulk => {
            if usize::from(count) > MAX_HOLDING_WRITE_REGISTERS {
                set_exception_response(
                    request,
                    response,
                    request[6],
                    function.byte(),
                    ErrorKind::IllegalDataAddress,
                );
                return Ok((false, config));
            }
            let mut values = [0_u16; MAX_HOLDING_WRITE_REGISTERS];
            for (index, pair) in request[13..].chunks_exact(2).enumerate() {
                values[index] = u16::from_be_bytes([pair[0], pair[1]]);
            }
            match apply_holding_write(config, address, &values[..usize::from(count)]) {
                Ok(updated_config) => {
                    initialize_response(request, response, request[6], 5);
                    response.bytes[7..12].copy_from_slice(&request[7..12]);
                    Ok((true, updated_config))
                }
                Err(error) => {
                    let error = match error {
                        deimos_shared::peripherals::deimos_daq_rev7::modbus::HoldingWriteError::IllegalDataAddress => ErrorKind::IllegalDataAddress,
                        deimos_shared::peripherals::deimos_daq_rev7::modbus::HoldingWriteError::IllegalDataValue => ErrorKind::IllegalDataValue,
                    };
                    set_exception_response(request, response, request[6], function.byte(), error);
                    Ok((false, config))
                }
            }
        }
        _ => Err(ErrorKind::IllegalFunction),
    }
}

/// Encode a full snapshot directly into its response payload when possible.
///
/// The complete 79-register read is the synchronized-snapshot use case and the
/// worst realtime request. Its direct path avoids constructing intermediate
/// registers and then converting each one back into network byte order.
/// Uncommon partial reads retain the generic register-slice implementation.
///
/// Args:
///   request: Complete request ADU supplying header and function fields.
///   response: Fixed response storage.
///   snapshot: Latest immutable engineering snapshot.
///   address: Zero-based first requested register.
///   count: Number of requested registers.
///
/// Returns:
///   `Ok(())` after encoding an in-range span, or `IllegalDataAddress`.
fn queue_snapshot_response(
    request: &[u8],
    response: &mut FixedResponse,
    snapshot: &OperatingSnapshot,
    address: u16,
    count: u16,
) -> Result<(), ErrorKind> {
    if address != 0 || count != SNAPSHOT_INPUT_REGISTER_COUNT {
        return queue_partial_snapshot_response(request, response, snapshot, address, count);
    }

    let response_len = 9 + SNAPSHOT_INPUT_BYTE_COUNT;
    initialize_response(request, response, request[6], SNAPSHOT_INPUT_BYTE_COUNT + 2);
    response.bytes[7] = request[7];
    response.bytes[8] = SNAPSHOT_INPUT_BYTE_COUNT as u8;
    let destination: &mut [u8; SNAPSHOT_INPUT_BYTE_COUNT] = (&mut response.bytes[9..response_len])
        .try_into()
        .map_err(|_| ErrorKind::OOB)?;
    write_snapshot_input_register_bytes(snapshot, destination);
    Ok(())
}

/// Encode an uncommon partial snapshot through the shared register-valued map.
#[inline(never)]
fn queue_partial_snapshot_response(
    request: &[u8],
    response: &mut FixedResponse,
    snapshot: &OperatingSnapshot,
    address: u16,
    count: u16,
) -> Result<(), ErrorKind> {
    let registers = snapshot_input_registers(snapshot);
    queue_read_response(request, response, &registers, address, count)
}

/// Encode one bounded register slice into an FC03 or FC04 response.
///
/// Args:
///   request: Complete request ADU supplying header and function fields.
///   response: Fixed response storage.
///   registers: Complete source map with shape `(N,)`.
///   address: Zero-based first requested register.
///   count: Number of requested registers.
///
/// Returns:
///   `Ok(())` after encoding the requested in-range span, or `IllegalDataAddress`.
fn queue_read_response<const N: usize>(
    request: &[u8],
    response: &mut FixedResponse,
    registers: &[u16; N],
    address: u16,
    count: u16,
) -> Result<(), ErrorKind> {
    let start = usize::from(address);
    let end = start
        .checked_add(usize::from(count))
        .ok_or(ErrorKind::IllegalDataAddress)?;
    let data_len = usize::from(count) * 2;
    let response_len = 9 + data_len;
    if end > registers.len() || response_len > MODBUS_ADU_CAPACITY {
        return Err(ErrorKind::IllegalDataAddress);
    }
    initialize_response(request, response, request[6], data_len + 2);
    response.bytes[7] = request[7];
    response.bytes[8] = data_len as u8;
    for (destination, value) in response.bytes[9..response_len]
        .chunks_exact_mut(2)
        .zip(&registers[start..end])
    {
        destination.copy_from_slice(&value.to_be_bytes());
    }
    Ok(())
}

/// Initialize the shared MBAP response header and final response length.
///
/// Args:
///   request: Complete request ADU supplying transaction and protocol IDs.
///   response: Fixed response storage to reset and initialize.
///   unit_id: Unit Identifier to echo.
///   pdu_len: Response PDU length in bytes, excluding the Unit Identifier.
fn initialize_response(request: &[u8], response: &mut FixedResponse, unit_id: u8, pdu_len: usize) {
    let response_len = 7 + pdu_len;
    debug_assert!(response_len <= MODBUS_ADU_CAPACITY);
    response.clear();
    response.bytes[..4].copy_from_slice(&request[..4]);
    response.bytes[4..6].copy_from_slice(&((pdu_len + 1) as u16).to_be_bytes());
    response.bytes[6] = unit_id;
    response.len = response_len;
}

/// Encode one standard exception response without a fallible realtime unwrap.
///
/// Args:
///   request: Complete request ADU supplying transaction and protocol IDs.
///   response: Fixed response storage.
///   unit_id: Unit Identifier copied from the request.
///   function: Rejected request function code.
///   error: rmodbus error mapped to a standard exception code; unexpected
///     internal errors defensively become `SlaveDeviceFailure`.
fn set_exception_response(
    request: &[u8],
    response: &mut FixedResponse,
    unit_id: u8,
    function: u8,
    error: ErrorKind,
) {
    let error_code = match error.to_modbus_error() {
        Ok(code) => code,
        Err(_) => ModbusErrorCode::SlaveDeviceFailure,
    }
    .byte();
    initialize_response(request, response, unit_id, 2);
    response.bytes[7] = function | 0x80;
    response.bytes[8] = error_code;
}

const _: () = assert!(SNAPSHOT_INPUT_REGISTER_COUNT <= MODBUS_MAX_READ_REGISTERS);
const _: () = assert!(HOLDING_REGISTER_COUNT <= MODBUS_MAX_READ_REGISTERS);

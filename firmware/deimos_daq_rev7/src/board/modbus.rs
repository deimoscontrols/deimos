//! Fixed-storage, cycle-budgeted Modbus/TCP request handling.

use deimos_shared::peripherals::deimos_daq_rev7::{
    ModbusInitialConfig, OperatingSnapshot,
    modbus::{
        HOLDING_REGISTER_COUNT, MAX_HOLDING_WRITE_REGISTERS, SNAPSHOT_INPUT_REGISTER_COUNT,
        apply_holding_write, holding_registers, snapshot_input_registers,
    },
};
use rmodbus::{ErrorKind, ModbusProto, VectorTrait, consts::ModbusFunction, server::ModbusFrame};

use super::subsystems::net::Net;

/// Largest ADU accepted by the selected no-alloc rmodbus codec.
const MODBUS_ADU_CAPACITY: usize = 256;
/// Bytes required before the MBAP length field can define the frame boundary.
const MBAP_PREFIX_LEN: usize = 6;
/// Lowest legal MBAP length: one unit byte and one function byte.
const MIN_MBAP_LENGTH: usize = 2;
/// Highest MBAP length accepted by rmodbus's fixed frame representation.
const MAX_MBAP_LENGTH: usize = 250;
/// Maximum socket receive calls in one publishing cycle.
const MAX_RX_CALLS_PER_CYCLE: u8 = 2;
/// Maximum socket transmit calls in one publishing cycle.
const MAX_TX_CALLS_PER_CYCLE: u8 = 2;

/// Remaining socket operations available to one binding or operating cycle.
pub(super) struct ModbusSocketBudget {
    rx_calls: u8,
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
    bytes: [u8; MODBUS_ADU_CAPACITY],
    len: usize,
}

impl FixedResponse {
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
        self.bytes[index] = value;
    }
}

/// Board-owned Modbus framing state which survives state-machine re-entry.
pub(super) struct ModbusTcpServer {
    request: [u8; MODBUS_ADU_CAPACITY],
    request_len: usize,
    expected_len: Option<usize>,
    response: FixedResponse,
    response_offset: usize,
    binding_config: Option<ModbusInitialConfig>,
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
            if self.expected_len.is_none() && self.request_len == MBAP_PREFIX_LEN {
                let protocol = u16::from_be_bytes([self.request[2], self.request[3]]);
                let mbap_length =
                    usize::from(u16::from_be_bytes([self.request[4], self.request[5]]));
                if protocol != 0 || !(MIN_MBAP_LENGTH..=MAX_MBAP_LENGTH).contains(&mbap_length) {
                    return ReceiveStatus::Malformed;
                }
                self.expected_len = Some(MBAP_PREFIX_LEN + mbap_length);
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

        if self.expected_len.is_none() && self.request_len == MBAP_PREFIX_LEN {
            let protocol = u16::from_be_bytes([self.request[2], self.request[3]]);
            let mbap_length = usize::from(u16::from_be_bytes([self.request[4], self.request[5]]));
            if protocol != 0 || !(MIN_MBAP_LENGTH..=MAX_MBAP_LENGTH).contains(&mbap_length) {
                return ReceiveStatus::Malformed;
            }
            self.expected_len = Some(MBAP_PREFIX_LEN + mbap_length);
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
    ///   `Ok(true)` once the complete response has been enqueued, `Ok(false)`
    ///   when backpressure leaves bytes pending, or `Err(())` on disconnect.
    pub(super) fn send_response(
        &mut self,
        net: &mut Net<'_>,
        budget: &mut ModbusSocketBudget,
    ) -> Result<bool, ()> {
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
        if self.response_pending() {
            Ok(false)
        } else {
            self.response.clear();
            self.response_offset = 0;
            Ok(true)
        }
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

    fn reset_request(&mut self) {
        self.request_len = 0;
        self.expected_len = None;
    }
}

fn validate_supported_pdu(request: &[u8]) -> Result<(), ErrorKind> {
    match request[7] {
        0x03 | 0x04 => {
            if request.len() != 12 {
                return Err(ErrorKind::IllegalDataValue);
            }
            let count = u16::from_be_bytes([request[10], request[11]]);
            if count == 0 || count > 125 {
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
                || count > 123
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

fn process_with_rmodbus(
    request: &[u8],
    response: &mut FixedResponse,
    snapshot: &OperatingSnapshot,
    config: ModbusInitialConfig,
    loss_of_contact_counter: u16,
) -> Result<(bool, ModbusInitialConfig), ErrorKind> {
    let mut frame = ModbusFrame::new(1, request, ModbusProto::TcpUdp, response);
    frame.parse()?;
    let function = frame.func;
    let address = frame.reg;
    let count = frame.count;
    drop(frame);

    match function {
        ModbusFunction::GetInputs => {
            let registers = snapshot_input_registers(snapshot);
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
                    response.clear();
                    response.bytes[..4].copy_from_slice(&request[..4]);
                    response.bytes[4..6].copy_from_slice(&6_u16.to_be_bytes());
                    response.bytes[6..12].copy_from_slice(&request[6..12]);
                    response.len = 12;
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
    response.clear();
    response.bytes[..4].copy_from_slice(&request[..4]);
    response.bytes[4..6].copy_from_slice(&u16::try_from(data_len + 3)?.to_be_bytes());
    response.bytes[6] = request[6];
    response.bytes[7] = request[7];
    response.bytes[8] = data_len as u8;
    for (destination, value) in response.bytes[9..response_len]
        .chunks_exact_mut(2)
        .zip(&registers[start..end])
    {
        destination.copy_from_slice(&value.to_be_bytes());
    }
    response.len = response_len;
    Ok(())
}

fn set_exception_response(
    request: &[u8],
    response: &mut FixedResponse,
    unit_id: u8,
    function: u8,
    error: ErrorKind,
) {
    response.clear();
    response.bytes[..9].copy_from_slice(&[
        request[0],
        request[1],
        0,
        0,
        0,
        3,
        unit_id,
        function | 0x80,
        error.to_modbus_error().unwrap().byte(),
    ]);
    response.len = 9;
}

const _: () = assert!(SNAPSHOT_INPUT_REGISTER_COUNT <= 125);
const _: () = assert!(HOLDING_REGISTER_COUNT <= 125);

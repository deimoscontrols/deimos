//! Minimal synchronized-snapshot Modbus/TCP client for a calibrated rev7 DAQ.
//!
//! Run with `cargo run -p deimos --example rev7_modbus_client -- IP[:PORT]`.
//! The endpoint defaults to SN3's first deterministic fallback address.

use std::{
    env,
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};

use deimos_shared::peripherals::deimos_daq_rev7::modbus::{
    SNAPSHOT_INPUT_REGISTER_COUNT, SNAPSHOT_INPUT_START, snapshot_from_input_registers,
};
use rmodbus::{ModbusProto, client::ModbusRequest, guess_response_frame_len};

const DEFAULT_ENDPOINT: &str = "169.254.101.34:502";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned());
    let endpoint = if endpoint.contains(':') {
        endpoint
    } else {
        format!("{endpoint}:502")
    };

    let mut stream = TcpStream::connect(&endpoint)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;

    // One full-block FC04 read obtains all values from the same publication
    // cycle. The board accepts and echoes every unit identifier; zero is used
    // here to exercise that explicitly supported behavior.
    let mut request_codec = ModbusRequest::new(0, ModbusProto::TcpUdp);
    let mut request = Vec::new();
    request_codec.generate_get_inputs(
        SNAPSHOT_INPUT_START,
        SNAPSHOT_INPUT_REGISTER_COUNT,
        &mut request,
    )?;
    stream.write_all(&request)?;

    let response = read_one_adu(&mut stream)?;
    let mut registers = Vec::with_capacity(SNAPSHOT_INPUT_REGISTER_COUNT as usize);
    request_codec.parse_u16(&response, &mut registers)?;
    let snapshot = snapshot_from_input_registers(&registers)
        .map_err(|error| format!("invalid rev7 snapshot register block: {error:?}"))?;

    println!("endpoint={endpoint}");
    println!("snapshot_id={}", snapshot.metrics.id);
    println!("cycle_time_ns={}", snapshot.metrics.cycle_time_ns);
    println!("board_temperature_k={}", snapshot.board_temperature_k);
    println!("module_bus_voltage_v={}", snapshot.module_bus_voltage_v);
    println!("module_bus_current_a={}", snapshot.module_bus_current_a);
    println!("snapshot={snapshot:#?}");
    Ok(())
}

/// Read exactly one length-delimited Modbus/TCP ADU from a byte stream.
///
/// Args:
///   stream: Connected Modbus/TCP stream with a finite read timeout.
///
/// Returns:
///   Complete response ADU beginning with its six-byte MBAP prefix.
fn read_one_adu(stream: &mut TcpStream) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut prefix = [0_u8; 6];
    stream.read_exact(&mut prefix)?;
    let total_len = usize::from(guess_response_frame_len(&prefix, ModbusProto::TcpUdp)?);
    if !(8..=256).contains(&total_len) {
        return Err(format!("invalid Modbus/TCP response length {total_len}").into());
    }
    let mut response = vec![0_u8; total_len];
    response[..6].copy_from_slice(&prefix);
    stream.read_exact(&mut response[6..])?;
    Ok(response)
}

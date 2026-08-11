//! Minimal blocking SCPI-over-TCP transport for instrument worker threads.
//!
//! Commands are ASCII and newline-delimited. Queries accept exactly one
//! bounded, newline-terminated response. This module intentionally avoids a
//! general SCPI dependency and contains no instrument-specific command syntax.

use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde::{Deserialize, Serialize};

const DEFAULT_MAX_RESPONSE_LEN: usize = 16 * 1024;
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_millis(250);
const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_millis(100);

/// Add the standard raw-SCPI port 5025 when an address has no explicit port.
pub(crate) fn address_with_default_port(host: String) -> String {
    if host.parse::<SocketAddr>().is_ok() {
        return host;
    }
    if host.starts_with('[') && host.ends_with(']') {
        return format!("{host}:5025");
    }
    if host
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_ipv6())
    {
        return format!("[{host}]:5025");
    }
    if host
        .rsplit_once(':')
        .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
    {
        host
    } else {
        format!("{host}:5025")
    }
}

/// Shared network, identity, and timeout settings for a SCPI/TCP instrument.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ScpiTcpConfig {
    /// TCP address in `host:port` form.
    pub address: String,
    /// Logical software serial number used in the Deimos peripheral ID.
    pub serial_number: u64,
    /// Case-insensitive manufacturer substring required in `*IDN?`.
    pub expected_vendor: String,
    /// Case-insensitive model substring required in `*IDN?`.
    pub expected_model: String,
    /// Maximum time allowed to establish the TCP connection.
    pub connect_timeout: Duration,
    /// Maximum time allowed for each SCPI response.
    pub read_timeout: Duration,
    /// Maximum time allowed for each SCPI command write.
    pub write_timeout: Duration,
}

impl ScpiTcpConfig {
    /// Build common identity settings with SCPI port 5025 and conservative
    /// connection and I/O timeouts.
    pub fn new(
        host: impl Into<String>,
        serial_number: u64,
        expected_vendor: impl Into<String>,
        expected_model: impl Into<String>,
    ) -> Self {
        Self {
            address: address_with_default_port(host.into()),
            serial_number,
            expected_vendor: expected_vendor.into(),
            expected_model: expected_model.into(),
            connect_timeout: Duration::from_secs(2),
            read_timeout: DEFAULT_READ_TIMEOUT,
            write_timeout: DEFAULT_WRITE_TIMEOUT,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.address.trim().is_empty() {
            return Err("address cannot be empty".to_owned());
        }
        if self.expected_vendor.trim().is_empty() || self.expected_model.trim().is_empty() {
            return Err("expected_vendor and expected_model cannot be empty".to_owned());
        }
        Ok(())
    }

    /// Budget a startup sequence from its sequential SCPI operations.
    pub(crate) fn startup_timeout(
        &self,
        query_count: u32,
        command_count: u32,
        additional_time: Duration,
    ) -> Duration {
        let query_timeout = self.read_timeout.saturating_add(self.write_timeout);
        self.connect_timeout
            .saturating_add(query_timeout.saturating_mul(query_count))
            .saturating_add(self.write_timeout.saturating_mul(command_count))
            .saturating_add(additional_time)
    }

    pub(crate) fn validate_identity(&self, identity: &str) -> Result<(), String> {
        let uppercase = identity.to_ascii_uppercase();
        if uppercase.contains(&self.expected_vendor.to_ascii_uppercase())
            && uppercase.contains(&self.expected_model.to_ascii_uppercase())
        {
            Ok(())
        } else {
            Err(format!(
                "identity `{identity}` did not match {} {}",
                self.expected_vendor, self.expected_model
            ))
        }
    }
}

/// Minimal newline-delimited SCPI client for one worker-owned TCP connection.
///
/// One worker owns each client for its entire lifetime, so command/query
/// ordering requires no additional synchronization.
pub(crate) struct ScpiClient {
    // Buffering is required because one TCP read may contain the end of the
    // current response and the beginning of a later response.
    stream: BufReader<TcpStream>,
    max_response_len: usize,
}

impl ScpiClient {
    /// Resolve and connect to an instrument with bounded socket operations.
    ///
    /// Errors:
    ///   Returns contextual resolution, connection, or socket-configuration errors.
    pub(crate) fn connect(config: &ScpiTcpConfig) -> Result<Self, String> {
        let address = &config.address;
        let addresses = address
            .to_socket_addrs()
            .map_err(|err| format!("unable to resolve `{address}`: {err}"))?;
        let mut last_error = None;
        // DNS may return several IPv4/IPv6 candidates. Try each candidate so a
        // failed first family does not make an otherwise reachable host fail.
        for resolved in addresses {
            match TcpStream::connect_timeout(&resolved, config.connect_timeout) {
                Ok(stream) => {
                    stream
                        .set_read_timeout(Some(config.read_timeout))
                        .map_err(|err| {
                            format!("unable to set read timeout for `{address}`: {err}")
                        })?;
                    stream
                        .set_write_timeout(Some(config.write_timeout))
                        .map_err(|err| {
                            format!("unable to set write timeout for `{address}`: {err}")
                        })?;
                    stream.set_nodelay(true).map_err(|err| {
                        format!("unable to set TCP_NODELAY for `{address}`: {err}")
                    })?;
                    return Ok(Self {
                        stream: BufReader::new(stream),
                        max_response_len: DEFAULT_MAX_RESPONSE_LEN,
                    });
                }
                Err(err) => last_error = Some((resolved, err)),
            }
        }

        match last_error {
            Some((resolved, err)) => Err(format!("unable to connect to `{resolved}`: {err}")),
            None => Err(format!("`{address}` resolved to no socket addresses")),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_max_response_len(mut self, max_response_len: usize) -> Self {
        self.max_response_len = max_response_len;
        self
    }

    /// Write one ASCII SCPI command and its newline terminator.
    ///
    /// Errors:
    ///   Returns an error for empty, non-ASCII, multiline, write, or flush failures.
    pub(crate) fn command(&mut self, command: &str) -> Result<(), String> {
        validate_command(command)?;
        let stream = self.stream.get_mut();
        stream
            .write_all(command.as_bytes())
            .map_err(|err| format!("failed to write `{command}`: {err}"))?;
        stream
            .write_all(b"\n")
            .map_err(|err| format!("failed to terminate `{command}`: {err}"))?;
        stream
            .flush()
            .map_err(|err| format!("failed to flush `{command}`: {err}"))
    }

    /// Write one command and read one bounded newline-terminated response.
    ///
    /// Errors:
    ///   Returns command errors plus timeout, EOF, empty, oversized, or
    ///   non-ASCII response errors.
    pub(crate) fn query(&mut self, command: &str) -> Result<String, String> {
        self.command(command)?;
        self.read_response(command)
    }

    /// Query the instrument's normalized standard identity string.
    pub(crate) fn identify(&mut self) -> Result<String, String> {
        self.query("*IDN?")
    }

    /// Read and validate the single-line response belonging to `command`.
    ///
    /// Errors:
    ///   Returns an error for socket failures or malformed response framing.
    fn read_response(&mut self, command: &str) -> Result<String, String> {
        let mut bytes = Vec::new();
        loop {
            let available = self
                .stream
                .fill_buf()
                .map_err(|err| format!("failed to read response to `{command}`: {err}"))?;
            if available.is_empty() {
                return Err(format!(
                    "connection closed before response to `{command}` was terminated"
                ));
            }

            let newline = available.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(available.len(), |position| position + 1);
            // Enforce the limit while consuming rather than using `read_line`,
            // which may allocate an unbounded response before it can be checked.
            if bytes.len() + take > self.max_response_len {
                return Err(format!(
                    "response to `{command}` exceeded {} bytes",
                    self.max_response_len
                ));
            }
            bytes.extend_from_slice(&available[..take]);
            self.stream.consume(take);
            if newline.is_some() {
                break;
            }
        }

        while matches!(bytes.last(), Some(b'\n' | b'\r')) {
            bytes.pop();
        }
        if bytes.is_empty() {
            return Err(format!("response to `{command}` was empty"));
        }
        if !bytes.is_ascii() {
            return Err(format!("response to `{command}` was not ASCII"));
        }
        String::from_utf8(bytes).map_err(|err| format!("invalid response to `{command}`: {err}"))
    }
}

/// Reject command text that could corrupt the newline-delimited stream.
fn validate_command(command: &str) -> Result<(), String> {
    if command.is_empty() {
        return Err("SCPI command cannot be empty".to_owned());
    }
    if !command.is_ascii() {
        return Err("SCPI command must be ASCII".to_owned());
    }
    if command.bytes().any(|byte| matches!(byte, b'\r' | b'\n')) {
        return Err("SCPI command cannot contain a line ending".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn client_with_server(
        response_parts: Vec<Vec<u8>>,
        max_response_len: usize,
    ) -> (ScpiClient, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut command = String::new();
            reader.read_line(&mut command).unwrap();
            let mut stream = stream;
            for part in response_parts {
                stream.write_all(&part).unwrap();
            }
            command
        });
        let mut config = ScpiTcpConfig::new(address.to_string(), 1, "TEST", "MODEL");
        config.connect_timeout = Duration::from_secs(1);
        config.read_timeout = Duration::from_secs(1);
        config.write_timeout = Duration::from_secs(1);
        let client = ScpiClient::connect(&config)
            .unwrap()
            .with_max_response_len(max_response_len);
        (client, server)
    }

    #[test]
    fn query_frames_command_and_assembles_partial_response() {
        let (mut client, server) =
            client_with_server(vec![b"SIGLENT,".to_vec(), b"SDG2042X\r\n".to_vec()], 64);
        assert_eq!(client.identify().unwrap(), "SIGLENT,SDG2042X");
        assert_eq!(server.join().unwrap(), "*IDN?\n");
    }

    #[test]
    fn query_rejects_oversized_response() {
        let (mut client, server) = client_with_server(vec![b"123456789\n".to_vec()], 8);
        assert!(client.query("READ?").unwrap_err().contains("exceeded"));
        server.join().unwrap();
    }

    #[test]
    fn command_rejects_embedded_newline() {
        assert!(validate_command("OUTP ON\n*RST").is_err());
    }

    #[test]
    fn default_port_handles_names_and_ipv6_literals() {
        assert_eq!(
            address_with_default_port("instrument".to_owned()),
            "instrument:5025"
        );
        assert_eq!(
            address_with_default_port("instrument:1234".to_owned()),
            "instrument:1234"
        );
        assert_eq!(address_with_default_port("::1".to_owned()), "[::1]:5025");
        assert_eq!(address_with_default_port("[::1]".to_owned()), "[::1]:5025");
        assert_eq!(
            address_with_default_port("[::1]:1234".to_owned()),
            "[::1]:1234"
        );
    }

    #[test]
    fn connection_config_validates_identity_case_insensitively() {
        let config = ScpiTcpConfig::new("instrument", 7, "Siglent", "SDG2042X");
        assert_eq!(config.address, "instrument:5025");
        assert!(
            config
                .validate_identity("SIGLENT TECHNOLOGIES,sdg2042x,123,1.0")
                .is_ok()
        );
        assert!(
            config
                .validate_identity("KEITHLEY,DMM6500,123,1.0")
                .is_err()
        );
    }
}

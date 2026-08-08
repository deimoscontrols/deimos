//! Minimal blocking SCPI-over-TCP transport for instrument worker threads.
//!
//! Commands are ASCII and newline-delimited. Queries accept exactly one
//! bounded, newline-terminated response. This module intentionally avoids a
//! general SCPI dependency and contains no instrument-specific command syntax.

use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

const DEFAULT_MAX_RESPONSE_LEN: usize = 16 * 1024;

/// Add the standard raw-SCPI port when the caller supplied only a host.
///
/// Args:
///   host: Host name, IP literal, or socket address.
///
/// Returns:
///   The input address with port 5025 added when it had no explicit port.
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

/// Minimal newline-delimited SCPI client for one worker-owned TCP connection.
///
/// One worker owns each client for its entire lifetime, so command/query
/// ordering requires no additional synchronization.
pub(crate) struct ScpiClient {
    stream: BufReader<TcpStream>,
    max_response_len: usize,
}

impl ScpiClient {
    /// Resolve and connect to an instrument with bounded socket operations.
    ///
    /// Args:
    ///   address: Host and port accepted by `ToSocketAddrs`.
    ///   connect_timeout: Per-address TCP connection deadline.
    ///   read_timeout: Deadline applied to each response read.
    ///   write_timeout: Deadline applied to each command write.
    ///
    /// Returns:
    ///   A buffered client owning the connected stream.
    ///
    /// Errors:
    ///   Returns contextual resolution, connection, or socket-configuration errors.
    pub(crate) fn connect(
        address: &str,
        connect_timeout: Duration,
        read_timeout: Duration,
        write_timeout: Duration,
    ) -> Result<Self, String> {
        let addresses = address
            .to_socket_addrs()
            .map_err(|err| format!("unable to resolve `{address}`: {err}"))?;
        let mut last_error = None;
        for resolved in addresses {
            match TcpStream::connect_timeout(&resolved, connect_timeout) {
                Ok(stream) => {
                    stream.set_read_timeout(Some(read_timeout)).map_err(|err| {
                        format!("unable to set read timeout for `{address}`: {err}")
                    })?;
                    stream
                        .set_write_timeout(Some(write_timeout))
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
    /// Args:
    ///   command: SCPI command without a trailing line ending.
    ///
    /// Returns:
    ///   Success after the complete command has been flushed to the socket.
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
    /// Args:
    ///   command: SCPI query without a trailing line ending.
    ///
    /// Returns:
    ///   The ASCII response with trailing CR/LF bytes removed.
    ///
    /// Errors:
    ///   Returns command errors plus timeout, EOF, empty, oversized, or
    ///   non-ASCII response errors.
    pub(crate) fn query(&mut self, command: &str) -> Result<String, String> {
        self.command(command)?;
        self.read_response(command)
    }

    /// Query the instrument's standard identity string.
    ///
    /// Returns:
    ///   The normalized response to `*IDN?`.
    ///
    /// Errors:
    ///   Returns any error produced by [`Self::query`].
    pub(crate) fn identify(&mut self) -> Result<String, String> {
        self.query("*IDN?")
    }

    /// Read and validate the single-line response belonging to `command`.
    ///
    /// Args:
    ///   command: Command text used to contextualize any error.
    ///
    /// Returns:
    ///   The ASCII response without its CR/LF terminator.
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
        let client = ScpiClient::connect(
            &address.to_string(),
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
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
}

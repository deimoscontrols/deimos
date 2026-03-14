//! Either reuse or create a new table in a TimescaleDB postgres
//! database and write to that table.

use std::io::Write;
use std::sync::mpsc::{Sender, channel};
use std::thread::{self, Builder, JoinHandle};
use std::time::{Duration, SystemTime};

use postgres::{Client, NoTls, Statement};
use postgres_types::{ToSql, Type};
use serde::{Deserialize, Serialize};
use tracing::info;

#[cfg(feature = "python")]
use pyo3::prelude::*;

use crate::controller::context::ControllerCtx;
use crate::py_json_methods;

use super::{Dispatcher, csv_row};

/// Either reuse or create a new table in a TimescaleDB postgres
/// database and write to that table.
///
/// Depending on the buffer time window, this dispatcher will either use a simple
/// INSERT query to send one row of values at a time, or use COPY INTO syntax
/// to send buffered batches of rows all at once.
///
/// Performs database transactions on on a separate thread to avoid blocking the control loop.
///
/// Does not support TLS, and as a result, is recommended for communication with databases
/// on the same internal network, not on the open web.
#[derive(Serialize, Deserialize, Default)]
#[cfg_attr(feature = "python", pyclass)]
pub struct TimescaleDbDispatcher {
    /// Name of the database. The table name will be the controller's op name.
    dbname: String,
    /// URL or unix socket address
    host: String,
    /// Database username
    ///
    /// If using a unix socket, the database username and the dispatcher's host machine login
    /// name must match and have permissions in the database, because postgres will not allow
    /// using a separate username and login name for unix socket connections.
    user: String,
    /// Name of the environment variable storing the password or access token
    token_name: String,
    /// Window of time to accumulate samples before storing.
    /// If this results in more than one row of samples in the buffer,
    /// the COPY INTO bulk write syntax is used.
    buffer_time: Duration,
    /// Duration for which data will be retained in the database
    retention_time: Duration,
    /// Optional hypertable chunk interval. If omitted, this defaults to one
    /// quarter of the retention duration during initialization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chunk_interval: Option<Duration>,

    #[serde(skip)]
    worker: Option<WorkerHandle>,
}

impl TimescaleDbDispatcher {
    /// Store configuration, but do not connect to database yet
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        dbname: &str,
        host: &str,
        user: &str,
        token_name: &str,
        buffer_time: Duration,
        retention_time: Duration,
    ) -> Box<Self> {
        Box::new(Self {
            dbname: dbname.to_owned(),
            host: host.to_owned(),
            user: user.to_owned(),
            token_name: token_name.to_owned(),
            buffer_time,
            retention_time,
            chunk_interval: None,
            worker: None,
        })
    }

    /// Override the default hypertable chunk interval.
    pub fn with_chunk_interval(mut self: Box<Self>, chunk_interval: Duration) -> Box<Self> {
        self.chunk_interval = Some(chunk_interval);
        self
    }
}

py_json_methods!(
    TimescaleDbDispatcher,
    Dispatcher,
    #[new]
    #[pyo3(signature=(
        dbname,
        host,
        user,
        token_name,
        buffer_time_ns,
        retention_time_ns,
        chunk_interval_ns=None
    ))]
    fn py_new(
        dbname: &str,
        host: &str,
        user: &str,
        token_name: &str,
        buffer_time_ns: u64,
        retention_time_ns: u64,
        chunk_interval_ns: Option<u64>,
    ) -> PyResult<Self> {
        let dispatcher = Self::new(
            dbname,
            host,
            user,
            token_name,
            Duration::from_nanos(buffer_time_ns),
            Duration::from_nanos(retention_time_ns),
        );
        let dispatcher = if let Some(chunk_interval_ns) = chunk_interval_ns {
            dispatcher.with_chunk_interval(Duration::from_nanos(chunk_interval_ns))
        } else {
            dispatcher
        };
        Ok(*dispatcher)
    }
);

#[typetag::serde]
impl Dispatcher for TimescaleDbDispatcher {
    /// Connect to the database and either reuse an existing table, or make a new one
    fn init(
        &mut self,
        ctx: &ControllerCtx,
        channel_names: &[String],
        core_assignment: usize,
    ) -> Result<(), String> {
        info!(
            "Initializing Timesdcale DB dispatcher: {}",
            serde_json::to_string_pretty(&self)
                .map_err(|e| format!("Failed to log dispatcher settings: {e}"))?
        );

        // Shut down any existing workers by dropping their tx handle
        self.worker = None;

        // Get token / pw from environment
        let pw = std::env::var(&self.token_name)
            .map_err(|e| format!("Did not find token name env var {}: {e}", &self.token_name))?;

        // Connect to database backend
        let mut client = init_timescaledb_client(&self.dbname, &self.host, &self.user, &pw)?;

        // Set up table for this op
        let chunk_interval = effective_chunk_interval(self.retention_time, self.chunk_interval)?;
        init_timescaledb_table(
            &mut client,
            channel_names,
            &ctx.op_name,
            self.retention_time,
            chunk_interval,
        )?;

        // Figure out how many samples to write per batch
        let n_buffer = (self.buffer_time.as_nanos() / (ctx.dt_ns as u128)).max(1) as usize;

        // Spawn workers
        let dbname = self.dbname.clone();
        let host = self.host.clone();
        let user = self.user.clone();
        self.worker = Some(WorkerHandle::new(
            dbname,
            host,
            user,
            pw,
            channel_names,
            ctx.op_name.to_owned(),
            n_buffer,
            core_assignment,
        )?);

        Ok(())
    }

    fn consume(
        &mut self,
        time: SystemTime,
        timestamp: i64,
        channel_values: Vec<f64>,
    ) -> Result<(), String> {
        match &mut self.worker {
            Some(worker) => worker
                .tx
                .send((time, timestamp, channel_values))
                .map_err(|e| format!("Failed to queue data to send to postgres database: {e}")),
            None => Err("Dispatcher must be initialized before consuming data".into()),
        }
    }

    fn terminate(&mut self) -> Result<(), String> {
        // Drop worker handle, closing thread channel,
        // which will indicate to the detached worker that it should
        // finish storing its buffered data and shut down.
        self.worker = None;
        Ok(())
    }
}

struct WorkerHandle {
    pub tx: Sender<(SystemTime, i64, Vec<f64>)>,
    _thread: JoinHandle<()>,
}

impl WorkerHandle {
    #[allow(clippy::too_many_arguments)]
    fn new(
        dbname: String,
        host: String,
        user: String,
        pw: String,
        channel_names: &[String],
        table_name: String,
        n_buffer: usize,
        core_assignment: usize,
    ) -> Result<Self, String> {
        let (tx, rx) = channel::<(SystemTime, i64, Vec<f64>)>();
        let channel_names: Vec<String> = channel_names.to_vec();

        // Connect to database
        // Client is not Send+Sync, so this has to be done here
        let mut client = init_timescaledb_client(&dbname, &host, &user, &pw)?;

        // Setting the columns to write explicitly isn't required as long as the schema matches,
        // but this protects the case where the user has added more columns in the table
        let channel_query = channel_names
            .iter()
            .map(|x| format!("\"{x}\""))
            .collect::<Vec<String>>()
            .join(" ,");

        // Prep for large-batch COPY INTO
        let copy_query =
            format!("COPY {table_name} (timestamp, time, {channel_query}) FROM STDIN CSV");
        // Buffers for large batch copy
        let mut linebuf = String::new(); // Lines to send to database
        let mut stringbuf = String::new(); // Active line being assembled
        let mut n_lines_buffered = 0;

        // Prep for small-batch INSERT
        let prepared_query = prepare_timescaledb_query(&mut client, &table_name, &channel_names)?;

        let use_copy = n_buffer > 1;

        // Run database I/O on a separate thread to avoid blocking controller
        let _thread = Builder::new()
            .name("tsdb-dispatcher".to_string())
            .spawn(move || {
                // Bind to assigned core
                core_affinity::set_for_current(core_affinity::CoreId {
                    id: core_assignment,
                });

                loop {
                    match rx.recv() {
                        Err(_) => {
                            // Channel closed; controller is shutting down
                            // Flush and exit
                            if use_copy {
                                let mut writer = client.copy_in(&copy_query).unwrap();
                                writer.write_all(linebuf.as_bytes()).unwrap();
                                writer.finish().unwrap();
                            }

                            return;
                        }
                        Ok(vals) => {
                            // Send

                            // COPY INTO method for batches of points
                            if use_copy {
                                //    Buffer this line
                                let (time, timestamp, channel_values) = vals;
                                csv_row(&mut stringbuf, (time, timestamp, &channel_values));
                                linebuf.push_str(&stringbuf);
                                n_lines_buffered += 1;
                                //    Flush buffer if ready
                                if n_lines_buffered >= n_buffer {
                                    let mut writer = client.copy_in(&copy_query).unwrap();
                                    writer.write_all(linebuf.as_bytes()).unwrap();
                                    writer.finish().unwrap();
                                    n_lines_buffered = 0;
                                    linebuf.clear();
                                }
                            }
                            // INSERT method for single points
                            else {
                                // It would be nice to not have to allocate this every time,
                                // but the borrowed values don't live long enough
                                let mut query_vals: Vec<&(dyn ToSql + Sync)> =
                                    Vec::with_capacity(2 + channel_names.len());

                                let (time, timestamp, channel_values) = vals;
                                query_vals.push(&timestamp);
                                query_vals.push(&time);
                                for v in channel_values.iter() {
                                    query_vals.push(v);
                                }

                                // Send
                                client.execute(&prepared_query, &query_vals).unwrap();

                                // Reset
                                query_vals.clear();
                            }
                        }
                    }

                    // Deliberately yield the thread to make time for the OS
                    // and to avoid interfering with other loops
                    thread::yield_now();
                }
            })
            .expect("Failed to spawn TSDB dispatcher thread");

        Ok(Self { tx, _thread })
    }
}

/// Connect to the database.
fn init_timescaledb_client(
    dbname: &str,
    host: &str,
    user: &str,
    pw: &str,
) -> Result<Client, String> {
    // Use provided port if it exists, otherwise the default port 5432
    let (host, port) = split_host(host);
    let port = port.unwrap_or("5432".to_string());

    // Connect to database backend
    Client::connect(
        &format!("dbname={dbname} host={host} port={port} user={user} password={pw}"),
        NoTls,
    )
    .map_err(|e| format!("Failed to connect postgres client: {e}"))
}

/// Check if a table for this op already exists;
/// reuse it if it exists, or create a new one if it does not.
///
/// # Panics
/// * If the query to check table existence fails
/// * If the table exists but has an incomatible schema
fn init_timescaledb_table(
    client: &mut Client,
    channel_names: &[String],
    op_name: &str,
    retention_time: Duration,
    chunk_interval: Duration,
) -> Result<(), String> {
    let policy_query = timescaledb_policy_query(op_name, retention_time, chunk_interval);

    // Check if the table exists
    let table_exists = !client
        .simple_query(&format!("SELECT 1 FROM public.{op_name};"))
        .unwrap_or_default()
        .is_empty();

    // If it already exists, keep the schema and reconcile Timescale policies in-place.
    // If the schema is incompatible, this will still produce an error on the first write.
    if table_exists {
        return client
            .batch_execute(&policy_query)
            .map_err(|e| format!("{e}"));
    }

    // Make a new table if there isn't one
    let channel_schema_strings: Vec<String> = channel_names
        .iter()
        .map(|x| format!("    \"{x}\" DOUBLE PRECISION,\n"))
        .collect();
    let channel_schema = channel_schema_strings.join("");
    let table_creation_query = format!(
        "
            CREATE TABLE \"{op_name}\" (
            timestamp  BIGINT,
            time       TIMESTAMPTZ,
            {channel_schema}
            PRIMARY KEY (time)
            );

            CREATE EXTENSION IF NOT EXISTS timescaledb CASCADE;

            SELECT create_hypertable(\'\"{op_name}\"\', by_range('time'), if_not_exists => TRUE);

            {policy_query}
    "
    );

    match client.batch_execute(&table_creation_query) {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("{e}")),
    }
}

/// Prepare a query template for a single-row INSERT
fn prepare_timescaledb_query(
    client: &mut Client,
    table_name: &str,
    channel_names: &[String],
) -> Result<Statement, String> {
    // Pre-bake query for storing data
    let channel_query = channel_names
        .iter()
        .map(|x| format!("\"{x}\""))
        .collect::<Vec<String>>()
        .join(" ,");
    let mut channel_types = vec![Type::FLOAT8; channel_names.len() + 2];
    channel_types[0] = Type::INT8;
    channel_types[1] = Type::TIMESTAMPTZ;
    let channel_template = (1..channel_names.len() + 1 + 2)
        .map(|i| format!("${i}"))
        .collect::<Vec<String>>()
        .join(", ");

    client
        .prepare_typed(
            &format!(
                "INSERT INTO \"{}\" (timestamp, time, {}) VALUES ({})",
                table_name, channel_query, channel_template
            ),
            &channel_types,
        )
        .map_err(|e| format!("Failed to build prepared postgres query: {e}"))
}

fn split_host(host: &str) -> (String, Option<String>) {
    let parts: Vec<String> = host.split(":").map(|x| x.to_string()).collect();
    let n = parts.len();
    if n < 2 {
        (host.to_string(), None)
    } else {
        (parts[..n - 1].join(""), parts.last().cloned())
    }
}

fn effective_chunk_interval(
    retention_time: Duration,
    chunk_interval: Option<Duration>,
) -> Result<Duration, String> {
    if let Some(chunk_interval) = chunk_interval {
        if chunk_interval.is_zero() {
            return Err("TimescaleDB chunk interval must be greater than zero".to_string());
        }
        return Ok(chunk_interval);
    }

    let default_chunk_nanos = retention_time.as_nanos() / 4;
    if default_chunk_nanos == 0 {
        return Err(
            "TimescaleDB chunk interval must be greater than zero; provide an explicit chunk interval when retention is less than four nanoseconds".to_string(),
        );
    }

    duration_from_nanos(default_chunk_nanos)
}

fn sql_interval_literal(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let nanos = duration.subsec_nanos();
    if nanos == 0 {
        format!("{seconds} seconds")
    } else {
        format!("{seconds}.{nanos:09} seconds")
    }
}

fn duration_from_nanos(total_nanos: u128) -> Result<Duration, String> {
    let secs = total_nanos / 1_000_000_000;
    let nanos = (total_nanos % 1_000_000_000) as u32;
    let secs =
        u64::try_from(secs).map_err(|_| format!("Duration is too large: {total_nanos} ns"))?;
    Ok(Duration::new(secs, nanos))
}

fn timescaledb_policy_query(
    op_name: &str,
    retention_time: Duration,
    chunk_interval: Duration,
) -> String {
    let chunk_interval = sql_interval_literal(chunk_interval);
    let retention_time = sql_interval_literal(retention_time);
    format!(
        "
            SELECT set_chunk_time_interval(\'\"{op_name}\"\', INTERVAL '{chunk_interval}');

            SELECT remove_retention_policy(\'\"{op_name}\"\', if_exists => true);

            SELECT add_retention_policy(\'\"{op_name}\"\', drop_after => INTERVAL '{retention_time}');
        "
    )
}

#[cfg(test)]
mod tests {
    use super::{
        duration_from_nanos, effective_chunk_interval, sql_interval_literal,
        timescaledb_policy_query,
    };
    use std::time::Duration;

    #[test]
    fn default_chunk_interval_is_quarter_retention() {
        assert_eq!(
            effective_chunk_interval(Duration::from_secs(10), None).unwrap(),
            Duration::from_millis(2500)
        );
    }

    #[test]
    fn explicit_chunk_interval_overrides_default() {
        let interval = Duration::from_secs(90);
        assert_eq!(
            effective_chunk_interval(Duration::from_secs(10), Some(interval)).unwrap(),
            interval
        );
    }

    #[test]
    fn sql_interval_literal_keeps_subsecond_precision() {
        assert_eq!(
            sql_interval_literal(Duration::from_millis(1500)),
            "1.500000000 seconds"
        );
    }

    #[test]
    fn duration_from_nanos_preserves_subsecond_values() {
        assert_eq!(
            duration_from_nanos(2_500_000_000).unwrap(),
            Duration::from_millis(2500)
        );
    }

    #[test]
    fn policy_query_reconciles_retention_on_existing_tables() {
        let sql =
            timescaledb_policy_query("demo", Duration::from_secs(600), Duration::from_secs(150));
        assert!(sql.contains("set_chunk_time_interval"));
        assert!(sql.contains("remove_retention_policy"));
        assert!(sql.contains("if_exists => true"));
        assert!(sql.contains("add_retention_policy"));
        assert!(sql.contains("drop_after => INTERVAL '600 seconds'"));
    }
}

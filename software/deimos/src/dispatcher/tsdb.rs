//! Either reuse or create a new table in a TimescaleDB postgres
//! database and write to that table.

use std::io::Write;
use std::sync::mpsc::{Sender, channel};
use std::thread::{self, JoinHandle, spawn};
use std::time::{Duration, SystemTime};

use postgres::{Client, NoTls, Statement};
use postgres_types::{ToSql, Type};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::controller::context::ControllerCtx;

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
    retention_time_hours: u64,

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
        retention_time_hours: u64,
    ) -> Self {
        Self {
            dbname: dbname.to_owned(),
            host: host.to_owned(),
            user: user.to_owned(),
            token_name: token_name.to_owned(),
            buffer_time,
            retention_time_hours,
            worker: None,
        }
    }
}

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
        init_timescaledb_table(
            &mut client,
            channel_names,
            &ctx.op_name,
            self.retention_time_hours,
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
        let _thread = spawn(move || {
            // Bind to assigned core, and set priority only if the core is not shared with the control loop
            {
                core_affinity::set_for_current(core_affinity::CoreId {
                    id: core_assignment,
                });
                if core_assignment > 0 {
                    let _ = thread_priority::set_current_thread_priority(
                        thread_priority::ThreadPriority::Max,
                    );
                }
            }

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
                            csv_row(&mut stringbuf, vals);
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
        });

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
    // Connect to database backend
    Client::connect(
        &format!("dbname={dbname} host={host} user={user} password={pw}"),
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
    retention_time_hours: u64,
) -> Result<(), String> {
    // Check if the table exists
    let table_exists = !client
        .simple_query(&format!("SELECT 1 FROM public.{op_name};"))
        .unwrap_or_default()
        .is_empty();

    // If it exists but has the wrong schema, this will produce an error on the first write
    if table_exists {
        return Ok(());
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

            SELECT add_retention_policy(\'\"{op_name}\"\', INTERVAL '{retention_time_hours} hours');
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

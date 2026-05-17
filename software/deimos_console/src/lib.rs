/// Public library surface for the `deimos-console` viewer.
///
/// Exposes the receiver and config modules so they can be used in integration tests
/// and potentially by other tooling without going through the binary entry point.
pub mod config;
pub mod receiver;

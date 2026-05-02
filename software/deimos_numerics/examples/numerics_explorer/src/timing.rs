/// Times one closure and returns the value plus elapsed milliseconds.
pub fn measure<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let start = now_millis();
    let value = f();
    let millis = now_millis() - start;
    (value, millis)
}

/// Repeats one closure until the total elapsed time crosses the requested minimum
/// and returns the last value plus the average milliseconds per run.
pub fn measure_average_until<T>(
    min_total_millis: f64,
    mut f: impl FnMut() -> T,
) -> (T, f64, usize) {
    let min_total_millis = min_total_millis.max(0.0);
    let start = now_millis();
    let mut runs = 1_usize;
    let mut value = f();
    while now_millis() - start < min_total_millis {
        value = f();
        runs += 1;
    }
    let elapsed = now_millis() - start;
    (value, elapsed / runs as f64, runs)
}

#[cfg(target_arch = "wasm32")]
fn now_millis() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or(0.0, |performance| performance.now())
}

#[cfg(not(target_arch = "wasm32"))]
fn now_millis() -> f64 {
    use std::time::Instant;

    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1_000.0
}

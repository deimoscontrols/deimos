use std::path::{Path, PathBuf};

use deimos::controller::context::ControllerCtx;

/// Add the repository-local website records folder as a calibration store.
///
/// Examples are commonly run from a checkout that also contains
/// `deimos_website/docs/records`, so searching that folder lets example
/// controllers use checked-in calibration records before falling back to the
/// default missing-calibration behavior.
pub fn add_website_record_store(ctx: &mut ControllerCtx) {
    ctx.calibration_local_sources.push(website_record_store());
}

/// Return the records folder relative to the `deimos` crate manifest.
fn website_record_store() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../deimos_website/docs/records")
}

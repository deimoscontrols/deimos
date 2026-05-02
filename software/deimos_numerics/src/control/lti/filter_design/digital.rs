//! Digital-spec preprocessing helpers.
//!
//! The public digital design path accepts physical angular frequencies and
//! optionally prewarps them before the bilinear transform.
//!
//! # Glossary
//!
//! - **Prewarp:** Bilinear-transform frequency correction that preserves one
//!   chosen analog frequency exactly.
//! - **Bilinear transform:** Mapping from the analog `s` plane to the digital
//!   `z` plane.

use super::spec::FilterShape;
use faer_traits::RealField;
use num_traits::Float;

pub(super) fn maybe_prewarp_shape<R>(
    shape: FilterShape<R>,
    sample_rate: R,
    prewarp: bool,
) -> FilterShape<R>
where
    R: Float + RealField,
{
    if !prewarp {
        return shape;
    }
    match shape {
        FilterShape::Lowpass { cutoff } => FilterShape::Lowpass {
            cutoff: prewarp_frequency(cutoff, sample_rate),
        },
        FilterShape::Highpass { cutoff } => FilterShape::Highpass {
            cutoff: prewarp_frequency(cutoff, sample_rate),
        },
        FilterShape::Bandpass {
            low_cutoff,
            high_cutoff,
        } => FilterShape::Bandpass {
            low_cutoff: prewarp_frequency(low_cutoff, sample_rate),
            high_cutoff: prewarp_frequency(high_cutoff, sample_rate),
        },
        FilterShape::Bandstop {
            low_cutoff,
            high_cutoff,
        } => FilterShape::Bandstop {
            low_cutoff: prewarp_frequency(low_cutoff, sample_rate),
            high_cutoff: prewarp_frequency(high_cutoff, sample_rate),
        },
    }
}

pub(super) fn maybe_unprewarp_shape<R>(
    shape: FilterShape<R>,
    sample_rate: R,
    prewarp: bool,
) -> FilterShape<R>
where
    R: Float + RealField,
{
    if !prewarp {
        return shape;
    }
    match shape {
        FilterShape::Lowpass { cutoff } => FilterShape::Lowpass {
            cutoff: unprewarp_frequency(cutoff, sample_rate),
        },
        FilterShape::Highpass { cutoff } => FilterShape::Highpass {
            cutoff: unprewarp_frequency(cutoff, sample_rate),
        },
        FilterShape::Bandpass {
            low_cutoff,
            high_cutoff,
        } => FilterShape::Bandpass {
            low_cutoff: unprewarp_frequency(low_cutoff, sample_rate),
            high_cutoff: unprewarp_frequency(high_cutoff, sample_rate),
        },
        FilterShape::Bandstop {
            low_cutoff,
            high_cutoff,
        } => FilterShape::Bandstop {
            low_cutoff: unprewarp_frequency(low_cutoff, sample_rate),
            high_cutoff: unprewarp_frequency(high_cutoff, sample_rate),
        },
    }
}

pub(super) fn prewarp_frequency<R>(omega: R, sample_rate: R) -> R
where
    R: Float + RealField,
{
    // For the bilinear transform z = (1 + s/(2fs)) / (1 - s/(2fs)), the
    // corresponding analog frequency is 2fs * tan(omega / (2fs)).
    let two = R::one() + R::one();
    two * sample_rate * (omega / (two * sample_rate)).tan()
}

fn unprewarp_frequency<R>(omega: R, sample_rate: R) -> R
where
    R: Float + RealField,
{
    let two = R::one() + R::one();
    two * sample_rate * (omega / (two * sample_rate)).atan()
}

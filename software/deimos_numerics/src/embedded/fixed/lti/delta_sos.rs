//! Fixed-size delta-operator SOS runtime filters.
//!
//! This is the embedded execution form for deployed discrete IIR filters. It
//! is meant to be produced from the alloc-side control representation after
//! design, not used as the primary design or interchange form.
//!
//! # Glossary
//!
//! - **Delta operator:** The discrete operator `δ = (1 - z^-1) / dt`.

use crate::embedded::error::EmbeddedError;
use crate::embedded::math::ensure_finite;
use num_traits::{Float, NumCast};

/// One fixed-size delta-operator filter section.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DeltaSection<T> {
    /// Pure direct feedthrough section.
    Direct { d: T },
    /// First-order delta section.
    First { alpha0: T, c0: T, d: T },
    /// Second-order delta section.
    Second {
        alpha0: T,
        alpha1: T,
        c1: T,
        c2: T,
        d: T,
    },
}

/// Fixed-size delta-SOS cascade shared across `LANES` independent channels.
///
/// This is an execution-only embedded representation of a discrete SOS
/// cascade. It is intended to hold already-designed filter sections in a
/// runtime basis that behaves better at low normalized cutoff.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeltaSos<T, const SECTIONS: usize, const LANES: usize> {
    sections: [DeltaSection<T>; SECTIONS],
    gain: T,
    sample_time: T,
}

/// Caller-owned runtime state for one [`DeltaSos`] filter bank.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeltaSosState<T, const SECTIONS: usize, const LANES: usize> {
    /// Per-section, per-lane delta state `[x1, x2]`.
    pub section_state: [[[T; 2]; LANES]; SECTIONS],
}

impl<T, const SECTIONS: usize, const LANES: usize> DeltaSosState<T, SECTIONS, LANES>
where
    T: Float,
{
    /// Returns the zero-initialized delta state.
    #[must_use]
    pub fn zeros() -> Self {
        Self {
            section_state: [[[T::zero(); 2]; LANES]; SECTIONS],
        }
    }

    /// Resets the stored section state to zero.
    pub fn reset(&mut self) {
        *self = Self::zeros();
    }
}

impl<T, const SECTIONS: usize, const LANES: usize> Default for DeltaSosState<T, SECTIONS, LANES>
where
    T: Float,
{
    fn default() -> Self {
        Self::zeros()
    }
}

impl<T, const SECTIONS: usize, const LANES: usize> DeltaSos<T, SECTIONS, LANES>
where
    T: Float,
{
    /// Creates a fixed-size delta-SOS cascade.
    ///
    /// This constructor expects coefficients that are already in delta runtime
    /// form. Typical callers should obtain those coefficients by converting a
    /// designed ordinary SOS filter in the alloc-side control layer.
    pub fn new(
        sections: [DeltaSection<T>; SECTIONS],
        gain: T,
        sample_time: T,
    ) -> Result<Self, EmbeddedError> {
        if !sample_time.is_finite() || sample_time <= T::zero() {
            return Err(EmbeddedError::InvalidSampleTime);
        }
        Ok(Self {
            sections,
            gain,
            sample_time,
        })
    }

    /// Returns the section list in cascade order.
    #[must_use]
    pub fn sections(&self) -> &[DeltaSection<T>; SECTIONS] {
        &self.sections
    }

    /// Returns the overall input gain.
    #[must_use]
    pub fn gain(&self) -> T {
        self.gain
    }

    /// Returns the stored sample interval.
    #[must_use]
    pub fn sample_time(&self) -> T {
        self.sample_time
    }

    /// Returns a fresh zero state sized for this filter bank.
    #[must_use]
    pub fn reset_state(&self) -> DeltaSosState<T, SECTIONS, LANES> {
        DeltaSosState::zeros()
    }

    /// Evaluates one multichannel timestep.
    pub fn step(
        &self,
        state: &mut DeltaSosState<T, SECTIONS, LANES>,
        input: [T; LANES],
    ) -> [T; LANES] {
        let mut output = input;
        let dt = self.sample_time;

        for lane in 0..LANES {
            output[lane] = output[lane] * self.gain;
        }

        for section_idx in 0..SECTIONS {
            let section = self.sections[section_idx];
            for lane in 0..LANES {
                let sample = output[lane];
                let state_lane = &mut state.section_state[section_idx][lane];
                output[lane] = match section {
                    DeltaSection::Direct { d } => d * sample,
                    DeltaSection::First { alpha0, c0, d } => {
                        let x = state_lane[0];
                        let y = c0 * x + d * sample;
                        state_lane[0] = x + dt * (-alpha0 * x + sample);
                        state_lane[1] = T::zero();
                        y
                    }
                    DeltaSection::Second {
                        alpha0,
                        alpha1,
                        c1,
                        c2,
                        d,
                    } => {
                        let x1 = state_lane[0];
                        let x2 = state_lane[1];
                        let y = c1 * x1 + c2 * x2 + d * sample;
                        state_lane[0] = x1 + dt * x2;
                        state_lane[1] = x2 + dt * (-alpha0 * x1 - alpha1 * x2 + sample);
                        y
                    }
                };
            }
        }

        output
    }

    /// Filters one caller-owned multichannel block into the destination slice.
    pub fn filter_into(
        &self,
        state: &mut DeltaSosState<T, SECTIONS, LANES>,
        input: &[[T; LANES]],
        output: &mut [[T; LANES]],
    ) -> Result<(), EmbeddedError> {
        if input.len() != output.len() {
            return Err(EmbeddedError::LengthMismatch {
                which: "delta_sos.filter_into",
                expected: input.len(),
                actual: output.len(),
            });
        }

        for idx in 0..input.len() {
            output[idx] = self.step(state, input[idx]);
        }
        Ok(())
    }

    /// Returns the scalar DC gain of the full cascade.
    pub fn dc_gain(&self) -> Result<T, EmbeddedError> {
        let mut gain = self.gain;
        for idx in 0..SECTIONS {
            gain = gain
                * match self.sections[idx] {
                    DeltaSection::Direct { d } => d,
                    DeltaSection::First { alpha0, c0, d } => d + c0 / alpha0,
                    DeltaSection::Second { alpha0, c1, d, .. } => d + c1 / alpha0,
                };
        }
        ensure_finite(gain, "delta_sos.dc_gain")
    }

    /// Casts the stored coefficients and sample time to another scalar dtype.
    pub fn try_cast<S>(&self) -> Result<DeltaSos<S, SECTIONS, LANES>, EmbeddedError>
    where
        S: Float,
    {
        let sections = self.sections.map(|section| match section {
            DeltaSection::Direct { d } => Ok(DeltaSection::Direct {
                d: NumCast::from(d).ok_or(EmbeddedError::InvalidParameter {
                    which: "delta_sos.section.d",
                })?,
            }),
            DeltaSection::First { alpha0, c0, d } => Ok(DeltaSection::First {
                alpha0: NumCast::from(alpha0).ok_or(EmbeddedError::InvalidParameter {
                    which: "delta_sos.section.alpha0",
                })?,
                c0: NumCast::from(c0).ok_or(EmbeddedError::InvalidParameter {
                    which: "delta_sos.section.c0",
                })?,
                d: NumCast::from(d).ok_or(EmbeddedError::InvalidParameter {
                    which: "delta_sos.section.d",
                })?,
            }),
            DeltaSection::Second {
                alpha0,
                alpha1,
                c1,
                c2,
                d,
            } => Ok(DeltaSection::Second {
                alpha0: NumCast::from(alpha0).ok_or(EmbeddedError::InvalidParameter {
                    which: "delta_sos.section.alpha0",
                })?,
                alpha1: NumCast::from(alpha1).ok_or(EmbeddedError::InvalidParameter {
                    which: "delta_sos.section.alpha1",
                })?,
                c1: NumCast::from(c1).ok_or(EmbeddedError::InvalidParameter {
                    which: "delta_sos.section.c1",
                })?,
                c2: NumCast::from(c2).ok_or(EmbeddedError::InvalidParameter {
                    which: "delta_sos.section.c2",
                })?,
                d: NumCast::from(d).ok_or(EmbeddedError::InvalidParameter {
                    which: "delta_sos.section.d",
                })?,
            }),
        });

        let mut cast_sections = [DeltaSection::Direct { d: S::zero() }; SECTIONS];
        for idx in 0..SECTIONS {
            cast_sections[idx] = sections[idx]?;
        }

        DeltaSos::new(
            cast_sections,
            NumCast::from(self.gain).ok_or(EmbeddedError::InvalidParameter {
                which: "delta_sos.gain",
            })?,
            NumCast::from(self.sample_time).ok_or(EmbeddedError::InvalidParameter {
                which: "delta_sos.sample_time",
            })?,
        )
    }
}

#[cfg(feature = "alloc")]
impl<T, const SECTIONS: usize, const LANES: usize> TryFrom<&crate::control::lti::DeltaSos<T>>
    for DeltaSos<T, SECTIONS, LANES>
where
    T: Float + faer_traits::RealField,
{
    type Error = EmbeddedError;

    /// Converts the dynamic control-side delta-SOS into a fixed-size embedded
    /// representation.
    fn try_from(value: &crate::control::lti::DeltaSos<T>) -> Result<Self, Self::Error> {
        if value.sections().len() != SECTIONS {
            return Err(EmbeddedError::SectionCountMismatch {
                expected: SECTIONS,
                actual: value.sections().len(),
            });
        }

        let mut sections = [DeltaSection::Direct { d: T::zero() }; SECTIONS];
        for idx in 0..SECTIONS {
            sections[idx] = match value.sections()[idx] {
                crate::control::lti::DeltaSection::Direct { d } => DeltaSection::Direct { d },
                crate::control::lti::DeltaSection::First { alpha0, c0, d } => {
                    DeltaSection::First { alpha0, c0, d }
                }
                crate::control::lti::DeltaSection::Second {
                    alpha0,
                    alpha1,
                    c1,
                    c2,
                    d,
                } => DeltaSection::Second {
                    alpha0,
                    alpha1,
                    c1,
                    c2,
                    d,
                },
            };
        }

        Self::new(sections, value.gain(), value.sample_time())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_delta_sos_runs_multilane_block() {
        let filter = DeltaSos::<f64, 2, 2>::new(
            [
                DeltaSection::First {
                    alpha0: 2.0,
                    c0: 1.0,
                    d: 0.0,
                },
                DeltaSection::Direct { d: 0.5 },
            ],
            1.0,
            0.1,
        )
        .unwrap();
        let mut state = filter.reset_state();
        let input = [[1.0, -1.0]; 4];
        let mut output = [[0.0, 0.0]; 4];
        filter.filter_into(&mut state, &input, &mut output).unwrap();

        assert!(output.iter().flatten().all(|value| value.is_finite()));
        assert!(filter.dc_gain().unwrap().is_finite());
    }
}

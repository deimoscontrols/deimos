//! Digital finite impulse-response filters and Savitzky-Golay design.
//!
//! FIR filtering is intentionally tap-native in this crate. Unlike the IIR
//! layer, there is no benefit in forcing runtime execution through state-space
//! or polynomial-recurrence machinery when the taps themselves are already the
//! numerically natural representation.
//!
//! # Two Intuitions
//!
//! 1. **Convolution view.** FIR filtering is just a sliding weighted average of
//!    the recent input history.
//! 2. **Polynomial-fit view.** Savitzky-Golay design interprets those weights
//!    as the coefficients of a local least-squares polynomial fit evaluated at
//!    the window center.
//!
//! # Glossary
//!
//! - **Tap:** One FIR coefficient.
//! - **Group delay:** Constant sample delay of a linear-phase FIR.
//! - **Savitzky-Golay filter:** FIR smoother or differentiator obtained from a
//!   local polynomial least-squares fit.
//!
//! # Mathematical Formulation
//!
//! FIR output is
//!
//! - `y[k] = sum_i h[i] u[k-i]`
//!
//! and Savitzky-Golay taps are formed from the pseudoinverse of a local
//! Vandermonde design matrix, optionally differentiated at the center sample.
//!
//! # Implementation Notes
//!
//! - FIR runtime execution stays native to the tap representation.
//! - `filtfilt` shares the padding policy with the IIR simulation module.
//! - Savitzky-Golay design uses a direct small dense normal-equations solve,
//!   which matches the actual problem size and keeps the implementation simple.

use super::sim::{padded_sample, resolve_pad_len};
use super::{
    BodeData, DiscreteTransferFunction, FiltFiltParams, FilteredSignal, LtiError, PoleZeroData,
    StatefulFilteredSignal,
};
use crate::scalar::real_complex_mul_add;
use crate::sparse::compensated::{CompensatedField, CompensatedSum};
use alloc::vec::Vec;
use faer::Mat;
use faer::complex::Complex;
use faer::prelude::Solve;
use faer_traits::RealField;
use num_traits::Float;

/// Digital finite impulse-response filter.
#[derive(Clone, Debug, PartialEq)]
pub struct Fir<R> {
    taps: Vec<R>,
    sample_time: R,
}

/// Stateful FIR delay-line storage for chunked filtering.
#[derive(Clone, Debug, PartialEq)]
pub struct FirFilterState<R> {
    /// Previous input samples, stored from newest to oldest.
    pub delay_line: Vec<R>,
}

impl<R> FirFilterState<R>
where
    R: Float,
{
    /// Creates a zero-initialized delay line of the requested length.
    #[must_use]
    pub fn zeros(delay_len: usize) -> Self {
        Self {
            delay_line: vec![R::zero(); delay_len],
        }
    }

    /// Creates a zero-initialized state sized for a particular filter.
    #[must_use]
    pub fn for_filter(filter: &Fir<R>) -> Self {
        Self::zeros(filter.taps.len().saturating_sub(1))
    }
}

/// Savitzky-Golay design specification.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SavGolSpec<R> {
    /// Odd sliding-window length.
    pub window_len: usize,
    /// Polynomial fit order.
    pub poly_order: usize,
    /// Requested derivative order evaluated at the window center.
    pub derivative_order: usize,
    /// Uniform sample spacing between consecutive samples.
    pub sample_spacing: R,
}

impl<R> SavGolSpec<R>
where
    R: Float + RealField,
{
    /// Creates and validates a Savitzky-Golay design specification.
    pub fn new(
        window_len: usize,
        poly_order: usize,
        derivative_order: usize,
        sample_spacing: R,
    ) -> Result<Self, LtiError> {
        validate_savgol_spec(window_len, poly_order, derivative_order, sample_spacing)?;
        Ok(Self {
            window_len,
            poly_order,
            derivative_order,
            sample_spacing,
        })
    }

    /// Center index of the odd-length fitting window.
    #[must_use]
    pub fn center_index(&self) -> usize {
        self.window_len / 2
    }
}

impl<R> Fir<R>
where
    R: Float + RealField + CompensatedField,
{
    /// Creates a validated digital FIR filter.
    pub fn new(taps: impl Into<Vec<R>>, sample_time: R) -> Result<Self, LtiError> {
        if !sample_time.is_finite() || sample_time <= R::zero() {
            return Err(LtiError::InvalidSampleTime);
        }
        let taps = taps.into();
        if taps.is_empty() {
            return Err(LtiError::EmptyFir);
        }
        if taps.iter().any(|tap| !tap.is_finite()) {
            return Err(LtiError::NonFiniteResult { which: "fir.taps" });
        }
        Ok(Self { taps, sample_time })
    }

    /// Tap coefficients from newest to oldest sample contribution.
    #[must_use]
    pub fn taps(&self) -> &[R] {
        &self.taps
    }

    /// Sampling interval carried by the FIR representation.
    #[must_use]
    pub fn sample_time(&self) -> R {
        self.sample_time
    }

    /// Number of taps.
    #[must_use]
    pub fn len(&self) -> usize {
        self.taps.len()
    }

    /// Returns whether the tap vector is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.taps.is_empty()
    }

    /// FIR order, equal to `len() - 1`.
    #[must_use]
    pub fn order(&self) -> usize {
        self.taps.len() - 1
    }

    /// Returns the DC gain, equal to the sum of the taps.
    #[must_use]
    pub fn dc_gain(&self) -> R {
        let mut acc = CompensatedSum::<R>::default();
        for &tap in &self.taps {
            acc.add(tap);
        }
        acc.finish()
    }

    /// Checks whether the taps are symmetric under reversal.
    #[must_use]
    pub fn is_symmetric(&self, tol: R) -> bool {
        self.taps
            .iter()
            .zip(self.taps.iter().rev())
            .all(|(&lhs, &rhs)| (lhs - rhs).abs() <= tol)
    }

    /// Checks whether the taps are antisymmetric under reversal.
    #[must_use]
    pub fn is_antisymmetric(&self, tol: R) -> bool {
        self.taps
            .iter()
            .zip(self.taps.iter().rev())
            .all(|(&lhs, &rhs)| (lhs + rhs).abs() <= tol)
    }

    /// Returns the linear-phase group delay in samples when the taps are
    /// symmetric or antisymmetric.
    #[must_use]
    pub fn group_delay_samples(&self, tol: R) -> Option<R> {
        if self.is_symmetric(tol) || self.is_antisymmetric(tol) {
            Some(R::from(self.taps.len() - 1).unwrap() / (R::one() + R::one()))
        } else {
            None
        }
    }

    /// Converts the FIR taps into the crate's discrete transfer-function form.
    ///
    /// The taps implement
    ///
    /// `H(z) = h[0] + h[1] z^-1 + ... + h[n-1] z^-(n-1)`
    ///
    /// which becomes a proper rational transfer function with denominator
    /// `z^(n-1)` in the polynomial storage used by `DiscreteTransferFunction`.
    pub fn to_transfer_function(&self) -> Result<DiscreteTransferFunction<R>, LtiError> {
        let mut denominator = vec![R::one()];
        denominator.resize(self.taps.len(), R::zero());
        DiscreteTransferFunction::discrete(self.taps.clone(), denominator, self.sample_time)
    }

    /// Returns Bode-plot data by chaining through the discrete transfer
    /// function representation.
    pub fn bode_data(&self, angular_frequencies: &[R]) -> Result<BodeData<R>, LtiError> {
        self.to_transfer_function()?.bode_data(angular_frequencies)
    }

    /// Returns poles and zeros by chaining through the discrete transfer
    /// function representation.
    pub fn pole_zero_data(&self) -> Result<PoleZeroData<R>, LtiError> {
        self.to_transfer_function()?.pole_zero_data()
    }

    /// Evaluates the discrete-time frequency response on the unit circle.
    pub fn frequency_response(
        &self,
        angular_frequencies: &[R],
    ) -> Result<Vec<Complex<R>>, LtiError> {
        if angular_frequencies
            .iter()
            .any(|omega| !omega.is_finite() || *omega < R::zero())
        {
            return Err(LtiError::InvalidSamplePoint {
                which: "fir.frequency_response",
            });
        }

        let mut values = Vec::with_capacity(angular_frequencies.len());
        for &omega in angular_frequencies {
            let mut acc = Complex::new(R::zero(), R::zero());
            for (k, &tap) in self.taps.iter().enumerate() {
                let phase = -(omega * self.sample_time * R::from(k).unwrap());
                let z_inv = Complex::new(phase.cos(), phase.sin());
                acc = real_complex_mul_add(tap, z_inv, acc);
            }
            values.push(acc);
        }
        Ok(values)
    }

    /// Filters one input slice causally with zero initial delay-line state.
    pub fn filter_forward(
        &self,
        input: &[R],
    ) -> Result<StatefulFilteredSignal<R, FirFilterState<R>>, LtiError> {
        let mut state = FirFilterState::for_filter(self);
        let output = self.filter_forward_stateful(&mut state, input)?;
        Ok(StatefulFilteredSignal {
            output: output.output,
            final_state: state,
        })
    }

    /// Filters one input slice causally while updating a caller-supplied delay
    /// line.
    pub fn filter_forward_stateful(
        &self,
        state: &mut FirFilterState<R>,
        input: &[R],
    ) -> Result<FilteredSignal<R>, LtiError> {
        validate_fir_state_len(self, state)?;
        let mut output = Vec::with_capacity(input.len());
        for &sample in input {
            output.push(fir_step(&self.taps, &mut state.delay_line, sample));
        }
        Ok(FilteredSignal { output })
    }

    /// Runs forward-backward zero-phase filtering with the default padding
    /// policy.
    pub fn filtfilt(&self, input: &[R]) -> Result<FilteredSignal<R>, LtiError> {
        self.filtfilt_with_params(input, &FiltFiltParams::default())
    }

    /// Runs forward-backward zero-phase filtering with explicit padding
    /// control.
    pub fn filtfilt_with_params(
        &self,
        input: &[R],
        params: &FiltFiltParams,
    ) -> Result<FilteredSignal<R>, LtiError> {
        if input.is_empty() {
            return Ok(FilteredSignal { output: Vec::new() });
        }

        let pad_len = resolve_pad_len(input.len(), params, 3 * self.order());
        let total_len = input.len() + 2 * pad_len;

        let mut state = FirFilterState::for_filter(self);
        let mut first_pass = Vec::with_capacity(total_len);
        // The padding is sampled logically through `padded_sample` rather than
        // by allocating a full padded copy of the signal.
        for idx in 0..total_len {
            let sample = padded_sample(input, params.mode, pad_len, idx);
            first_pass.push(fir_step(&self.taps, &mut state.delay_line, sample));
        }

        first_pass.reverse();

        state = FirFilterState::for_filter(self);
        let mut second_pass = Vec::with_capacity(total_len);
        for &sample in &first_pass {
            second_pass.push(fir_step(&self.taps, &mut state.delay_line, sample));
        }

        second_pass.reverse();
        Ok(FilteredSignal {
            output: second_pass[pad_len..(pad_len + input.len())].to_vec(),
        })
    }
}

/// Designs a Savitzky-Golay FIR kernel for smoothing or derivative
/// estimation.
pub fn design_savgol<R>(spec: &SavGolSpec<R>) -> Result<Fir<R>, LtiError>
where
    R: Float + RealField + CompensatedField,
{
    validate_savgol_spec(
        spec.window_len,
        spec.poly_order,
        spec.derivative_order,
        spec.sample_spacing,
    )?;

    let half = spec.window_len / 2;
    let a = Mat::from_fn(spec.window_len, spec.poly_order + 1, |row, col| {
        let offset = row as isize - half as isize;
        R::from(offset).unwrap().powi(col as i32)
    });

    // The fitting problem is always tiny in this API, so form the normal
    // equations directly instead of routing through the large-problem SVD
    // machinery. The requested Savitzky-Golay row is:
    //
    // A^+ = (A^T A)^-1 A^T
    // taps = e_d^T A^+
    //
    // which we assemble by solving `(A^T A) q = e_d` and then evaluating
    // `A q` at each sample location.
    let gram = Mat::from_fn(spec.poly_order + 1, spec.poly_order + 1, |row, col| {
        let mut acc = CompensatedSum::<R>::default();
        for sample_idx in 0..spec.window_len {
            acc.add(a[(sample_idx, row)] * a[(sample_idx, col)]);
        }
        acc.finish()
    });
    let rhs = Mat::from_fn(spec.poly_order + 1, 1, |row, _| {
        if row == spec.derivative_order {
            R::one()
        } else {
            R::zero()
        }
    });
    let coeffs = gram.full_piv_lu().solve(rhs.as_ref());
    if !coeffs.as_ref().is_all_finite() {
        return Err(LtiError::InvalidSavGolSpec {
            which: "rank_deficient_design",
        });
    }

    let deriv_scale = factorial_as_real::<R>(spec.derivative_order)
        / spec.sample_spacing.powi(spec.derivative_order as i32);
    let taps = (0..spec.window_len)
        .map(|sample_idx| {
            let mut acc = CompensatedSum::<R>::default();
            for basis_idx in 0..=spec.poly_order {
                acc.add(a[(sample_idx, basis_idx)] * coeffs[(basis_idx, 0)]);
            }
            deriv_scale * acc.finish()
        })
        .collect::<Vec<_>>();

    Fir::new(taps, spec.sample_spacing)
}

/// Validates that a reusable FIR delay line matches the filter order.
fn validate_fir_state_len<R>(filter: &Fir<R>, state: &FirFilterState<R>) -> Result<(), LtiError> {
    let expected = filter.taps.len().saturating_sub(1);
    if state.delay_line.len() == expected {
        Ok(())
    } else {
        Err(LtiError::InvalidFilterStateLength {
            which: "fir_filter_state",
            expected,
            actual: state.delay_line.len(),
        })
    }
}

/// Advances one direct-convolution FIR step.
///
/// The delay line stores previous inputs from newest to oldest, so the causal
/// recurrence is just `h[0] * x[k] + h[1] * x[k-1] + ...`.
fn fir_step<R>(taps: &[R], delay_line: &mut [R], input: R) -> R
where
    R: Float + RealField + CompensatedField,
{
    let mut acc = CompensatedSum::<R>::default();
    acc.add(taps[0] * input);
    for (tap, &sample) in taps.iter().skip(1).zip(delay_line.iter()) {
        acc.add(*tap * sample);
    }
    let output = acc.finish();

    for idx in (1..delay_line.len()).rev() {
        delay_line[idx] = delay_line[idx - 1];
    }
    if let Some(first) = delay_line.first_mut() {
        *first = input;
    }
    output
}

/// Validates the structural constraints of a Savitzky-Golay specification.
fn validate_savgol_spec<R>(
    window_len: usize,
    poly_order: usize,
    derivative_order: usize,
    sample_spacing: R,
) -> Result<(), LtiError>
where
    R: Float + RealField,
{
    if window_len == 0 || window_len.is_multiple_of(2) {
        return Err(LtiError::InvalidSavGolSpec {
            which: "window_len",
        });
    }
    if poly_order >= window_len {
        return Err(LtiError::InvalidSavGolSpec {
            which: "poly_order",
        });
    }
    if derivative_order > poly_order {
        return Err(LtiError::InvalidSavGolSpec {
            which: "derivative_order",
        });
    }
    if !sample_spacing.is_finite() || sample_spacing <= R::zero() {
        return Err(LtiError::InvalidSavGolSpec {
            which: "sample_spacing",
        });
    }
    Ok(())
}

/// Returns `n!` as the target real scalar type.
///
/// Savitzky-Golay derivative kernels differ from the smoothing kernel by the
/// usual factorial scale coming from the derivative of the fitted polynomial at
/// the window center.
fn factorial_as_real<R>(n: usize) -> R
where
    R: Float,
{
    (1..=n).fold(R::one(), |acc, value| acc * R::from(value).unwrap())
}

#[cfg(test)]
mod tests {
    use super::{Fir, FirFilterState, SavGolSpec, design_savgol};
    use crate::control::lti::{FiltFiltPadLen, FiltFiltParams, LtiError};
    use alloc::vec::Vec;

    fn assert_close(lhs: f64, rhs: f64, tol: f64) {
        let err = (lhs - rhs).abs();
        assert!(err <= tol, "lhs={lhs}, rhs={rhs}, err={err}, tol={tol}");
    }

    fn assert_vec_close(lhs: &[f64], rhs: &[f64], tol: f64) {
        assert_eq!(lhs.len(), rhs.len());
        for (&lhs, &rhs) in lhs.iter().zip(rhs.iter()) {
            assert_close(lhs, rhs, tol);
        }
    }

    #[test]
    fn fir_constructor_rejects_empty_taps() {
        let err = Fir::<f64>::new(Vec::new(), 1.0).unwrap_err();
        assert!(matches!(err, LtiError::EmptyFir));
    }

    #[test]
    fn fir_forward_impulse_reproduces_taps() {
        let fir = Fir::new(vec![1.0, -0.5, 0.25], 1.0).unwrap();
        let filtered = fir.filter_forward(&[1.0, 0.0, 0.0]).unwrap();
        assert_vec_close(&filtered.output, fir.taps(), 1.0e-12);
    }

    #[test]
    fn fir_stateful_chunked_processing_matches_one_shot() {
        let fir = Fir::new(vec![1.0, 2.0, 3.0], 1.0).unwrap();
        let input = [1.0, -1.0, 0.5, 2.0, 0.0];

        let one_shot = fir.filter_forward(&input).unwrap();
        let mut state = FirFilterState::for_filter(&fir);
        let first = fir
            .filter_forward_stateful(&mut state, &input[..2])
            .unwrap();
        let second = fir
            .filter_forward_stateful(&mut state, &input[2..])
            .unwrap();
        let mut combined = first.output;
        combined.extend(second.output);
        assert_vec_close(&combined, &one_shot.output, 1.0e-12);
    }

    #[test]
    fn fir_filtfilt_preserves_constant_signal() {
        let fir = Fir::new(vec![0.25, 0.5, 0.25], 1.0).unwrap();
        let input = vec![2.0; 16];
        let output = fir.filtfilt(&input).unwrap();
        for value in output.output {
            assert_close(value, 2.0, 1.0e-12);
        }
    }

    #[test]
    fn fir_filtfilt_shortens_padding_on_short_signals() {
        let fir = Fir::new(vec![0.25, 0.5, 0.25], 1.0).unwrap();
        let params = FiltFiltParams::new().with_len(FiltFiltPadLen::Exact(99));
        let output = fir.filtfilt_with_params(&[1.0, 2.0], &params).unwrap();
        assert_eq!(output.output.len(), 2);
    }

    #[test]
    fn fir_helpers_report_symmetry_and_group_delay() {
        let fir = Fir::new(vec![1.0, 2.0, 1.0], 1.0).unwrap();
        assert!(fir.is_symmetric(1.0e-12));
        assert!(!fir.is_antisymmetric(1.0e-12));
        assert_close(fir.group_delay_samples(1.0e-12).unwrap(), 1.0, 1.0e-12);
        assert_close(fir.dc_gain(), 4.0, 1.0e-12);
    }

    #[test]
    fn savgol_smoothing_preserves_quadratic_center_value() {
        let spec = SavGolSpec::new(5, 2, 0, 1.0).unwrap();
        let fir = design_savgol(&spec).unwrap();
        assert!(fir.is_symmetric(1.0e-12));

        let samples = (-2..=2)
            .map(|x| {
                let x = x as f64;
                1.0 + 2.0 * x + 3.0 * x * x
            })
            .collect::<Vec<_>>();
        let value = fir
            .taps()
            .iter()
            .zip(samples.iter())
            .map(|(&tap, &sample)| tap * sample)
            .sum::<f64>();
        assert_close(value, 1.0, 1.0e-12);
    }

    #[test]
    fn savgol_first_derivative_preserves_quadratic_derivative_at_center() {
        let spec = SavGolSpec::new(5, 2, 1, 1.0).unwrap();
        let fir = design_savgol(&spec).unwrap();
        assert!(fir.is_antisymmetric(1.0e-12));

        let samples = (-2..=2)
            .map(|x| {
                let x = x as f64;
                1.0 + 2.0 * x + 3.0 * x * x
            })
            .collect::<Vec<_>>();
        let value = fir
            .taps()
            .iter()
            .zip(samples.iter())
            .map(|(&tap, &sample)| tap * sample)
            .sum::<f64>();
        assert_close(value, 2.0, 1.0e-12);
    }

    #[test]
    fn savgol_high_order_smoothing_preserves_dc_gain() {
        let spec = SavGolSpec::new(21, 10, 0, 1.0).unwrap();
        let fir = design_savgol(&spec).unwrap();
        assert!(fir.is_symmetric(1.0e-8));
        assert_close(fir.dc_gain(), 1.0, 1.0e-8);
    }

    #[test]
    fn savgol_rejects_invalid_specs() {
        let err = SavGolSpec::new(4, 2, 0, 1.0).unwrap_err();
        assert!(matches!(
            err,
            LtiError::InvalidSavGolSpec {
                which: "window_len"
            }
        ));

        let err = SavGolSpec::new(5, 5, 0, 1.0).unwrap_err();
        assert!(matches!(
            err,
            LtiError::InvalidSavGolSpec {
                which: "poly_order"
            }
        ));

        let err = SavGolSpec::new(5, 2, 3, 1.0).unwrap_err();
        assert!(matches!(
            err,
            LtiError::InvalidSavGolSpec {
                which: "derivative_order"
            }
        ));
    }
}

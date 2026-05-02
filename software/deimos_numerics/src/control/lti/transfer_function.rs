use super::error::LtiError;
use super::sos::Sos;
use super::util::{
    CompositionDomain, cast_real_scalar, is_zero_polynomial, normalize_ratio, poly_add_aligned,
    poly_eval, poly_mul, poly_roots, poly_sub_aligned, real_poly_from_roots, trim_leading_zeros,
    validate_sample_time,
};
use super::zpk::Zpk;
use super::{ContinuousStateSpace, ContinuousTime, DiscreteStateSpace, DiscreteTime};
use crate::decomp::dense_eigenvalues;
use alloc::vec::Vec;
use faer::complex::Complex;
use faer::prelude::Solve;
use faer::{Mat, MatRef};
use faer_traits::RealField;
use faer_traits::ext::ComplexFieldExt;
use faer_traits::math_utils::{eps, from_f64};
use num_traits::{Float, NumCast};

/// Real-coefficient single-input single-output transfer function.
///
/// Coefficients are stored in descending-power order.
#[derive(Clone, Debug, PartialEq)]
pub struct TransferFunction<R, Domain> {
    numerator: Vec<R>,
    denominator: Vec<R>,
    domain: Domain,
}

/// Continuous-time SISO transfer function.
pub type ContinuousTransferFunction<R> = TransferFunction<R, ContinuousTime>;

/// Discrete-time SISO transfer function.
pub type DiscreteTransferFunction<R> = TransferFunction<R, DiscreteTime<R>>;

impl<R, Domain> TransferFunction<R, Domain>
where
    R: Float + RealField,
    Domain: Clone,
{
    /// Creates a normalized transfer function from numerator/denominator
    /// coefficients in descending-power order.
    pub fn new(
        numerator: impl Into<Vec<R>>,
        denominator: impl Into<Vec<R>>,
        domain: Domain,
    ) -> Result<Self, LtiError> {
        let (numerator, denominator) = normalize_ratio(&numerator.into(), &denominator.into())?;
        Ok(Self {
            numerator,
            denominator,
            domain,
        })
    }

    /// Numerator coefficients in descending-power order.
    #[must_use]
    pub fn numerator(&self) -> &[R] {
        &self.numerator
    }

    /// Denominator coefficients in descending-power order.
    #[must_use]
    pub fn denominator(&self) -> &[R] {
        &self.denominator
    }

    /// Domain metadata carried by the transfer function.
    #[must_use]
    pub fn domain(&self) -> &Domain {
        &self.domain
    }

    /// Evaluates the rational transfer function at the supplied complex point.
    #[must_use]
    pub fn evaluate(&self, point: Complex<R>) -> Complex<R> {
        poly_eval(&self.numerator, point) / poly_eval(&self.denominator, point)
    }

    /// Converts coefficient form into zeros/poles/gain form.
    pub fn to_zpk(&self) -> Result<Zpk<R, Domain>, LtiError> {
        let zeros = poly_roots(&self.numerator)?;
        let poles = poly_roots(&self.denominator)?;
        let gain = self.numerator[0] / self.denominator[0];
        Zpk::new(zeros, poles, gain, self.domain.clone())
    }

    /// Converts the transfer function into a second-order-section cascade.
    pub fn to_sos(&self) -> Result<Sos<R, Domain>, LtiError> {
        self.to_zpk()?.to_sos()
    }
}

impl<R, Domain> TransferFunction<R, Domain>
where
    R: Float + RealField,
    Domain: CompositionDomain<R>,
{
    /// Forms the parallel composition `self + rhs`.
    ///
    /// In transfer-function form this is done with a common denominator:
    ///
    /// `N₁ / D₁ + N₂ / D₂ = (N₁ D₂ + N₂ D₁) / (D₁ D₂)`.
    pub fn add(&self, rhs: &Self) -> Result<Self, LtiError> {
        let domain = Domain::composed(self.domain(), rhs.domain())?;
        let lhs_num = poly_mul(self.numerator(), rhs.denominator());
        let rhs_num = poly_mul(rhs.numerator(), self.denominator());
        let numerator = poly_add_aligned(&lhs_num, &rhs_num);
        let denominator = poly_mul(self.denominator(), rhs.denominator());
        Self::new(numerator, denominator, domain)
    }

    /// Forms the parallel difference `self - rhs`.
    ///
    /// This is the same common-denominator construction as `add`, but with the
    /// second numerator contribution subtracted instead of added.
    pub fn sub(&self, rhs: &Self) -> Result<Self, LtiError> {
        let domain = Domain::composed(self.domain(), rhs.domain())?;
        let lhs_num = poly_mul(self.numerator(), rhs.denominator());
        let rhs_num = poly_mul(rhs.numerator(), self.denominator());
        let numerator = poly_sub_aligned(&lhs_num, &rhs_num);
        let denominator = poly_mul(self.denominator(), rhs.denominator());
        Self::new(numerator, denominator, domain)
    }

    /// Forms the series composition `self * rhs`.
    ///
    /// Since transfer functions compose multiplicatively in series, this is
    /// just polynomial convolution on numerator and denominator.
    pub fn mul(&self, rhs: &Self) -> Result<Self, LtiError> {
        let domain = Domain::composed(self.domain(), rhs.domain())?;
        let numerator = poly_mul(self.numerator(), rhs.numerator());
        let denominator = poly_mul(self.denominator(), rhs.denominator());
        Self::new(numerator, denominator, domain)
    }

    /// Returns the multiplicative inverse `1 / self`.
    ///
    /// This swaps numerator and denominator and therefore rejects the
    /// identically zero transfer map.
    pub fn inv(&self) -> Result<Self, LtiError> {
        if is_zero_polynomial(self.numerator()) {
            return Err(LtiError::ZeroTransferInverse);
        }
        Self::new(
            self.denominator().to_vec(),
            self.numerator().to_vec(),
            self.domain().clone(),
        )
    }

    /// Forms the quotient `self / rhs`.
    ///
    /// This is implemented as multiplication by `rhs.inv()`, so it shares the
    /// same zero-divisor checks and domain-compatibility rules as the primitive
    /// inverse and series-composition paths.
    pub fn div(&self, rhs: &Self) -> Result<Self, LtiError> {
        if is_zero_polynomial(rhs.numerator()) {
            return Err(LtiError::ZeroTransferDivisor);
        }
        let rhs_inv = rhs.inv()?;
        self.mul(&rhs_inv)
    }

    /// Forms the standard negative-feedback closure `self / (1 + self * rhs)`.
    ///
    /// Here `self` is the forward path and `rhs` is the return path. This
    /// matches the common SISO control convention for closing a loop around a
    /// plant and sensor/controller return path.
    pub fn feedback(&self, rhs: &Self) -> Result<Self, LtiError> {
        self.feedback_with_sign(rhs, false)
    }

    /// Forms the positive-feedback closure `self / (1 - self * rhs)`.
    pub fn positive_feedback(&self, rhs: &Self) -> Result<Self, LtiError> {
        self.feedback_with_sign(rhs, true)
    }

    /// Forms the standard unity negative-feedback closure `self / (1 + self)`.
    pub fn unity_feedback(&self) -> Result<Self, LtiError> {
        let one = unit_transfer(self.domain().clone())?;
        self.feedback(&one)
    }

    /// Forms the unity positive-feedback closure `self / (1 - self)`.
    pub fn positive_unity_feedback(&self) -> Result<Self, LtiError> {
        let one = unit_transfer(self.domain().clone())?;
        self.positive_feedback(&one)
    }

    /// Shared implementation for the two feedback signs.
    ///
    /// This deliberately uses the direct closed-loop polynomial formula instead
    /// of composing `mul`, `add`/`sub`, and `div` naively. Algebraically the
    /// two are equivalent, but the direct form avoids introducing an
    /// avoidable extra factor of `D_g D_h` in the numerator and denominator
    /// before normalization.
    fn feedback_with_sign(&self, rhs: &Self, positive: bool) -> Result<Self, LtiError> {
        let domain = Domain::composed(self.domain(), rhs.domain())?;
        let numerator = poly_mul(self.numerator(), rhs.denominator());
        let direct_denominator = poly_mul(self.denominator(), rhs.denominator());
        let loop_numerator = poly_mul(self.numerator(), rhs.numerator());
        let denominator = if positive {
            poly_sub_aligned(&direct_denominator, &loop_numerator)
        } else {
            poly_add_aligned(&direct_denominator, &loop_numerator)
        };
        Self::new(numerator, denominator, domain)
    }
}

impl<R> ContinuousTransferFunction<R>
where
    R: Float + RealField,
{
    /// Creates a continuous-time transfer function.
    pub fn continuous(
        numerator: impl Into<Vec<R>>,
        denominator: impl Into<Vec<R>>,
    ) -> Result<Self, LtiError> {
        Self::new(numerator, denominator, ContinuousTime)
    }

    /// Returns the steady-state gain `G(0)`.
    ///
    /// For continuous-time transfer functions this evaluates the rational map
    /// at `s = 0`. If the transfer function has a pole at the origin, the
    /// resulting non-finite value is reported as an error instead of being
    /// passed through to the caller.
    pub fn dc_gain(&self) -> Result<Complex<R>, LtiError> {
        let gain = self.evaluate(Complex::new(R::zero(), R::zero()));
        if gain.re.is_finite() && gain.im.is_finite() {
            Ok(gain)
        } else {
            Err(LtiError::NonFiniteResult { which: "dc_gain" })
        }
    }

    /// Realizes the proper transfer function as a dense continuous-time
    /// state-space model in controllable companion form.
    ///
    /// This is a deterministic reference realization, not a minimality pass.
    /// If the input transfer function has pole-zero cancellations, the
    /// resulting state-space model still represents the same transfer map, but
    /// it may contain canceling internal modes.
    pub fn to_state_space(&self) -> Result<ContinuousStateSpace<R>, LtiError> {
        let (a, b, c, d) = companion_realization(self.numerator(), self.denominator())?;
        Ok(ContinuousStateSpace::new(a, b, c, d)?)
    }

    /// Casts the continuous-time transfer function coefficients to another
    /// real scalar dtype.
    ///
    /// This is a structural cast only. It preserves the same coefficient-form
    /// model and reports [`LtiError::ScalarConversionFailed`] if any entry
    /// cannot be represented in the requested dtype.
    pub fn try_cast<S>(&self) -> Result<ContinuousTransferFunction<S>, LtiError>
    where
        S: Float + RealField + NumCast,
    {
        ContinuousTransferFunction::continuous(
            self.numerator()
                .iter()
                .copied()
                .map(|value| cast_real_scalar(value, "transfer_function.numerator"))
                .collect::<Result<Vec<_>, _>>()?,
            self.denominator()
                .iter()
                .copied()
                .map(|value| cast_real_scalar(value, "transfer_function.denominator"))
                .collect::<Result<Vec<_>, _>>()?,
        )
    }
}

impl<R> DiscreteTransferFunction<R>
where
    R: Float + RealField,
{
    /// Creates a discrete-time transfer function with explicit sample time.
    pub fn discrete(
        numerator: impl Into<Vec<R>>,
        denominator: impl Into<Vec<R>>,
        sample_time: R,
    ) -> Result<Self, LtiError> {
        validate_sample_time(sample_time)?;
        Self::new(numerator, denominator, DiscreteTime::new(sample_time))
    }

    /// Sample interval carried by the discrete-time representation.
    #[must_use]
    pub fn sample_time(&self) -> R {
        self.domain.sample_time()
    }

    /// Creates the exact `samples`-step pure delay `z^-samples`.
    ///
    /// The discrete transfer-function layer stores coefficients in descending
    /// powers of `z`, not `z^-1`, so a pure delay is represented as
    ///
    /// `1 / z^samples`
    ///
    /// with denominator coefficients `[1, 0, ..., 0]`.
    pub fn delay(samples: usize, sample_time: R) -> Result<Self, LtiError> {
        let mut denominator = vec![R::one()];
        denominator.resize(samples + 1, R::zero());
        Self::discrete(vec![R::one()], denominator, sample_time)
    }

    /// Returns the steady-state gain `G(1)`.
    ///
    /// For discrete-time transfer functions the steady-state point lies at
    /// `z = 1`. As in the continuous-time path, poles at the evaluation point
    /// are reported through `NonFiniteResult`.
    pub fn dc_gain(&self) -> Result<Complex<R>, LtiError> {
        let gain = self.evaluate(Complex::new(R::one(), R::zero()));
        if gain.re.is_finite() && gain.im.is_finite() {
            Ok(gain)
        } else {
            Err(LtiError::NonFiniteResult { which: "dc_gain" })
        }
    }

    /// Realizes the proper transfer function as a dense discrete-time
    /// state-space model in controllable companion form.
    ///
    /// The state update matrix is built from the same companion polynomial
    /// coefficients as the continuous-time path; only the carried domain
    /// metadata differs.
    pub fn to_state_space(&self) -> Result<DiscreteStateSpace<R>, LtiError> {
        let (a, b, c, d) = companion_realization(self.numerator(), self.denominator())?;
        Ok(DiscreteStateSpace::new(a, b, c, d, self.sample_time())?)
    }

    /// Casts the discrete-time transfer function coefficients and sample time
    /// to another real scalar dtype.
    ///
    /// This is mainly intended for runtime precision comparisons after a
    /// design has already been computed in a higher-precision dtype.
    pub fn try_cast<S>(&self) -> Result<DiscreteTransferFunction<S>, LtiError>
    where
        S: Float + RealField + NumCast,
    {
        DiscreteTransferFunction::discrete(
            self.numerator()
                .iter()
                .copied()
                .map(|value| cast_real_scalar(value, "transfer_function.numerator"))
                .collect::<Result<Vec<_>, _>>()?,
            self.denominator()
                .iter()
                .copied()
                .map(|value| cast_real_scalar(value, "transfer_function.denominator"))
                .collect::<Result<Vec<_>, _>>()?,
            cast_real_scalar(self.sample_time(), "transfer_function.sample_time")?,
        )
    }
}

impl<R> ContinuousStateSpace<R>
where
    R: Float + RealField,
{
    /// Converts the dense real SISO continuous-time state-space model into
    /// coefficient form.
    ///
    /// The returned transfer function represents the same input/output map as
    /// the current realization. It does not attempt pole-zero cancellation for
    /// nonminimal systems.
    pub fn to_transfer_function(&self) -> Result<ContinuousTransferFunction<R>, LtiError> {
        ensure_siso(self.ninputs(), self.noutputs())?;
        state_space_to_transfer_function(self.a(), self.b(), self.c(), self.d(), ContinuousTime)
    }

    /// Re-realizes the dense real SISO system in controllable companion form.
    ///
    /// This first converts the current realization into coefficient form and
    /// then realizes that transfer function again through the controllable
    /// companion constructor. The result preserves the external transfer map
    /// of the system, but it is not a similarity-transform API and does not
    /// preserve the original internal state coordinates.
    pub fn to_controllable_canonical(&self) -> Result<ContinuousStateSpace<R>, LtiError> {
        self.to_transfer_function()?.to_state_space()
    }

    /// Converts the continuous-time state-space model into zero/pole/gain
    /// form through `TransferFunction`.
    pub fn to_zpk(&self) -> Result<Zpk<R, ContinuousTime>, LtiError> {
        self.to_transfer_function()?.to_zpk()
    }

    /// Converts the continuous-time state-space model into a second-order-
    /// section cascade through `TransferFunction`.
    pub fn to_sos(&self) -> Result<Sos<R, ContinuousTime>, LtiError> {
        self.to_transfer_function()?.to_sos()
    }
}

impl<R> DiscreteStateSpace<R>
where
    R: Float + RealField,
{
    /// Converts the dense real SISO discrete-time state-space model into
    /// coefficient form.
    ///
    /// As in the continuous-time path, this recovers the transfer behavior of
    /// the current realization and does not attempt to cancel common factors
    /// numerically.
    pub fn to_transfer_function(&self) -> Result<DiscreteTransferFunction<R>, LtiError> {
        ensure_siso(self.ninputs(), self.noutputs())?;
        state_space_to_transfer_function(
            self.a(),
            self.b(),
            self.c(),
            self.d(),
            DiscreteTime::new(self.sample_time()),
        )
    }

    /// Re-realizes the dense real SISO system in controllable companion form.
    ///
    /// As in the continuous-time path, this uses the transfer-function
    /// roundtrip rather than an explicit similarity transform. The returned
    /// system preserves the discrete transfer map and carries the same sample
    /// time, but it may not retain the original realization order or internal
    /// state coordinates.
    pub fn to_controllable_canonical(&self) -> Result<DiscreteStateSpace<R>, LtiError> {
        self.to_transfer_function()?.to_state_space()
    }

    /// Converts the discrete-time state-space model into zero/pole/gain form
    /// through `TransferFunction`.
    pub fn to_zpk(&self) -> Result<Zpk<R, DiscreteTime<R>>, LtiError> {
        self.to_transfer_function()?.to_zpk()
    }

    /// Converts the discrete-time state-space model into a second-order-
    /// section cascade through `TransferFunction`.
    pub fn to_sos(&self) -> Result<Sos<R, DiscreteTime<R>>, LtiError> {
        self.to_transfer_function()?.to_sos()
    }
}

/// Rejects state-space conversions that are only defined for SISO models.
fn ensure_siso(ninputs: usize, noutputs: usize) -> Result<(), LtiError> {
    if ninputs == 1 && noutputs == 1 {
        Ok(())
    } else {
        Err(LtiError::NonSisoStateSpace { ninputs, noutputs })
    }
}

/// Builds a controllable companion realization of a proper SISO transfer
/// function.
///
/// The input coefficients are first normalized so the denominator is monic.
/// Equal-degree numerator terms are split into:
///
/// - a direct feedthrough term `D`
/// - a strictly proper remainder realized in companion form
///
/// This keeps the realization algebra simple and makes the proper/improper
/// boundary explicit.
fn companion_realization<R>(
    numerator: &[R],
    denominator: &[R],
) -> Result<(Mat<R>, Mat<R>, Mat<R>, Mat<R>), LtiError>
where
    R: Float + RealField,
{
    let numerator = trim_leading_zeros(numerator);
    let denominator = trim_leading_zeros(denominator);
    if denominator.is_empty() {
        return Err(LtiError::EmptyPolynomial {
            which: "denominator",
        });
    }
    if denominator[0] == R::zero() {
        return Err(LtiError::ZeroLeadingCoefficient {
            which: "denominator",
        });
    }

    let n = denominator.len() - 1;
    let m = numerator.len() - 1;
    if m > n {
        return Err(LtiError::ImproperTransferFunction {
            numerator_degree: m,
            denominator_degree: n,
        });
    }

    if n == 0 {
        let d = numerator[0] / denominator[0];
        return Ok((
            Mat::zeros(0, 0),
            Mat::zeros(0, 1),
            Mat::zeros(1, 0),
            Mat::from_fn(1, 1, |_, _| d),
        ));
    }

    let (numerator, denominator) = normalize_ratio(&numerator, &denominator)?;
    let mut padded_numerator = vec![R::zero(); n + 1];
    let offset = n + 1 - numerator.len();
    for (idx, &coef) in numerator.iter().enumerate() {
        padded_numerator[offset + idx] = coef;
    }
    // When the transfer function is proper but not strictly proper, the
    // highest-degree numerator coefficient becomes the direct feedthrough.
    let direct = padded_numerator[0];

    let mut a = Mat::<R>::zeros(n, n);
    for row in 0..(n - 1) {
        a[(row, row + 1)] = R::one();
    }
    for col in 0..n {
        a[(n - 1, col)] = -denominator[n - col];
    }

    let b = Mat::from_fn(
        n,
        1,
        |row, _| if row + 1 == n { R::one() } else { R::zero() },
    );
    let c = Mat::from_fn(1, n, |_, col| {
        let idx = n - col;
        padded_numerator[idx] - denominator[idx] * direct
    });
    let d = Mat::from_fn(1, 1, |_, _| direct);
    Ok((a, b, c, d))
}

/// Converts a dense real SISO state-space realization into coefficient form.
///
/// The denominator comes from the characteristic polynomial of `A`. The
/// numerator is then reconstructed by evaluating the represented transfer map
/// at enough real interpolation points to solve for the unknown coefficients.
///
/// This is a dense reference algorithm. It is appropriate for the current SISO
/// conversion layer, even though it is not the only possible realization-to-TF
/// route.
fn state_space_to_transfer_function<R, Domain>(
    a: MatRef<'_, R>,
    b: MatRef<'_, R>,
    c: MatRef<'_, R>,
    d: MatRef<'_, R>,
    domain: Domain,
) -> Result<TransferFunction<R, Domain>, LtiError>
where
    R: Float + RealField,
    Domain: Clone,
{
    if a.nrows() == 0 {
        return TransferFunction::new(vec![d[(0, 0)]], vec![R::one()], domain);
    }

    let poles = dense_eigenvalues(a)?
        .try_as_col_major()
        .unwrap()
        .as_slice()
        .to_vec();
    let denominator = real_poly_from_roots(&poles, "state_space_poles")?;
    let numerator = interpolate_numerator(a, b, c, d, &denominator)?;
    TransferFunction::new(numerator, denominator, domain)
}

/// Reconstructs the numerator coefficients once the denominator is known.
///
/// At each interpolation point `x`, we evaluate the represented transfer
/// value `G(x)` and multiply by the known denominator polynomial to get the
/// numerator polynomial value. Solving the resulting Vandermonde system
/// recovers the numerator coefficients in descending-power order.
fn interpolate_numerator<R>(
    a: MatRef<'_, R>,
    b: MatRef<'_, R>,
    c: MatRef<'_, R>,
    d: MatRef<'_, R>,
    denominator: &[R],
) -> Result<Vec<R>, LtiError>
where
    R: Float + RealField,
{
    let degree = denominator.len() - 1;
    let points = interpolation_points(denominator, degree + 1);
    let vandermonde = Mat::from_fn(points.len(), degree + 1, |row, col| {
        let power = degree - col;
        points[row].powi(power as i32)
    });
    let rhs_values = points
        .iter()
        .map(|&sample| {
            let point = Complex::new(sample, R::zero());
            let gain = dense_transfer_siso(a, b, c, d, point)?;
            let imag_tol =
                (gain.re.abs() + gain.im.abs() + R::one()) * from_f64::<R>(128.0) * eps::<R>();
            if gain.im.abs() > imag_tol {
                return Err(LtiError::NonFiniteResult {
                    which: "state_space_to_transfer_function.imaginary_interpolation_value",
                });
            }
            Ok(gain.re * poly_eval(denominator, point).re)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let rhs = Mat::from_fn(points.len(), 1, |row, _| rhs_values[row]);
    let solution = vandermonde.full_piv_lu().solve(rhs.as_ref());
    if !solution.as_ref().is_all_finite() {
        return Err(LtiError::NonFiniteResult {
            which: "state_space_to_transfer_function.solve",
        });
    }
    let coeffs = (0..solution.nrows())
        .map(|row| solution[(row, 0)])
        .collect::<Vec<_>>();
    Ok(trim_small_leading_coeffs(&coeffs))
}

/// Returns the identity transfer function `1`.
fn unit_transfer<R, Domain>(domain: Domain) -> Result<TransferFunction<R, Domain>, LtiError>
where
    R: Float + RealField,
    Domain: Clone,
{
    // Using the ordinary constructor keeps the same normalization and domain
    // bookkeeping path as user-provided transfer functions.
    TransferFunction::new(vec![R::one()], vec![R::one()], domain)
}

/// Chooses real interpolation points away from denominator roots.
///
/// The points are simple deterministic samples around the origin. Any sample
/// where the denominator is numerically too small is skipped so the numerator
/// reconstruction does not divide by a nearly singular transfer evaluation.
fn interpolation_points<R>(denominator: &[R], count: usize) -> Vec<R>
where
    R: Float + RealField,
{
    let mut points = Vec::with_capacity(count);
    let mut k = 0usize;
    while points.len() < count {
        let candidate = match k {
            0 => R::zero(),
            _ if k % 2 == 1 => R::from(k.div_ceil(2)).unwrap_or_else(R::one),
            _ => -R::from(k / 2).unwrap_or_else(R::one),
        };
        let value = poly_eval(denominator, Complex::new(candidate, R::zero())).abs();
        let threshold = from_f64::<R>(256.0) * eps::<R>();
        if value > threshold {
            points.push(candidate);
        }
        k += 1;
    }
    points
}

/// Evaluates the dense real SISO state-space transfer map at one complex
/// point.
///
/// This helper is used only inside the interpolation-based
/// `StateSpace -> TransferFunction` path, so it stays local instead of
/// exposing another public transfer API surface.
fn dense_transfer_siso<R>(
    a: MatRef<'_, R>,
    b: MatRef<'_, R>,
    c: MatRef<'_, R>,
    d: MatRef<'_, R>,
    point: Complex<R>,
) -> Result<Complex<R>, LtiError>
where
    R: Float + RealField,
{
    let a = Mat::from_fn(a.nrows(), a.ncols(), |row, col| {
        Complex::new(a[(row, col)], R::zero())
    });
    let b = Mat::from_fn(b.nrows(), b.ncols(), |row, col| {
        Complex::new(b[(row, col)], R::zero())
    });
    let c = Mat::from_fn(c.nrows(), c.ncols(), |row, col| {
        Complex::new(c[(row, col)], R::zero())
    });
    let d = Complex::new(d[(0, 0)], R::zero());

    let lhs = Mat::from_fn(a.nrows(), a.ncols(), |row, col| {
        if row == col {
            point - a[(row, col)]
        } else {
            -a[(row, col)]
        }
    });
    let sol = lhs.full_piv_lu().solve(b.as_ref());
    let mut value = d;
    for k in 0..a.nrows() {
        value += c[(0, k)] * sol[(k, 0)];
    }
    if value.re.is_finite() && value.im.is_finite() {
        Ok(value)
    } else {
        Err(LtiError::NonFiniteResult {
            which: "state_space_to_transfer_function.transfer_at",
        })
    }
}

/// Trims numerically insignificant leading coefficients after interpolation.
///
/// The Vandermonde solve can leave a tiny leading coefficient where the exact
/// transfer function has lower degree, especially for strictly proper systems.
/// Removing those near-zero leading terms restores the expected polynomial
/// degree before the result is normalized into a `TransferFunction`.
fn trim_small_leading_coeffs<R>(coeffs: &[R]) -> Vec<R>
where
    R: Float + RealField,
{
    let scale = coeffs
        .iter()
        .fold(R::one(), |acc, &value| acc.max(value.abs()));
    let tol = scale * from_f64::<R>(128.0) * eps::<R>();
    let first_nz = coeffs.iter().position(|&value| value.abs() > tol);
    match first_nz {
        Some(idx) => coeffs[idx..].to_vec(),
        None => vec![R::zero()],
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ContinuousStateSpace, ContinuousTransferFunction, DiscreteStateSpace,
        DiscreteTransferFunction,
    };
    use crate::control::lti::{DiscreteSos, DiscreteZpk, LtiError, Sos};
    use faer::Mat;
    use faer::complex::Complex;
    use nalgebra::ComplexField;

    fn assert_coeffs_close(lhs: &[f64], rhs: &[f64], tol: f64) {
        assert_eq!(lhs.len(), rhs.len());
        for (idx, (&lhs, &rhs)) in lhs.iter().zip(rhs.iter()).enumerate() {
            let err = (lhs - rhs).abs();
            assert!(
                err <= tol,
                "coefficient {idx} differs: lhs={lhs}, rhs={rhs}, err={err}, tol={tol}",
            );
        }
    }

    fn assert_mat_close(lhs: &Mat<f64>, rhs: &Mat<f64>, tol: f64) {
        assert_eq!(lhs.nrows(), rhs.nrows());
        assert_eq!(lhs.ncols(), rhs.ncols());
        for row in 0..lhs.nrows() {
            for col in 0..lhs.ncols() {
                let err = (lhs[(row, col)] - rhs[(row, col)]).abs();
                assert!(
                    err <= tol,
                    "entry ({row}, {col}) differs: lhs={}, rhs={}, err={err}, tol={tol}",
                    lhs[(row, col)],
                    rhs[(row, col)]
                );
            }
        }
    }

    #[test]
    fn constructor_normalizes_denominator() {
        let tf = ContinuousTransferFunction::continuous(vec![2.0, 4.0], vec![2.0, 6.0]).unwrap();
        assert_eq!(tf.numerator(), &[1.0, 2.0]);
        assert_eq!(tf.denominator(), &[1.0, 3.0]);
    }

    #[test]
    fn constructor_rejects_nonfinite_coefficients_anywhere_in_ratio() {
        let trailing_nan =
            ContinuousTransferFunction::continuous(vec![1.0, f64::NAN], vec![1.0, 1.0])
                .unwrap_err();
        let trailing_inf =
            ContinuousTransferFunction::continuous(vec![1.0], vec![1.0, f64::INFINITY])
                .unwrap_err();

        assert!(matches!(
            trailing_nan,
            LtiError::NonFiniteResult {
                which: "normalize_ratio"
            }
        ));
        assert!(matches!(
            trailing_inf,
            LtiError::NonFiniteResult {
                which: "normalize_ratio"
            }
        ));
    }

    #[test]
    fn zpk_round_trip_preserves_coefficients() {
        let tf = ContinuousTransferFunction::continuous(vec![1.0, 3.0, 2.0], vec![1.0, 5.0, 6.0])
            .unwrap();
        let back = tf.to_zpk().unwrap().to_transfer_function().unwrap();
        assert_coeffs_close(back.numerator(), tf.numerator(), 1.0e-12);
        assert_coeffs_close(back.denominator(), tf.denominator(), 1.0e-12);
    }

    #[test]
    fn sos_round_trip_preserves_coefficients() {
        let tf = DiscreteTransferFunction::discrete(
            vec![1.0, 0.0, 5.0, 0.0, 4.0],
            vec![1.0, 0.0, 6.0, 0.0, 9.0],
            0.1,
        )
        .unwrap();
        let sos = tf.to_sos().unwrap();
        let back = sos.to_transfer_function().unwrap();
        assert_coeffs_close(back.numerator(), tf.numerator(), 1.0e-12);
        assert_coeffs_close(back.denominator(), tf.denominator(), 1.0e-12);
        assert_eq!(back.sample_time(), 0.1);
    }

    #[test]
    fn evaluate_matches_closed_form() {
        let tf = ContinuousTransferFunction::continuous(vec![2.0], vec![1.0, 3.0]).unwrap();
        let point = Complex::new(1.0, 2.0);
        let expected = Complex::new(2.0, 0.0) / (point + Complex::new(3.0, 0.0));
        assert!((tf.evaluate(point) - expected).abs() <= 1.0e-12);
    }

    #[test]
    fn discrete_constructor_validates_sample_time() {
        let err = DiscreteTransferFunction::discrete(vec![1.0], vec![1.0, -0.5], 0.0).unwrap_err();
        assert!(matches!(
            err,
            crate::control::lti::LtiError::InvalidSampleTime
        ));
    }

    #[test]
    fn discrete_delay_constructors_match_exact_z_inverse_power() {
        let tf = DiscreteTransferFunction::delay(3, 0.1).unwrap();
        let zpk = DiscreteZpk::delay(3, 0.1).unwrap();
        let sos = DiscreteSos::delay(3, 0.1).unwrap();
        let point = Complex::new(0.8, 0.2);
        let expected = Complex::new(1.0, 0.0) / (point * point * point);

        assert_eq!(tf.numerator(), &[1.0]);
        assert_eq!(tf.denominator(), &[1.0, 0.0, 0.0, 0.0]);
        assert_eq!(tf.sample_time(), 0.1);
        assert_eq!(zpk.sample_time(), 0.1);
        assert_eq!(sos.sample_time(), 0.1);
        assert!((tf.evaluate(point) - expected).abs() <= 1.0e-12);
        assert!((zpk.evaluate(point) - expected).abs() <= 1.0e-12);
        assert!((sos.evaluate(point) - expected).abs() <= 1.0e-12);
        assert_coeffs_close(
            zpk.to_transfer_function().unwrap().denominator(),
            tf.denominator(),
            1.0e-12,
        );
        assert_coeffs_close(
            sos.to_transfer_function().unwrap().denominator(),
            tf.denominator(),
            1.0e-12,
        );
    }

    #[test]
    fn zero_sample_delay_is_identity_across_representations() {
        let tf = DiscreteTransferFunction::delay(0, 0.2).unwrap();
        let zpk = DiscreteZpk::delay(0, 0.2).unwrap();
        let sos = DiscreteSos::delay(0, 0.2).unwrap();
        let point = Complex::new(0.3, -0.4);

        assert_eq!(tf.numerator(), &[1.0]);
        assert_eq!(tf.denominator(), &[1.0]);
        assert!((tf.evaluate(point) - Complex::new(1.0, 0.0)).abs() <= 1.0e-12);
        assert!((zpk.evaluate(point) - Complex::new(1.0, 0.0)).abs() <= 1.0e-12);
        assert!((sos.evaluate(point) - Complex::new(1.0, 0.0)).abs() <= 1.0e-12);
    }

    #[test]
    fn sos_identity_section_round_trip() {
        let sos = Sos::continuous(
            vec![
                crate::control::lti::SecondOrderSection::new([1.0, 0.0, 0.0], [1.0, -0.5, 0.0])
                    .unwrap(),
            ],
            2.0,
        )
        .unwrap();
        let tf = sos.to_transfer_function().unwrap();
        assert_eq!(tf.numerator(), &[2.0, 0.0, 0.0]);
        assert_eq!(tf.denominator(), &[1.0, -0.5, 0.0]);
    }

    #[test]
    fn continuous_transfer_function_realizes_and_round_trips_through_state_space() {
        let tf =
            ContinuousTransferFunction::continuous(vec![2.0, 5.0], vec![1.0, 3.0, 2.0]).unwrap();
        let ss = tf.to_state_space().unwrap();
        let back = ss.to_transfer_function().unwrap();
        assert_coeffs_close(back.numerator(), tf.numerator(), 1.0e-10);
        assert_coeffs_close(back.denominator(), tf.denominator(), 1.0e-10);
    }

    #[test]
    fn discrete_transfer_function_realizes_and_preserves_sample_time() {
        let tf = DiscreteTransferFunction::discrete(vec![1.0, -0.25], vec![1.0, -0.5, 0.125], 0.2)
            .unwrap();
        let ss = tf.to_state_space().unwrap();
        let back = ss.to_transfer_function().unwrap();
        assert_coeffs_close(back.numerator(), tf.numerator(), 1.0e-10);
        assert_coeffs_close(back.denominator(), tf.denominator(), 1.0e-10);
        assert_eq!(back.sample_time(), 0.2);
    }

    #[test]
    fn lti_representations_try_cast_to_f32_preserve_sample_time_and_response() {
        let tf = DiscreteTransferFunction::discrete(
            vec![1.0, -0.25, 0.0625],
            vec![1.0, -1.2, 0.45],
            0.2,
        )
        .unwrap();
        let zpk = tf.to_zpk().unwrap();
        let sos = tf.to_sos().unwrap();
        let point64 = Complex::new(0.6f64, 0.2);
        let point32 = Complex::new(point64.re as f32, point64.im as f32);

        let tf32 = tf.try_cast::<f32>().unwrap();
        let zpk32 = zpk.try_cast::<f32>().unwrap();
        let sos32 = sos.try_cast::<f32>().unwrap();

        assert!((tf32.sample_time() - 0.2f32).abs() <= 1.0e-6);
        assert!((zpk32.sample_time() - 0.2f32).abs() <= 1.0e-6);
        assert!((sos32.sample_time() - 0.2f32).abs() <= 1.0e-6);

        let tf_eval = tf.evaluate(point64);
        let tf32_eval = tf32.evaluate(point32);
        let zpk32_eval = zpk32.evaluate(point32);
        let sos32_eval = sos32.evaluate(point32);

        assert!((f64::from(tf32_eval.re) - tf_eval.re).abs() <= 5.0e-5);
        assert!((f64::from(tf32_eval.im) - tf_eval.im).abs() <= 5.0e-5);
        assert!((f64::from(zpk32_eval.re) - tf_eval.re).abs() <= 5.0e-5);
        assert!((f64::from(zpk32_eval.im) - tf_eval.im).abs() <= 5.0e-5);
        assert!((f64::from(sos32_eval.re) - tf_eval.re).abs() <= 5.0e-5);
        assert!((f64::from(sos32_eval.im) - tf_eval.im).abs() <= 5.0e-5);
    }

    #[test]
    fn dc_gain_helpers_match_across_siso_representations() {
        let cont =
            ContinuousTransferFunction::continuous(vec![2.0, 1.0], vec![1.0, 4.0, 3.0]).unwrap();
        let disc = DiscreteTransferFunction::discrete(vec![1.0, 0.5], vec![1.0, -0.25, 0.125], 0.1)
            .unwrap();

        let cont_gain = cont.dc_gain().unwrap();
        let cont_zpk_gain = cont.to_zpk().unwrap().dc_gain().unwrap();
        let cont_sos_gain = cont.to_sos().unwrap().dc_gain().unwrap();
        let disc_gain = disc.dc_gain().unwrap();
        let disc_zpk_gain = disc.to_zpk().unwrap().dc_gain().unwrap();
        let disc_sos_gain = disc.to_sos().unwrap().dc_gain().unwrap();

        assert!((cont_gain - cont_zpk_gain).abs() <= 1.0e-12);
        assert!((cont_gain - cont_sos_gain).abs() <= 1.0e-12);
        assert!((disc_gain - disc_zpk_gain).abs() <= 1.0e-12);
        assert!((disc_gain - disc_sos_gain).abs() <= 1.0e-12);
    }

    #[test]
    fn dc_gain_rejects_poles_at_steady_state_point() {
        let cont = ContinuousTransferFunction::continuous(vec![1.0], vec![1.0, 0.0]).unwrap();
        let disc = DiscreteTransferFunction::discrete(vec![1.0], vec![1.0, -1.0], 0.1).unwrap();

        assert!(matches!(
            cont.dc_gain().unwrap_err(),
            LtiError::NonFiniteResult { which: "dc_gain" }
        ));
        assert!(matches!(
            cont.to_zpk().unwrap().dc_gain().unwrap_err(),
            LtiError::NonFiniteResult { which: "dc_gain" }
        ));
        assert!(matches!(
            cont.to_sos().unwrap().dc_gain().unwrap_err(),
            LtiError::NonFiniteResult { which: "dc_gain" }
        ));
        assert!(matches!(
            disc.dc_gain().unwrap_err(),
            LtiError::NonFiniteResult { which: "dc_gain" }
        ));
        assert!(matches!(
            disc.to_zpk().unwrap().dc_gain().unwrap_err(),
            LtiError::NonFiniteResult { which: "dc_gain" }
        ));
        assert!(matches!(
            disc.to_sos().unwrap().dc_gain().unwrap_err(),
            LtiError::NonFiniteResult { which: "dc_gain" }
        ));
    }

    #[test]
    fn continuous_state_space_re_realizes_in_controllable_canonical_form() {
        let ss = ContinuousStateSpace::new(
            Mat::from_fn(2, 2, |row, col| match (row, col) {
                (0, 0) => -4.0,
                (0, 1) => 3.0,
                (1, 0) => -1.0,
                _ => 0.0,
            }),
            Mat::from_fn(2, 1, |row, _| if row == 0 { 2.0 } else { 1.0 }),
            Mat::from_fn(1, 2, |_, col| if col == 0 { 1.5 } else { -0.75 }),
            Mat::from_fn(1, 1, |_, _| 0.25),
        )
        .unwrap();

        let canonical = ss.to_controllable_canonical().unwrap();
        let expected = ss.to_transfer_function().unwrap().to_state_space().unwrap();
        let back = canonical.to_transfer_function().unwrap();
        let original = ss.to_transfer_function().unwrap();

        assert_mat_close(&canonical.a().to_owned(), &expected.a().to_owned(), 1.0e-12);
        assert_mat_close(&canonical.b().to_owned(), &expected.b().to_owned(), 1.0e-12);
        assert_mat_close(&canonical.c().to_owned(), &expected.c().to_owned(), 1.0e-12);
        assert_mat_close(&canonical.d().to_owned(), &expected.d().to_owned(), 1.0e-12);
        assert_coeffs_close(back.numerator(), original.numerator(), 1.0e-10);
        assert_coeffs_close(back.denominator(), original.denominator(), 1.0e-10);
    }

    #[test]
    fn discrete_state_space_re_realizes_in_controllable_canonical_form() {
        let ss = DiscreteStateSpace::new(
            Mat::from_fn(2, 2, |row, col| match (row, col) {
                (0, 0) => 0.4,
                (0, 1) => -0.2,
                (1, 0) => 1.0,
                _ => 0.3,
            }),
            Mat::from_fn(2, 1, |row, _| if row == 0 { 0.5 } else { 1.0 }),
            Mat::from_fn(1, 2, |_, col| if col == 0 { 1.0 } else { 0.2 }),
            Mat::from_fn(1, 1, |_, _| -0.1),
            0.05,
        )
        .unwrap();

        let canonical = ss.to_controllable_canonical().unwrap();
        let expected = ss.to_transfer_function().unwrap().to_state_space().unwrap();
        let back = canonical.to_transfer_function().unwrap();
        let original = ss.to_transfer_function().unwrap();

        assert_mat_close(&canonical.a().to_owned(), &expected.a().to_owned(), 1.0e-12);
        assert_mat_close(&canonical.b().to_owned(), &expected.b().to_owned(), 1.0e-12);
        assert_mat_close(&canonical.c().to_owned(), &expected.c().to_owned(), 1.0e-12);
        assert_mat_close(&canonical.d().to_owned(), &expected.d().to_owned(), 1.0e-12);
        assert_coeffs_close(back.numerator(), original.numerator(), 1.0e-10);
        assert_coeffs_close(back.denominator(), original.denominator(), 1.0e-10);
        assert_eq!(canonical.sample_time(), ss.sample_time());
    }

    #[test]
    fn controllable_canonical_rejects_mimo_state_space() {
        let ss = ContinuousStateSpace::new(
            Mat::from_fn(2, 2, |row, col| if row == col { -1.0 } else { 0.0 }),
            Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 }),
            Mat::from_fn(1, 2, |_, col| if col == 0 { 1.0 } else { 0.0 }),
            Mat::from_fn(1, 2, |_, _| 0.0),
        )
        .unwrap();

        let err = ss.to_controllable_canonical().unwrap_err();
        assert!(matches!(
            err,
            LtiError::NonSisoStateSpace {
                ninputs: 2,
                noutputs: 1
            }
        ));
    }

    #[test]
    fn improper_transfer_function_rejects_state_space_realization() {
        let tf = ContinuousTransferFunction::continuous(vec![1.0, 2.0], vec![1.0]).unwrap();
        let err = tf.to_state_space().unwrap_err();
        assert!(matches!(
            err,
            LtiError::ImproperTransferFunction {
                numerator_degree: 1,
                denominator_degree: 0
            }
        ));
    }

    #[test]
    fn state_space_to_zpk_and_sos_chain_through_transfer_function() {
        let tf =
            ContinuousTransferFunction::continuous(vec![1.0, 3.0], vec![1.0, 5.0, 6.0]).unwrap();
        let ss = tf.to_state_space().unwrap();
        let zpk = ss.to_zpk().unwrap();
        let sos = ss.to_sos().unwrap();
        let tf_from_zpk = zpk.to_transfer_function().unwrap();
        let tf_from_sos = sos.to_transfer_function().unwrap();
        assert_coeffs_close(tf_from_zpk.numerator(), tf.numerator(), 1.0e-10);
        assert_coeffs_close(tf_from_zpk.denominator(), tf.denominator(), 1.0e-10);
        assert_coeffs_close(tf_from_sos.numerator(), tf.numerator(), 1.0e-10);
        assert_coeffs_close(tf_from_sos.denominator(), tf.denominator(), 1.0e-10);
    }

    #[test]
    fn transfer_function_add_sub_mul_div_follow_rational_arithmetic() {
        let lhs = ContinuousTransferFunction::continuous(vec![1.0, 2.0], vec![1.0, 3.0]).unwrap();
        let rhs = ContinuousTransferFunction::continuous(vec![2.0], vec![1.0, 4.0]).unwrap();

        let sum = lhs.add(&rhs).unwrap();
        let diff = lhs.sub(&rhs).unwrap();
        let prod = lhs.mul(&rhs).unwrap();
        let quot = lhs.div(&rhs).unwrap();

        assert_coeffs_close(sum.numerator(), &[1.0, 8.0, 14.0], 1.0e-12);
        assert_coeffs_close(sum.denominator(), &[1.0, 7.0, 12.0], 1.0e-12);
        assert_coeffs_close(diff.numerator(), &[1.0, 4.0, 2.0], 1.0e-12);
        assert_coeffs_close(diff.denominator(), &[1.0, 7.0, 12.0], 1.0e-12);
        assert_coeffs_close(prod.numerator(), &[2.0, 4.0], 1.0e-12);
        assert_coeffs_close(prod.denominator(), &[1.0, 7.0, 12.0], 1.0e-12);
        assert_coeffs_close(quot.numerator(), &[0.5, 3.0, 4.0], 1.0e-12);
        assert_coeffs_close(quot.denominator(), &[1.0, 3.0], 1.0e-12);
    }

    #[test]
    fn transfer_function_feedback_matches_closed_form() {
        let plant = ContinuousTransferFunction::continuous(vec![2.0], vec![1.0, 3.0]).unwrap();
        let sensor = ContinuousTransferFunction::continuous(vec![1.0], vec![1.0, 1.0]).unwrap();

        let closed_loop = plant.feedback(&sensor).unwrap();
        let positive = plant.positive_feedback(&sensor).unwrap();
        let unity = plant.unity_feedback().unwrap();

        assert_coeffs_close(closed_loop.numerator(), &[2.0, 2.0], 1.0e-12);
        assert_coeffs_close(closed_loop.denominator(), &[1.0, 4.0, 5.0], 1.0e-12);
        assert_coeffs_close(positive.numerator(), &[2.0, 2.0], 1.0e-12);
        assert_coeffs_close(positive.denominator(), &[1.0, 4.0, 1.0], 1.0e-12);
        assert_coeffs_close(unity.numerator(), &[2.0], 1.0e-12);
        assert_coeffs_close(unity.denominator(), &[1.0, 5.0], 1.0e-12);
    }

    #[test]
    fn transfer_function_division_rejects_zero_divisor() {
        let lhs = ContinuousTransferFunction::continuous(vec![1.0], vec![1.0, 1.0]).unwrap();
        let rhs = ContinuousTransferFunction::continuous(vec![0.0], vec![1.0]).unwrap();
        let err = lhs.div(&rhs).unwrap_err();
        assert!(matches!(err, LtiError::ZeroTransferDivisor));
    }

    #[test]
    fn discrete_arithmetic_rejects_sample_time_mismatch() {
        let lhs = DiscreteTransferFunction::discrete(vec![1.0], vec![1.0, -0.5], 0.1).unwrap();
        let rhs = DiscreteTransferFunction::discrete(vec![1.0], vec![1.0, -0.25], 0.2).unwrap();
        let err = lhs.add(&rhs).unwrap_err();
        assert!(matches!(err, LtiError::MismatchedSampleTime));
    }

    #[test]
    fn zpk_and_sos_arithmetic_chain_through_transfer_function() {
        let lhs = ContinuousTransferFunction::continuous(vec![1.0, 2.0], vec![1.0, 3.0]).unwrap();
        let rhs = ContinuousTransferFunction::continuous(vec![2.0], vec![1.0, 4.0]).unwrap();

        let zpk_sum = lhs
            .to_zpk()
            .unwrap()
            .add(&rhs.to_zpk().unwrap())
            .unwrap()
            .to_transfer_function()
            .unwrap();
        let sos_feedback = lhs
            .to_sos()
            .unwrap()
            .feedback(&rhs.to_sos().unwrap())
            .unwrap()
            .to_transfer_function()
            .unwrap();

        let tf_sum = lhs.add(&rhs).unwrap();
        let tf_feedback = lhs.feedback(&rhs).unwrap();

        assert_coeffs_close(zpk_sum.numerator(), tf_sum.numerator(), 1.0e-10);
        assert_coeffs_close(zpk_sum.denominator(), tf_sum.denominator(), 1.0e-10);
        assert_coeffs_close(sos_feedback.numerator(), tf_feedback.numerator(), 1.0e-10);
        assert_coeffs_close(
            sos_feedback.denominator(),
            tf_feedback.denominator(),
            1.0e-10,
        );
    }
}

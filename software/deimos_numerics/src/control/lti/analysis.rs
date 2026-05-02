use super::error::LtiError;
use super::state_space::{
    ContinuousStateSpace, DiscreteStateSpace, SparseContinuousStateSpace, SparseDiscreteStateSpace,
    SparseStateSpace, StateSpace,
};
use crate::control::dense_ops::dense_mul_plain as dense_mul;
use crate::decomp::dense_eigenvalues;
use crate::sparse::lu::SparseLu;
use alloc::vec::Vec;
use faer::complex::Complex;
use faer::linalg::lu::partial_pivoting::factor::PartialPivLuParams;
use faer::prelude::Solve;
use faer::sparse::linalg::lu::LuSymbolicParams;
use faer::sparse::{SparseColMat, SparseColMatRef, Triplet};
use faer::{Mat, MatRef, Par, Spec};
use faer_traits::ext::ComplexFieldExt;
use faer_traits::math_utils::eps;
use faer_traits::{ComplexField, RealField};
use num_traits::{Float, One, Zero};

impl<T, Domain> StateSpace<T, Domain>
where
    T: ComplexField + Copy,
    T::Real: Float + RealField,
{
    /// Returns the poles of the dense state-space model.
    ///
    /// Poles are sorted by descending magnitude, then by descending real part,
    /// then by descending imaginary part. That gives deterministic output for
    /// testing and for later comparison across alternate representations.
    pub fn poles(&self) -> Result<Vec<Complex<T::Real>>, LtiError> {
        let mut poles = dense_eigenvalues(self.a())?
            .try_as_col_major()
            .unwrap()
            .as_slice()
            .to_vec();
        poles.sort_by(|lhs, rhs| compare_poles(*lhs, *rhs));
        Ok(poles)
    }

    /// Returns the controllability matrix `[B, AB, ..., A^(n-1) B]`.
    ///
    /// For the first LTI-analysis pass this is a dense reference
    /// implementation. It is appropriate for diagnostics and small/moderate
    /// models, even though it is not the most efficient route for large-scale
    /// systems.
    #[must_use]
    pub fn controllability_matrix(&self) -> Mat<T> {
        let n = self.nstates();
        let m = self.ninputs();
        let mut out = Mat::zeros(n, n * m);
        let mut block = self.b().to_owned();
        for k in 0..n {
            copy_block(out.as_mut(), 0, k * m, block.as_ref());
            if k + 1 != n {
                block = dense_mul(self.a(), block.as_ref());
            }
        }
        out
    }

    /// Returns the observability matrix `[C; CA; ...; C A^(n-1)]`.
    #[must_use]
    pub fn observability_matrix(&self) -> Mat<T> {
        let n = self.nstates();
        let p = self.noutputs();
        let mut out = Mat::zeros(n * p, n);
        let mut block = self.c().to_owned();
        for k in 0..n {
            copy_block(out.as_mut(), k * p, 0, block.as_ref());
            if k + 1 != n {
                block = dense_mul(block.as_ref(), self.a());
            }
        }
        out
    }

    /// Returns the numerical rank of the controllability matrix.
    pub fn controllability_rank(&self) -> Result<usize, LtiError> {
        let ctrb = self.controllability_matrix();
        numerical_rank(ctrb.as_ref())
    }

    /// Returns the numerical rank of the controllability matrix using an
    /// explicit singular-value threshold.
    pub fn controllability_rank_with_tol(&self, tol: T::Real) -> Result<usize, LtiError> {
        let ctrb = self.controllability_matrix();
        numerical_rank_with_tol(ctrb.as_ref(), tol)
    }

    /// Returns the numerical rank of the observability matrix.
    pub fn observability_rank(&self) -> Result<usize, LtiError> {
        let obsv = self.observability_matrix();
        numerical_rank(obsv.as_ref())
    }

    /// Returns the numerical rank of the observability matrix using an
    /// explicit singular-value threshold.
    pub fn observability_rank_with_tol(&self, tol: T::Real) -> Result<usize, LtiError> {
        let obsv = self.observability_matrix();
        numerical_rank_with_tol(obsv.as_ref(), tol)
    }

    /// Returns whether the dense model is numerically controllable.
    pub fn is_controllable(&self) -> Result<bool, LtiError> {
        Ok(self.controllability_rank()? == self.nstates())
    }

    /// Returns whether the dense model is numerically controllable using an
    /// explicit singular-value threshold.
    pub fn is_controllable_with_tol(&self, tol: T::Real) -> Result<bool, LtiError> {
        Ok(self.controllability_rank_with_tol(tol)? == self.nstates())
    }

    /// Returns whether the dense model is numerically observable.
    pub fn is_observable(&self) -> Result<bool, LtiError> {
        Ok(self.observability_rank()? == self.nstates())
    }

    /// Returns whether the dense model is numerically observable using an
    /// explicit singular-value threshold.
    pub fn is_observable_with_tol(&self, tol: T::Real) -> Result<bool, LtiError> {
        Ok(self.observability_rank_with_tol(tol)? == self.nstates())
    }

    /// Returns whether the dense model is numerically minimal.
    ///
    /// Minimality is defined here by the usual dense rank tests:
    /// controllable and observable.
    pub fn is_minimal(&self) -> Result<bool, LtiError> {
        Ok(self.is_controllable()? && self.is_observable()?)
    }

    /// Evaluates the transfer matrix at the supplied complex point.
    ///
    /// The caller supplies the point in the natural transform variable for the
    /// domain:
    ///
    /// - continuous-time: `s`
    /// - discrete-time: `z`
    ///
    /// This is the dense reference path:
    ///
    /// `G(point) = C (point I - A)^(-1) B + D`
    pub fn transfer_at(&self, point: Complex<T::Real>) -> Result<Mat<Complex<T::Real>>, LtiError> {
        let a = to_complex_mat(self.a());
        let b = to_complex_mat(self.b());
        let c = to_complex_mat(self.c());
        let d = to_complex_mat(self.d());

        let n = a.nrows();
        let lhs = Mat::from_fn(n, n, |row, col| {
            if row == col {
                point - a[(row, col)]
            } else {
                -a[(row, col)]
            }
        });
        let sol = lhs.full_piv_lu().solve(b.as_ref());
        let gain = dense_mul(c.as_ref(), sol.as_ref());
        let out = Mat::from_fn(gain.nrows(), gain.ncols(), |row, col| {
            gain[(row, col)] + d[(row, col)]
        });
        if out.as_ref().is_all_finite() {
            Ok(out)
        } else {
            Err(LtiError::NonFiniteResult {
                which: "transfer_at",
            })
        }
    }
}

impl<T, Domain> SparseStateSpace<T, Domain>
where
    T: ComplexField + Copy,
    T::Real: Float + RealField,
{
    /// Evaluates the sparse transfer matrix at the supplied complex point.
    ///
    /// The sparse path computes
    ///
    /// `G(point) = C (point I - A)^(-1) B + D`
    ///
    /// through sparse shifted solves instead of by densifying `A`.
    pub fn transfer_at(&self, point: Complex<T::Real>) -> Result<Mat<Complex<T::Real>>, LtiError> {
        let mut values =
            sparse_transfer_at_points(self.a(), self.b(), self.c(), self.d(), &[point])?;
        Ok(values
            .pop()
            .expect("single-point sparse transfer evaluation"))
    }
}

impl<T> ContinuousStateSpace<T>
where
    T: ComplexField + Copy,
    T::Real: Float + RealField,
{
    /// Returns whether all poles lie strictly in the open left half-plane.
    ///
    /// The default tolerance is `sqrt(eps)`, which is conservative enough to
    /// avoid classifying numerically marginal poles as safely stable.
    pub fn is_asymptotically_stable(&self) -> Result<bool, LtiError> {
        self.is_asymptotically_stable_with_tol(eps::<T::Real>().sqrt())
    }

    /// Returns whether all poles lie to the left of `-tol`.
    pub fn is_asymptotically_stable_with_tol(&self, tol: T::Real) -> Result<bool, LtiError> {
        Ok(self.poles()?.into_iter().all(|pole| pole.re < -tol))
    }

    /// Returns the DC gain `G(0)`.
    pub fn dc_gain(&self) -> Result<Mat<Complex<T::Real>>, LtiError> {
        self.transfer_at(Complex::new(
            <T::Real as Zero>::zero(),
            <T::Real as Zero>::zero(),
        ))
    }
}

impl<T> DiscreteStateSpace<T>
where
    T: ComplexField + Copy,
    T::Real: Float + RealField,
{
    /// Returns whether all poles lie strictly inside the unit disk.
    pub fn is_asymptotically_stable(&self) -> Result<bool, LtiError> {
        self.is_asymptotically_stable_with_tol(eps::<T::Real>().sqrt())
    }

    /// Returns whether all poles satisfy `|pole| < 1 - tol`.
    pub fn is_asymptotically_stable_with_tol(&self, tol: T::Real) -> Result<bool, LtiError> {
        Ok(self
            .poles()?
            .into_iter()
            .all(|pole: Complex<T::Real>| pole.abs() < <T::Real as One>::one() - tol))
    }

    /// Returns the DC gain `G(1)`.
    pub fn dc_gain(&self) -> Result<Mat<Complex<T::Real>>, LtiError> {
        self.transfer_at(Complex::new(
            <T::Real as One>::one(),
            <T::Real as Zero>::zero(),
        ))
    }
}

impl<T> SparseContinuousStateSpace<T>
where
    T: ComplexField + Copy,
    T::Real: Float + RealField,
{
    /// Returns the DC gain `G(0)` for the sparse continuous-time model.
    pub fn dc_gain(&self) -> Result<Mat<Complex<T::Real>>, LtiError> {
        self.transfer_at(Complex::new(
            <T::Real as Zero>::zero(),
            <T::Real as Zero>::zero(),
        ))
    }
}

impl<T> SparseDiscreteStateSpace<T>
where
    T: ComplexField + Copy,
    T::Real: Float + RealField,
{
    /// Returns the DC gain `G(1)` for the sparse discrete-time model.
    pub fn dc_gain(&self) -> Result<Mat<Complex<T::Real>>, LtiError> {
        self.transfer_at(Complex::new(
            <T::Real as One>::one(),
            <T::Real as Zero>::zero(),
        ))
    }
}

fn compare_poles<R: Float>(lhs: Complex<R>, rhs: Complex<R>) -> core::cmp::Ordering {
    let rhs_abs2 = rhs.re * rhs.re + rhs.im * rhs.im;
    let lhs_abs2 = lhs.re * lhs.re + lhs.im * lhs.im;
    rhs_abs2
        .partial_cmp(&lhs_abs2)
        .unwrap_or(core::cmp::Ordering::Equal)
        .then_with(|| {
            rhs.re
                .partial_cmp(&lhs.re)
                .unwrap_or(core::cmp::Ordering::Equal)
        })
        .then_with(|| {
            rhs.im
                .partial_cmp(&lhs.im)
                .unwrap_or(core::cmp::Ordering::Equal)
        })
}

fn copy_block<T: Copy>(
    mut dst: faer::MatMut<'_, T>,
    row_offset: usize,
    col_offset: usize,
    src: MatRef<'_, T>,
) {
    for col in 0..src.ncols() {
        for row in 0..src.nrows() {
            dst[(row_offset + row, col_offset + col)] = src[(row, col)];
        }
    }
}

fn numerical_rank<T>(matrix: MatRef<'_, T>) -> Result<usize, LtiError>
where
    T: ComplexField,
    T::Real: Float + RealField,
{
    let sv = matrix.singular_values()?;
    Ok(rank_from_singular_values(
        &sv,
        matrix.nrows(),
        matrix.ncols(),
        None,
    ))
}

fn numerical_rank_with_tol<T>(matrix: MatRef<'_, T>, tol: T::Real) -> Result<usize, LtiError>
where
    T: ComplexField,
    T::Real: Float + RealField,
{
    let sv = matrix.singular_values()?;
    Ok(rank_from_singular_values(
        &sv,
        matrix.nrows(),
        matrix.ncols(),
        Some(tol),
    ))
}

fn rank_from_singular_values<R: Float + RealField>(
    singular_values: &[R],
    nrows: usize,
    ncols: usize,
    tol: Option<R>,
) -> usize {
    let Some(&sigma_max) = singular_values.first() else {
        return 0;
    };
    let threshold = tol.unwrap_or_else(|| {
        let dim = R::from((nrows.max(ncols)) as f64).unwrap_or_else(R::one);
        sigma_max * dim * eps::<R>()
    });
    singular_values
        .iter()
        .take_while(|&&sigma| sigma > threshold)
        .count()
}

fn to_complex_mat<T>(matrix: MatRef<'_, T>) -> Mat<Complex<T::Real>>
where
    T: ComplexField + Copy,
    T::Real: Float + RealField,
{
    Mat::from_fn(matrix.nrows(), matrix.ncols(), |row, col| {
        let value = matrix[(row, col)];
        Complex::new(value.real(), value.imag())
    })
}

/// Sparse complex shift wrapper used by sparse transfer evaluation.
///
/// The stored pattern contains an explicit diagonal even if the original `A`
/// did not. That lets every shifted operator `point I - A` reuse the same CSC
/// symbolic pattern across all frequency samples.
#[derive(Clone, Debug)]
struct ShiftedComplexCscMatrix<R> {
    matrix: SparseColMat<usize, Complex<R>>,
    base_values: Vec<Complex<R>>,
    diag_positions: Vec<usize>,
}

impl<R> ShiftedComplexCscMatrix<R>
where
    R: Float + RealField,
{
    /// Converts a sparse real-or-complex state matrix into complex CSC storage
    /// with an explicit diagonal.
    fn from_matrix<T>(matrix: SparseColMatRef<'_, usize, T>) -> Result<Self, LtiError>
    where
        T: ComplexField<Real = R> + Copy,
    {
        let nrows = matrix.nrows();
        let ncols = matrix.ncols();
        let mut triplets = Vec::with_capacity(matrix.row_idx().len() + nrows.min(ncols));

        for col in 0..ncols {
            let start = matrix.col_ptr()[col];
            let end = matrix.col_ptr()[col + 1];
            let mut has_diag = false;
            for idx in start..end {
                let row = matrix.row_idx()[idx];
                has_diag |= row == col;
                let value = matrix.val()[idx];
                triplets.push(Triplet::new(
                    row,
                    col,
                    Complex::new(value.real(), value.imag()),
                ));
            }
            if col < nrows && !has_diag {
                triplets.push(Triplet::new(col, col, Complex::new(R::zero(), R::zero())));
            }
        }

        let matrix =
            SparseColMat::<usize, Complex<R>>::try_new_from_triplets(nrows, ncols, &triplets)?;
        let diag_positions = diagonal_positions(matrix.as_ref());
        let base_values = matrix.val().to_vec();
        Ok(Self {
            matrix,
            base_values,
            diag_positions,
        })
    }

    /// Overwrites the matrix entries with the shifted operator `point I - A`.
    fn apply_point_minus_matrix(&mut self, point: Complex<R>) {
        let values = self.matrix.val_mut();
        for (dst, &src) in values.iter_mut().zip(self.base_values.iter()) {
            *dst = -src;
        }
        for &diag_idx in &self.diag_positions {
            values[diag_idx] += point;
        }
    }

    fn as_ref(&self) -> SparseColMatRef<'_, usize, Complex<R>> {
        self.matrix.as_ref()
    }
}

fn diagonal_positions<R>(matrix: SparseColMatRef<'_, usize, Complex<R>>) -> Vec<usize>
where
    R: Float + RealField,
{
    let mut positions = Vec::with_capacity(matrix.ncols());
    for col in 0..matrix.ncols() {
        let start = matrix.col_ptr()[col];
        let end = matrix.col_ptr()[col + 1];
        let mut found = None;
        for idx in start..end {
            if matrix.row_idx()[idx] == col {
                found = Some(idx);
                break;
            }
        }
        positions.push(found.expect("shifted sparse operator must contain a diagonal"));
    }
    positions
}

pub(crate) fn sparse_transfer_at_points<T>(
    a: SparseColMatRef<'_, usize, T>,
    b: MatRef<'_, T>,
    c: MatRef<'_, T>,
    d: MatRef<'_, T>,
    points: &[Complex<T::Real>],
) -> Result<Vec<Mat<Complex<T::Real>>>, LtiError>
where
    T: ComplexField + Copy,
    T::Real: Float + RealField,
{
    let mut shifted = ShiftedComplexCscMatrix::from_matrix(a)?;
    let mut lu = SparseLu::<usize, Complex<T::Real>>::analyze(
        shifted.as_ref(),
        LuSymbolicParams::default(),
    )?;
    let b_complex = to_complex_mat(b);
    let c_complex = to_complex_mat(c);
    let d_complex = to_complex_mat(d);

    let mut values = Vec::with_capacity(points.len());
    for &point in points {
        shifted.apply_point_minus_matrix(point);
        lu.refactor(
            shifted.as_ref(),
            Par::Seq,
            Spec::<PartialPivLuParams, Complex<T::Real>>::default(),
        )?;

        let mut state_response = b_complex.as_ref().to_owned();
        lu.solve_in_place(state_response.as_mut(), Par::Seq)?;

        let gain = dense_mul(c_complex.as_ref(), state_response.as_ref());
        let out = Mat::from_fn(gain.nrows(), gain.ncols(), |row, col| {
            gain[(row, col)] + d_complex[(row, col)]
        });
        if !out.as_ref().is_all_finite() {
            return Err(LtiError::NonFiniteResult {
                which: "sparse_transfer_at",
            });
        }
        values.push(out);
    }

    Ok(values)
}

#[cfg(test)]
mod tests {
    use crate::control::lti::state_space::{
        ContinuousStateSpace, DiscreteStateSpace, SparseContinuousStateSpace,
        SparseDiscreteStateSpace,
    };
    use faer::Mat;
    use faer::complex::Complex;
    use faer::sparse::{SparseColMat, Triplet};
    use nalgebra::ComplexField;

    fn assert_close_complex(
        lhs: MatRef<'_, Complex<f64>>,
        rhs: MatRef<'_, Complex<f64>>,
        tol: f64,
    ) {
        assert_eq!(lhs.nrows(), rhs.nrows());
        assert_eq!(lhs.ncols(), rhs.ncols());
        for col in 0..lhs.ncols() {
            for row in 0..lhs.nrows() {
                let err = (lhs[(row, col)] - rhs[(row, col)]).abs();
                assert!(
                    err <= tol,
                    "entry ({row}, {col}) differs: lhs={:?}, rhs={:?}, err={err}, tol={tol}",
                    lhs[(row, col)],
                    rhs[(row, col)],
                );
            }
        }
    }

    use faer::MatRef;

    #[test]
    fn continuous_poles_and_stability_work() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => -1.0,
            (1, 1) => -3.0,
            _ => 0.0,
        });
        let b = Mat::from_fn(2, 1, |row, _| if row == 0 { 1.0 } else { 0.0 });
        let c = Mat::from_fn(1, 2, |_, col| if col == 0 { 1.0 } else { 0.0 });
        let sys = ContinuousStateSpace::with_zero_feedthrough(a, b, c).unwrap();

        let poles = sys.poles().unwrap();
        assert_eq!(poles.len(), 2);
        assert!(poles[0].abs() >= poles[1].abs());
        assert!(sys.is_asymptotically_stable().unwrap());
    }

    #[test]
    fn discrete_stability_detects_unit_disk_violation() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 0.5,
            (1, 1) => 1.1,
            _ => 0.0,
        });
        let b = Mat::from_fn(2, 1, |row, _| if row == 0 { 1.0 } else { 0.0 });
        let c = Mat::from_fn(1, 2, |_, col| if col == 0 { 1.0 } else { 0.0 });
        let sys = DiscreteStateSpace::with_zero_feedthrough(a, b, c, 0.1).unwrap();

        assert!(!sys.is_asymptotically_stable().unwrap());
    }

    #[test]
    fn controllability_and_observability_detect_nonminimal_system() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 0.0,
            (1, 1) => -1.0,
            _ => 0.0,
        });
        let b = Mat::from_fn(2, 1, |row, _| if row == 0 { 1.0 } else { 0.0 });
        let c = Mat::from_fn(1, 2, |_, col| if col == 0 { 1.0 } else { 0.0 });
        let sys = ContinuousStateSpace::with_zero_feedthrough(a, b, c).unwrap();

        assert_eq!(sys.controllability_rank().unwrap(), 1);
        assert_eq!(sys.observability_rank().unwrap(), 1);
        assert!(!sys.is_minimal().unwrap());
    }

    #[test]
    fn transfer_at_matches_first_order_closed_form() {
        let a = Mat::from_fn(1, 1, |_, _| -2.0);
        let b = Mat::from_fn(1, 1, |_, _| 3.0);
        let c = Mat::from_fn(1, 1, |_, _| 4.0);
        let d = Mat::from_fn(1, 1, |_, _| 5.0);
        let sys = ContinuousStateSpace::new(a, b, c, d).unwrap();

        let point = Complex::new(1.0, 2.0);
        let got = sys.transfer_at(point).unwrap();
        let expected = Mat::from_fn(1, 1, |_, _| {
            Complex::new(5.0, 0.0) + Complex::new(12.0, 0.0) / (point + Complex::new(2.0, 0.0))
        });
        assert_close_complex(got.as_ref(), expected.as_ref(), 1.0e-12);
    }

    #[test]
    fn dc_gain_matches_transfer_at_zero_or_one() {
        let a = Mat::from_fn(1, 1, |_, _| -2.0);
        let b = Mat::from_fn(1, 1, |_, _| 3.0);
        let c = Mat::from_fn(1, 1, |_, _| 4.0);
        let d = Mat::from_fn(1, 1, |_, _| 5.0);
        let cont = ContinuousStateSpace::new(a.clone(), b.clone(), c.clone(), d.clone()).unwrap();
        let disc = DiscreteStateSpace::new(Mat::from_fn(1, 1, |_, _| 0.25), b, c, d, 0.1).unwrap();

        assert_close_complex(
            cont.dc_gain().unwrap().as_ref(),
            cont.transfer_at(Complex::new(0.0, 0.0)).unwrap().as_ref(),
            1.0e-12,
        );
        assert_close_complex(
            disc.dc_gain().unwrap().as_ref(),
            disc.transfer_at(Complex::new(1.0, 0.0)).unwrap().as_ref(),
            1.0e-12,
        );
    }

    fn sparse_scalar_system(a: f64, b: f64, c: f64, d: f64) -> SparseContinuousStateSpace<f64> {
        let a = SparseColMat::<usize, f64>::try_new_from_triplets(1, 1, &[Triplet::new(0, 0, a)])
            .unwrap();
        let b = Mat::from_fn(1, 1, |_, _| b);
        let c = Mat::from_fn(1, 1, |_, _| c);
        let d = Mat::from_fn(1, 1, |_, _| d);
        SparseContinuousStateSpace::new(a, b, c, d).unwrap()
    }

    #[test]
    fn sparse_transfer_at_matches_dense_reference() {
        let dense = ContinuousStateSpace::new(
            Mat::from_fn(1, 1, |_, _| -2.0),
            Mat::from_fn(1, 1, |_, _| 3.0),
            Mat::from_fn(1, 1, |_, _| 4.0),
            Mat::from_fn(1, 1, |_, _| 5.0),
        )
        .unwrap();
        let sparse = sparse_scalar_system(-2.0, 3.0, 4.0, 5.0);

        let point = Complex::new(1.0, 2.0);
        let dense_value = dense.transfer_at(point).unwrap();
        let sparse_value = sparse.transfer_at(point).unwrap();
        assert_close_complex(sparse_value.as_ref(), dense_value.as_ref(), 1.0e-12);
    }

    #[test]
    fn sparse_discrete_dc_gain_matches_dense_reference() {
        let dense = DiscreteStateSpace::new(
            Mat::from_fn(1, 1, |_, _| 0.5),
            Mat::from_fn(1, 1, |_, _| 2.0),
            Mat::from_fn(1, 1, |_, _| 3.0),
            Mat::from_fn(1, 1, |_, _| 4.0),
            0.1,
        )
        .unwrap();

        let a_sparse =
            SparseColMat::<usize, f64>::try_new_from_triplets(1, 1, &[Triplet::new(0, 0, 0.5)])
                .unwrap();
        let sparse = SparseDiscreteStateSpace::new(
            a_sparse,
            Mat::from_fn(1, 1, |_, _| 2.0),
            Mat::from_fn(1, 1, |_, _| 3.0),
            Mat::from_fn(1, 1, |_, _| 4.0),
            0.1,
        )
        .unwrap();

        let dense_gain = dense.dc_gain().unwrap();
        let sparse_gain = sparse.dc_gain().unwrap();
        assert_close_complex(sparse_gain.as_ref(), dense_gain.as_ref(), 1.0e-12);
    }
}

//! Evaluate or fit a polynomial of order N to a set of x,y points using Levenberg-Marquardt method.

use levenberg_marquardt::{LeastSquaresProblem, LevenbergMarquardt};
use nalgebra::{DMatrix, DVector, Dyn, storage::Owned};

/// Evaluate a polynomial with coeffs ordered by increasing order (c[0] + [c1]*x + ...)
#[inline]
pub fn polyval(x: f64, c: &[f64]) -> f64 {
    // Horner's Method for efficient polynomial evaluation using one multiply-add
    // operation per coefficient.
    // The first-visited coefficient gets multiplied by `x` the most times,
    // so we start from the last coefficient and work back, ending at the
    // first coefficient, which never gets multiplied by `x`.
    c.iter().rev().fold(0.0, |acc, &coef| x.mul_add(acc, coef))
}

/// Fit a polynomial of order N to a set of x,y points using Levenberg-Marquardt method
pub fn polyfit(points: &[(f64, f64)], order: usize) -> Result<Vec<f64>, String> {
    let coeff_count = order.saturating_add(1); // no risk of trillionth order polynomial
    if points.is_empty() {
        return Err("Polynomial fit requires at least one calibration point".to_string());
    }
    if coeff_count > points.len() {
        return Err(format!(
            "Polynomial fit of order {} requires at least {} points; got {}",
            order,
            coeff_count,
            points.len()
        ));
    }
    if points.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
        return Err("Polynomial fit requires finite calibration points".to_string());
    }

    let problem = PolynomialFitProblem::new(points, coeff_count);
    let (problem, report) = LevenbergMarquardt::new().minimize(problem);
    if !report.termination.was_successful() {
        return Err(format!(
            "Polynomial fit failed: {:?} (objective {:.3e})",
            report.termination, report.objective_function
        ));
    }
    let coefficients: Vec<f64> = problem.params().iter().copied().collect();
    if coefficients.iter().any(|c| !c.is_finite()) {
        return Err("Polynomial fit produced non-finite coefficients".to_string());
    }

    Ok(coefficients)
}

/// Levenberg-Marquardt curve-fitting wrapper for n'th order polynomial
struct PolynomialFitProblem<'a> {
    points: &'a [(f64, f64)],
    params: DVector<f64>,
}

impl<'a> PolynomialFitProblem<'a> {
    fn new(points: &'a [(f64, f64)], coeff_count: usize) -> Self {
        debug_assert!(
            coeff_count > 0,
            "Polynomial requires at least one coefficient"
        );
        let mut params = DVector::zeros(coeff_count);
        if !points.is_empty() {
            let avg = points.iter().map(|(_, y)| *y).sum::<f64>() / points.len() as f64;
            params[0] = avg;
        }
        Self { points, params }
    }
}

impl<'a> LeastSquaresProblem<f64, Dyn, Dyn> for PolynomialFitProblem<'a> {
    type ResidualStorage = Owned<f64, Dyn>;
    type JacobianStorage = Owned<f64, Dyn, Dyn>;
    type ParameterStorage = Owned<f64, Dyn>;

    fn set_params(&mut self, x: &nalgebra::Vector<f64, Dyn, Self::ParameterStorage>) {
        self.params.copy_from(x);
    }

    fn params(&self) -> nalgebra::Vector<f64, Dyn, Self::ParameterStorage> {
        self.params.clone_owned()
    }

    fn residuals(&self) -> Option<nalgebra::Vector<f64, Dyn, Self::ResidualStorage>> {
        let mut residuals = DVector::zeros(self.points.len());
        for (row, &(x, y)) in self.points.iter().enumerate() {
            residuals[row] = polyval(x, &self.params.as_slice()) - y;
        }
        Some(residuals)
    }

    fn jacobian(&self) -> Option<nalgebra::Matrix<f64, Dyn, Dyn, Self::JacobianStorage>> {
        let rows = self.points.len();
        let cols = self.params.len();
        let mut jacobian = DMatrix::zeros(rows, cols);
        for (row, &(x, _)) in self.points.iter().enumerate() {
            let mut x_pow = 1.0;
            for col in 0..cols {
                jacobian[(row, col)] = x_pow;
                x_pow *= x;
            }
        }
        Some(jacobian)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_polynomial_coefficients() {
        let points = [
            (-2.0, -0.8),
            (-1.0, 0.3),
            (0.0, 1.0),
            (1.0, 1.3),
            (2.0, 1.2),
        ]; // These are from an order 2 polynomial
        let coeffs = polyfit(&points, 2).unwrap();
        let expected = [1.0, 0.5, -0.2];

        // Check coeffs
        for (got, exp) in coeffs.iter().zip(expected) {
            assert!((got - exp).abs() < 1e-15, "expected {} got {}", exp, got);
        }

        // Check values at control points
        for (x, y) in points.iter() {
            let yf = polyval(*x, &coeffs);
            let err = yf - *y;
            assert!(err.abs() < 1e-15, "{yf} != {y}");
        }
    }

    #[test]
    fn errors_on_insufficient_points() {
        let points = [(0.0, 1.0), (1.0, 2.0)];
        let result = polyfit(&points, 2);
        assert!(result.is_err());
    }
}

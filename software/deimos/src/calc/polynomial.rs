//! Evaluate an Nth order polynomial calibration curve.

use super::*;
use crate::{calc_config, calc_input_names, calc_output_names};
use levenberg_marquardt::{LeastSquaresProblem, LevenbergMarquardt};
use nalgebra::{DMatrix, DVector, Dyn, storage::Owned};


/// Polynomial calibration: y = c0 + c1*x + c2*x^2 + ...
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Polynomial {
    // User inputs
    input_name: String,
    coefficients: Vec<f64>,
    note: String,
    save_outputs: bool,

    // Values provided by calc orchestrator during init
    #[serde(skip)]
    input_index: usize,

    #[serde(skip)]
    output_index: usize,
}

impl Polynomial {
    pub fn new(
        input_name: String,
        coefficients: Vec<f64>,
        note: String,
        save_outputs: bool,
    ) -> Self {
        Self {
            input_name,
            coefficients,
            note,
            save_outputs,
            input_index: usize::MAX,
            output_index: usize::MAX,
        }
    }

    pub fn fit_from_points<S1, S2>(
        input_name: S1,
        points: &[(f64, f64)],
        order: usize,
        note: S2,
        save_outputs: bool,
    ) -> Result<Self, String>
    where
        S1: Into<String>,
        S2: Into<String>,
    {
        let coeff_count = order
            .checked_add(1)
            .ok_or_else(|| "Polynomial order is too large".to_string())?;
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

        Ok(Self::new(
            input_name.into(),
            coefficients,
            note.into(),
            save_outputs,
        ))
    }

    fn eval_poly(&self, x: f64) -> f64 {
        // Horner's method, coefficients assumed ascending order c0, c1, ...
        self.coefficients
            .iter()
            .rev()
            .fold(0.0, |acc, &coef| acc * x + coef)
    }
}

#[typetag::serde]
impl Calc for Polynomial {
    fn init(
        &mut self,
        _: ControllerCtx,
        input_indices: Vec<usize>,
        output_range: Range<usize>,
    ) -> Result<(), String> {
        if self.coefficients.is_empty() {
            return Err("Polynomial coefficients cannot be empty".to_string());
        }
        self.input_index = input_indices
            .first()
            .copied()
            .ok_or_else(|| "Polynomial calc missing input index".to_string())?;
        self.output_index = output_range
            .clone()
            .next()
            .ok_or_else(|| "Polynomial calc missing output index".to_string())?;
        Ok(())
    }

    fn terminate(&mut self) -> Result<(), String> {
        self.input_index = usize::MAX;
        self.output_index = usize::MAX;
        Ok(())
    }

    fn eval(&mut self, tape: &mut [f64]) -> Result<(), String> {
        let x = tape[self.input_index];
        let y = self.eval_poly(x);
        tape[self.output_index] = y;
        Ok(())
    }

    fn get_input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        let mut map = BTreeMap::new();
        map.insert("x".to_owned(), self.input_name.clone());
        map
    }

    fn update_input_map(&mut self, field: &str, source: &str) -> Result<(), String> {
        if field == "x" {
            self.input_name = source.to_owned();
            Ok(())
        } else {
            Err(format!("Unrecognized field {field}"))
        }
    }

    calc_config!();
    calc_input_names!(x);
    calc_output_names!(y);
}

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

    fn eval_poly(&self, x: f64) -> f64 {
        self.params
            .iter()
            .rev()
            .fold(0.0, |acc, &coef| acc * x + coef)
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
            residuals[row] = self.eval_poly(x) - y;
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
        ];
        let poly = Polynomial::fit_from_points("x", &points, 2, "test", false).unwrap();
        let expected = [1.0, 0.5, -0.2];
        for (got, exp) in poly.coefficients.iter().zip(expected) {
            assert!((got - exp).abs() < 1e-15, "expected {} got {}", exp, got);
        }
    }

    #[test]
    fn errors_on_insufficient_points() {
        let points = [(0.0, 1.0), (1.0, 2.0)];
        let result = Polynomial::fit_from_points("x", &points, 2, "test", false);
        assert!(result.is_err());
    }
}

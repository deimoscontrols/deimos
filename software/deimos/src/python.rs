use pyo3::exceptions;
use pyo3::prelude::*;


/// Python bindings for the deimos control program.
#[pymodule]
#[pyo3(name = "deimos")]
fn deimos<'py>(_py: Python, m: &Bound<'py, PyModule>) -> PyResult<()> {
    // m.add_function(wrap_pyfunction!(interpn_linear_regular_f64, m)?)?;
    Ok(())
}

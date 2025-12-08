use pyo3::prelude::*;

use crate::{calc::Calc, python::BackendErr};

use pythonize::depythonize;

impl<'a, 'py> FromPyObject<'a, 'py> for Box<dyn Calc> {
    type Error = PyErr;

    fn extract(obj: Borrowed<'a, 'py, PyAny>) -> Result<Self, Self::Error> {
        let a: Box<dyn Calc> =
            depythonize::<Box<dyn Calc>>(obj.as_any()).map_err(|e| BackendErr::InvalidCalcErr {
                msg: format!("Unable to process Calc object: {e}"),
            })?;
        Ok(a)
    }
}

// #[pymodule]
// #[pyo3(name = "calc")]
// pub(crate) fn calc<'py>(_py: Python, m: &Bound<'py, PyModule>) -> PyResult<()> {
//     m.add_class::<Affine>()?;

//     Ok(())
// }

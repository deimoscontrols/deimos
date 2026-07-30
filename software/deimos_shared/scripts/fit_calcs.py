#!/usr/bin/env python3
# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#     "interpn[pydantic]==0.11.2",
#     "numpy==1.26.4",
#     "scipy>=1.13,<2",
# ]
# ///
"""Generate and validate compact rev7 thermocouple and Pt100 calculations.

The emitted Rust data uses `f32`, so acceptance checks emulate the operation
rounding of the firmware evaluator rather than validating only the fitted
`f64` coefficients.

Run from the repository root with:
    uv run software/deimos_shared/scripts/fit_calcs.py

References:
    [1] IEC 60751, *Industrial platinum resistance thermometers and platinum
        temperature sensors*.
    [2] G. W. Burns et al., *Temperature-Electromotive Force Reference
        Functions and Tables for the Letter-Designated Thermocouple Types Based
        on the ITS-90*, NIST Monograph 175, 1993,
        doi: 10.6028/NIST.MONO.175.
    [3] C. de Boor, *A Practical Guide to Splines*, revised ed., Springer, 2001.
"""

from pathlib import Path

import numpy as np
from interpn import MultiBsplineRegular
from interpn.serialization import ArrayF32, ArrayF64
from scipy.optimize import brentq, least_squares, minimize_scalar

CALCS_DIR = Path(__file__).resolve().parents[1] / "src/calcs"
TC_OUTPUT = CALCS_DIR / "tc_ktype_data.rs"
RTD_OUTPUT = CALCS_DIR / "rtd_pt100_data.rs"
REPORT_OUTPUT = Path(__file__).with_name("FIT_REPORT.md")

ZERO_C_K = 273.15
MAX_TEMPERATURE_ERROR_K = 0.01
ACCEPTANCE_ERROR_K = 0.0099

RTD_MIN_C = -200.0
RTD_MAX_C = 850.0
RTD_R0_OHM = 100.0
RTD_A = 3.9083e-3
RTD_B = -5.775e-7
RTD_C = -4.183e-12

TC_MIN_C = -210.0
TC_MAX_C = 1370.0
FIT_SAMPLES_PER_SPAN = 16
FORWARD_INTERVAL_CANDIDATES = (32, 48, 64, 72, 80, 88, 96, 112)
INVERSE_INTERVAL_CANDIDATES = (192, 256, 320, 384, 416, 448, 480, 512, 544, 576, 608, 640)

# NIST ITS-90 monograph 175 coefficients, with voltage expressed in mV.
FORWARD_NEGATIVE_C_TO_MV = np.array(
    [
        0.000000000000e00,
        0.394501280250e-01,
        0.236223735980e-04,
        -0.328589067840e-06,
        -0.499048287770e-08,
        -0.675090591730e-10,
        -0.574103274280e-12,
        -0.310888728940e-14,
        -0.104516093650e-16,
        -0.198892668780e-19,
        -0.163226974860e-22,
    ]
)
FORWARD_POSITIVE_C_TO_MV = np.array(
    [
        -0.176004136860e-01,
        0.389212049750e-01,
        0.185587700320e-04,
        -0.994575928740e-07,
        0.318409457190e-09,
        -0.560728448890e-12,
        0.560750590590e-15,
        -0.320207200030e-18,
        0.971511471520e-22,
        -0.121047212750e-25,
    ]
)


def nist_voltage_v(temperature_c):
    """Evaluate the NIST ITS-90 type-K forward reference function.

    Args:
        temperature_c: Temperature scalar or array in `degC` with shape `(...)`.

    Returns:
        Type-K equivalent voltage in `V` with the input shape `(...)`.

    References:
        [1] G. W. Burns et al., NIST Monograph 175, 1993,
        doi: 10.6028/NIST.MONO.175.
    """
    temperature_c = np.asarray(temperature_c)
    negative = np.polynomial.polynomial.polyval(
        temperature_c, FORWARD_NEGATIVE_C_TO_MV
    )
    positive = np.polynomial.polynomial.polyval(
        temperature_c, FORWARD_POSITIVE_C_TO_MV
    ) + 0.1185976 * np.exp(-0.1183432e-3 * (temperature_c - 126.9686) ** 2)
    return np.where(temperature_c < 0.0, negative, positive) / 1000.0


TC_MIN_V = float(nist_voltage_v(TC_MIN_C))
TC_MAX_V = float(nist_voltage_v(TC_MAX_C))


def nist_temperature_c(voltage_v):
    """Invert the monotonic NIST type-K forward function for one voltage.

    Args:
        voltage_v: Type-K equivalent voltage scalar in `V`.

    Returns:
        Junction temperature scalar in `degC`.
    """
    return brentq(
        lambda temperature_c: float(nist_voltage_v(temperature_c)) - voltage_v,
        -270.0,
        1372.0,
        xtol=1e-13,
    )


def pt100_resistance_ohm(temperature_c):
    """Evaluate the IEC 60751 Pt100 Callendar--Van Dusen curve.

    Args:
        temperature_c: Temperature scalar or array in `degC` with shape `(...)`.

    Returns:
        Pt100 resistance in `ohm` with the input shape `(...)`.

    References:
        [1] IEC 60751, *Industrial platinum resistance thermometers and
        platinum temperature sensors*.
    """
    temperature_c = np.asarray(temperature_c)
    negative_term = np.where(
        temperature_c < 0.0,
        RTD_C * (temperature_c - 100.0) * temperature_c**3,
        0.0,
    )
    return RTD_R0_OHM * (
        1.0 + RTD_A * temperature_c + RTD_B * temperature_c**2 + negative_term
    )


RTD_MIN_R = float(pt100_resistance_ohm(RTD_MIN_C))
RTD_MAX_R = float(pt100_resistance_ohm(RTD_MAX_C))
RTD_RESISTANCE_ORIGIN = (RTD_MIN_R + RTD_MAX_R) / 2.0
RTD_RESISTANCE_SCALE = 2.0 / (RTD_MAX_R - RTD_MIN_R)


def pt100_inverse_tangent_k_per_ohm(temperature_c):
    """Evaluate the local slope of the exact inverse Pt100 curve.

    Args:
        temperature_c: Temperature scalar in `degC`.

    Returns:
        Inverse-curve tangent scalar in `K/ohm`.
    """
    derivative = RTD_A + 2.0 * RTD_B * temperature_c
    if temperature_c < 0.0:
        derivative += RTD_C * temperature_c**2 * (4.0 * temperature_c - 300.0)
    return 1.0 / (RTD_R0_OHM * derivative)


def spline_from_coefficients(value_min, value_max, coefficients):
    """Construct the same precomputed-coefficient interpolator used by firmware.

    Args:
        value_min: Coordinate scalar at the first regular-grid node.
        value_max: Coordinate scalar at the last regular-grid node.
        coefficients: Precomputed B-spline coefficients with shape `(n_grid,)`.

    Returns:
        `interpn.MultiBsplineRegular` borrowing equivalent typed arrays.
    """
    coefficients = np.ascontiguousarray(coefficients)
    dtype = coefficients.dtype
    wrapper = ArrayF32 if dtype == np.float32 else ArrayF64
    start = np.array([value_min], dtype=dtype)
    step = np.array(
        [
            (dtype.type(value_max) - dtype.type(value_min))
            / dtype.type(len(coefficients) - 1)
        ],
        dtype=dtype,
    )
    return MultiBsplineRegular(
        dims=[len(coefficients)],
        starts=wrapper(data=start),
        steps=wrapper(data=step),
        coeffs=wrapper(data=coefficients),
        linearize_extrapolation=True,
    )


def fit_regular_bspline(function, value_min, value_max, intervals):
    """Fit one `interpn` regular B-spline with Levenberg--Marquardt.

    The parameter vector contains nodal values, seeded from the reference
    function. `interpn` converts those values to its boundary-conditioned
    B-spline coefficients for every residual evaluation, so the fit and emitted
    evaluator share exactly one definition of the spline basis.

    Args:
        function: Callable mapping coordinates with shape `(n_values,)` to fit
            values with the same shape.
        value_min: Coordinate scalar at the first regular-grid node.
        value_max: Coordinate scalar at the last regular-grid node.
        intervals: Number of uniform spline spans.

    Returns:
        Precomputed `f64` B-spline coefficients with shape `(intervals + 1,)`.

    Raises:
        RuntimeError: The Levenberg--Marquardt solve does not converge.
    """
    node_count = intervals + 1
    starts = np.array([value_min], dtype=np.float64)
    steps = np.array([(value_max - value_min) / intervals], dtype=np.float64)
    nodes = np.linspace(value_min, value_max, node_count)
    initial_values = np.ascontiguousarray(function(nodes), dtype=np.float64)
    fit_values = np.linspace(
        value_min,
        value_max,
        intervals * FIT_SAMPLES_PER_SPAN + 1,
    )
    targets = function(fit_values)

    def interpolator(nodal_values):
        return MultiBsplineRegular.new(
            [node_count],
            starts,
            steps,
            np.ascontiguousarray(nodal_values),
            True,
        )

    result = least_squares(
        lambda nodal_values: interpolator(nodal_values).eval([fit_values]) - targets,
        initial_values,
        method="lm",
        ftol=1.0e-13,
        xtol=1.0e-13,
        gtol=1.0e-13,
        max_nfev=5_000,
    )
    if not result.success:
        raise RuntimeError(f"B-spline fit did not converge: {result.message}")
    return np.array(interpolator(result.x).coeffs.data, copy=True)


def evaluate_spline_scalar(interpolator, value):
    """Evaluate one scalar through an `interpn` B-spline.

    Args:
        interpolator: Configured `interpn.MultiBsplineRegular`.
        value: Scalar coordinate in interpolator input units.

    Returns:
        Interpolated scalar in coefficient units.
    """
    dtype = interpolator.coeffs.data.dtype
    observation = np.array([value], dtype=dtype)
    return float(interpolator.eval([observation])[0])


def evaluate_spline_array(interpolator, values):
    """Evaluate an array through the production `interpn` B-spline path.

    Args:
        interpolator: Configured `interpn.MultiBsplineRegular`.
        values: Coordinates with shape `(n_values,)`.

    Returns:
        Interpolated values with shape `(n_values,)` and coefficient dtype.
    """
    values = np.ascontiguousarray(values, dtype=interpolator.coeffs.data.dtype)
    return interpolator.eval([values])


def spline_derivative(interpolator, value):
    """Evaluate the analytic `interpn` B-spline derivative at one coordinate.

    Args:
        interpolator: Configured `interpn.MultiBsplineRegular`.
        value: Scalar coordinate in interpolator input units.

    Returns:
        Derivative scalar in coefficient units per coordinate unit.
    """
    dtype = interpolator.coeffs.data.dtype
    observation = np.array([value], dtype=dtype)
    return float(interpolator.eval_grad([observation])[0, 0])


def strict_interval_maximum(error_function, lower, upper):
    """Search one interval for the largest absolute scalar error.

    The bounded local optimizations are seeded between every neighboring pair
    of 17 regular points so narrow local extrema are not hidden by a dense-grid
    summary.

    Args:
        error_function: Callable returning signed scalar error.
        lower: Lower interval coordinate.
        upper: Upper interval coordinate.

    Returns:
        `(absolute_error, coordinate)` pair in the function's units.
    """
    seeds = np.linspace(lower, upper, 17)
    best = max((abs(error_function(seed)), seed) for seed in seeds)
    for left, right in zip(seeds[:-1], seeds[1:]):
        result = minimize_scalar(
            lambda value: -abs(error_function(value)),
            bounds=(left, right),
            method="bounded",
            options={"xatol": 1e-12},
        )
        candidate = (abs(error_function(result.x)), result.x)
        if candidate > best:
            best = candidate
    return best


def validate_local_maxima(error_function, edges, explicit_points=()):
    """Find the largest locally optimized error across all validation spans.

    Args:
        error_function: Callable returning signed scalar error.
        edges: Ordered validation-span boundaries with shape `(n_edges,)`.
        explicit_points: Additional scalar coordinates, such as curve branch
            points, that must be checked exactly.

    Returns:
        Global `(absolute_error, coordinate)` pair in the function's units.
    """
    maxima = [
        strict_interval_maximum(error_function, left, right)
        for left, right in zip(edges[:-1], edges[1:])
    ]
    maxima.extend((abs(error_function(point)), point) for point in explicit_points)
    return max(maxima)


def strict_derivative_minimum(value_min, value_max, intervals, interpolator):
    """Search every spline span for the minimum analytic derivative.

    Args:
        value_min: Coordinate scalar at the first span boundary.
        value_max: Coordinate scalar at the last span boundary.
        intervals: Number of regular spline spans.
        interpolator: Configured `interpn.MultiBsplineRegular`.

    Returns:
        `(derivative, coordinate)` pair in control-value units per coordinate
        unit and coordinate units, respectively.
    """
    edges = np.linspace(value_min, value_max, intervals + 1)
    best = (float("inf"), value_min)
    for left, right in zip(edges[:-1], edges[1:]):
        seeds = np.linspace(left, right, 9)
        for seed in seeds:
            candidate = (spline_derivative(interpolator, seed), seed)
            if candidate < best:
                best = candidate
        for seed_left, seed_right in zip(seeds[:-1], seeds[1:]):
            result = minimize_scalar(
                lambda value: spline_derivative(interpolator, value),
                bounds=(seed_left, seed_right),
                method="bounded",
                options={"xatol": 1e-12},
            )
            candidate = (float(result.fun), result.x)
            if candidate < best:
                best = candidate
    return best


def evaluate_rtd_polynomial(resistance_ohm, coefficients, emulate_f32):
    """Evaluate the fitted global Pt100 inverse and endpoint extrapolation.

    Args:
        resistance_ohm: Pt100 resistance scalar in `ohm`.
        coefficients: Ascending power-basis coefficients in `degC` with shape
            `(degree + 1,)`.
        emulate_f32: Whether to round every evaluator operation to `f32`.

    Returns:
        Absolute temperature scalar in `K`.
    """
    dtype = np.float32 if emulate_f32 else float
    resistance = dtype(resistance_ohm)
    origin = dtype(RTD_RESISTANCE_ORIGIN)
    scale = dtype(RTD_RESISTANCE_SCALE)
    coefficients = np.asarray(coefficients, dtype=dtype)
    minimum = dtype(RTD_MIN_R)
    maximum = dtype(RTD_MAX_R)

    def inside(value):
        normalized = dtype((value - origin) * scale)
        output = dtype(coefficients[-1])
        for coefficient in coefficients[-2::-1]:
            output = dtype(output * normalized + coefficient)
        return dtype(output + dtype(ZERO_C_K))

    if resistance < minimum:
        slope = dtype(pt100_inverse_tangent_k_per_ohm(RTD_MIN_C))
        return dtype(inside(minimum) + slope * (resistance - minimum))
    if resistance > maximum:
        slope = dtype(pt100_inverse_tangent_k_per_ohm(RTD_MAX_C))
        return dtype(inside(maximum) + slope * (resistance - maximum))
    return inside(resistance)


def fit_rtd_inverse():
    """Fit and validate the lowest-degree acceptable global Pt100 inverse.

    Candidate polynomials are fit in a normalized resistance coordinate, then
    validated against local error maxima after conversion to the exact emitted
    `f32` coefficients. The selected polynomial must also remain strictly
    monotonic over the IEC 60751 range.

    Returns:
        Mapping containing polynomial degree, coefficients with shape
        `(degree + 1,)`, scalar error metrics in `K`, and minimum derivative in
        `K/ohm`.

    Raises:
        RuntimeError: No candidate meets the `0.01 K` error requirement or the
        selected polynomial is not strictly monotonic.
    """
    temperatures = np.linspace(RTD_MIN_C, RTD_MAX_C, 2_000_001)
    resistances = pt100_resistance_ohm(temperatures)
    normalized = (resistances - RTD_RESISTANCE_ORIGIN) * RTD_RESISTANCE_SCALE
    edges = pt100_resistance_ohm(np.linspace(RTD_MIN_C, RTD_MAX_C, 106))

    selected = None
    for degree in range(5, 21):
        chebyshev = np.polynomial.chebyshev.chebfit(normalized, temperatures, degree)
        power_f64 = np.polynomial.chebyshev.cheb2poly(chebyshev)
        power_f32 = power_f64.astype(np.float32)

        def f32_error(resistance):
            reference_c = brentq(
                lambda temperature: float(pt100_resistance_ohm(temperature)) - resistance,
                RTD_MIN_C,
                RTD_MAX_C,
                xtol=1e-13,
            )
            return (
                float(evaluate_rtd_polynomial(resistance, power_f32, True))
                - (reference_c + ZERO_C_K)
            )

        f32_max = validate_local_maxima(
            f32_error,
            edges,
            explicit_points=(RTD_MIN_R, 100.0, RTD_MAX_R),
        )
        if f32_max[0] <= ACCEPTANCE_ERROR_K:
            selected = (degree, power_f64, power_f32, f32_max)
            break
    if selected is None:
        raise RuntimeError("No global Pt100 inverse polynomial met the error limit")

    degree, power_f64, power_f32, f32_max = selected

    def f64_error(resistance):
        reference_c = brentq(
            lambda temperature: float(pt100_resistance_ohm(temperature)) - resistance,
            RTD_MIN_C,
            RTD_MAX_C,
            xtol=1e-13,
        )
        return (
            float(evaluate_rtd_polynomial(resistance, power_f64, False))
            - (reference_c + ZERO_C_K)
        )

    f64_max = validate_local_maxima(
        f64_error,
        edges,
        explicit_points=(RTD_MIN_R, 100.0, RTD_MAX_R),
    )

    calculated = np.array(
        [evaluate_rtd_polynomial(value, power_f32, True) for value in resistances[::20]]
    )
    reference = temperatures[::20] + ZERO_C_K
    errors = calculated.astype(float) - reference

    derivative_coefficients = np.arange(1, len(power_f32)) * power_f32[1:]
    derivative_values = np.polynomial.polynomial.polyval(
        np.linspace(-1.0, 1.0, 1_000_001), derivative_coefficients
    ) * np.float32(RTD_RESISTANCE_SCALE)
    derivative_min = float(np.min(derivative_values))
    if derivative_min <= 0.0:
        raise RuntimeError("Generated Pt100 inverse is not monotonic")

    roundtrip_error = np.max(np.abs(errors))
    return {
        "degree": degree,
        "power_f32": power_f32,
        "f64_max": f64_max,
        "f32_max": f32_max,
        "rms": float(np.sqrt(np.mean(errors**2))),
        "derivative_min": derivative_min,
        "roundtrip": float(roundtrip_error),
    }


def choose_tc_spline(direction, inverse_reference_voltage, inverse_reference_temperature):
    """Choose and validate the smallest acceptable type-K spline candidate.

    Args:
        direction: Either `"forward"` for `K -> V` or `"inverse"` for
            `V -> K`.
        inverse_reference_voltage: Monotonic NIST voltage grid in `V` with
            shape `(n_reference,)`.
        inverse_reference_temperature: Matching absolute-temperature grid in `K`
            with shape `(n_reference,)`.

    Returns:
        Mapping containing span count, `f32` coefficients with shape
        `(intervals + 1,)`, scalar error metrics in `K` or `K`-equivalent, grid
        step, and the minimum derivative.

    Raises:
        RuntimeError: No candidate meets the `0.01 K` error requirement or the
        selected spline is not strictly monotonic.
    """
    if direction == "forward":
        candidates = FORWARD_INTERVAL_CANDIDATES
        value_min = TC_MIN_C + ZERO_C_K
        value_max = TC_MAX_C + ZERO_C_K
        function = lambda temperature_k: nist_voltage_v(temperature_k - ZERO_C_K)
    else:
        candidates = INVERSE_INTERVAL_CANDIDATES
        value_min, value_max = TC_MIN_V, TC_MAX_V
        function = lambda voltage: np.interp(
            voltage, inverse_reference_voltage, inverse_reference_temperature
        )

    validation_values = np.linspace(
        value_min,
        value_max,
        1_000_001,
        dtype=np.float32,
    )
    selected = None
    for intervals in candidates:
        coefficients_f64 = fit_regular_bspline(function, value_min, value_max, intervals)
        coefficients_f32 = coefficients_f64.astype(np.float32)
        spline_f32 = spline_from_coefficients(value_min, value_max, coefficients_f32)
        approximate = evaluate_spline_array(spline_f32, validation_values)
        if direction == "forward":
            approximate_output = np.interp(
                approximate,
                inverse_reference_voltage,
                inverse_reference_temperature,
            )
            dense_error = approximate_output - validation_values
        else:
            reference_output = np.interp(
                validation_values,
                inverse_reference_voltage,
                inverse_reference_temperature,
            )
            dense_error = approximate - reference_output
        if np.max(np.abs(dense_error)) < ACCEPTANCE_ERROR_K:
            selected = (intervals, coefficients_f64, coefficients_f32)
            break
    if selected is None:
        raise RuntimeError(f"No {direction} K-type spline candidate met the error limit")

    intervals, coefficients_f64, coefficients_f32 = selected
    spline_f64 = spline_from_coefficients(value_min, value_max, coefficients_f64)
    spline_f32 = spline_from_coefficients(value_min, value_max, coefficients_f32)
    edges = np.linspace(value_min, value_max, intervals + 1)
    explicit = (ZERO_C_K,) if direction == "forward" else (0.0,)

    def error(interpolator, value):
        approximate = evaluate_spline_scalar(interpolator, value)
        if direction == "forward":
            return nist_temperature_c(approximate) + ZERO_C_K - value
        return approximate - (nist_temperature_c(value) + ZERO_C_K)

    f64_max = validate_local_maxima(
        lambda value: error(spline_f64, value), edges, explicit
    )
    f32_max = validate_local_maxima(
        lambda value: error(spline_f32, value), edges, explicit
    )
    if f64_max[0] > MAX_TEMPERATURE_ERROR_K or f32_max[0] > ACCEPTANCE_ERROR_K:
        raise RuntimeError(
            f"{direction} spline strict error exceeds limit: f64={f64_max}, f32={f32_max}"
        )

    derivative_min = strict_derivative_minimum(value_min, value_max, intervals, spline_f32)
    if derivative_min[0] <= 0.0:
        raise RuntimeError(f"{direction} spline is not monotonic: {derivative_min}")

    approximate = evaluate_spline_array(spline_f32, validation_values)
    if direction == "forward":
        output_error = (
            np.interp(
                approximate,
                inverse_reference_voltage,
                inverse_reference_temperature,
            )
            - validation_values
        )
    else:
        output_error = approximate - np.interp(
            validation_values,
            inverse_reference_voltage,
            inverse_reference_temperature,
        )
    return {
        "intervals": intervals,
        "coefficients_f32": coefficients_f32,
        "step": np.float32(
            (np.float32(value_max) - np.float32(value_min)) / np.float32(intervals)
        ),
        "f64_max": f64_max,
        "f32_max": f32_max,
        "rms": float(np.sqrt(np.mean(output_error.astype(float) ** 2))),
        "derivative_min": derivative_min,
    }


def format_array(name, values, declaration="const", attributes=""):
    """Format one numeric vector as a Rust `f32` declaration.

    Args:
        name: Rust identifier for the generated value.
        values: Numeric values with shape `(n_values,)`.
        declaration: Rust storage keyword, either `const` or `static`.
        attributes: Rust attributes prepended to the declaration.

    Returns:
        Rust source text declaring an `[f32; n_values]` value.
    """
    body = "\n".join(f"    {float(value):.9e}_f32," for value in values)
    return (
        f"{attributes}pub {declaration} {name}: [f32; {len(values)}] = [\n"
        f"{body}\n];\n"
    )


def write_rtd_data(result):
    """Write the selected Pt100 inverse constants to the generated Rust module.

    Args:
        result: Mapping returned by `fit_rtd_inverse`.
    """
    generated = "// @generated by scripts/fit_calcs.py; do not hand edit.\n"
    generated += "/// Minimum fitted Pt100 resistance in `ohm`.\n"
    generated += f"pub const PT100_MIN_RESISTANCE_OHM: f32 = {RTD_MIN_R:.9e}_f32;\n"
    generated += "/// Maximum fitted Pt100 resistance in `ohm`.\n"
    generated += f"pub const PT100_MAX_RESISTANCE_OHM: f32 = {RTD_MAX_R:.9e}_f32;\n"
    generated += "/// Resistance origin in `ohm` for the normalized inverse coordinate.\n"
    generated += (
        f"pub const PT100_RESISTANCE_ORIGIN_OHM: f32 = "
        f"{RTD_RESISTANCE_ORIGIN:.9e}_f32;\n"
    )
    generated += "/// Scale in `1/ohm` for the normalized inverse coordinate.\n"
    generated += (
        f"pub const PT100_RESISTANCE_SCALE: f32 = {RTD_RESISTANCE_SCALE:.9e}_f32;\n"
    )
    generated += "/// Kelvin offset applied after inverse-polynomial evaluation.\n"
    generated += f"pub const PT100_TEMPERATURE_ORIGIN_K: f32 = {ZERO_C_K:.9e}_f32;\n"
    generated += "/// Lower-end inverse tangent in `K/ohm`.\n"
    generated += (
        "pub const PT100_MIN_EXTRAPOLATION_SLOPE_K_PER_OHM: f32 = "
        f"{pt100_inverse_tangent_k_per_ohm(RTD_MIN_C):.9e}_f32;\n"
    )
    generated += "/// Upper-end inverse tangent in `K/ohm`.\n"
    generated += (
        "pub const PT100_MAX_EXTRAPOLATION_SLOPE_K_PER_OHM: f32 = "
        f"{pt100_inverse_tangent_k_per_ohm(RTD_MAX_C):.9e}_f32;\n"
    )
    generated += (
        "/// Ascending normalized-power coefficients in `degC` with shape "
        "`(degree + 1,)`.\n"
    )
    generated += format_array("PT100_INVERSE_COEFFICIENTS_C", result["power_f32"])
    RTD_OUTPUT.write_text(generated)


def write_tc_data(forward, inverse):
    """Write selected forward and inverse type-K spline constants.

    Args:
        forward: Forward-spline mapping returned by `choose_tc_spline`.
        inverse: Inverse-spline mapping returned by `choose_tc_spline`.
    """
    generated = """// @generated by scripts/fit_calcs.py; do not hand edit.\n\
/// Minimum fitted type-K temperature in `K`.\n\
pub const TC_SPLINE_MIN_TEMPERATURE_K: f32 = %.9e_f32;\n\
/// Maximum fitted type-K temperature in `K`.\n\
pub const TC_SPLINE_MAX_TEMPERATURE_K: f32 = %.9e_f32;\n\
/// Minimum fitted type-K equivalent voltage in `V`.\n\
pub const TC_SPLINE_MIN_VOLTAGE_V: f32 = %.9e_f32;\n\
/// Maximum fitted type-K equivalent voltage in `V`.\n\
pub const TC_SPLINE_MAX_VOLTAGE_V: f32 = %.9e_f32;\n\
/// Forward-spline regular-grid step in `K`.\n\
pub const TC_FORWARD_STEP_K: f32 = %.9e_f32;\n\
/// Inverse-spline regular-grid step in `V`.\n\
pub const TC_INVERSE_STEP_V: f32 = %.9e_f32;\n\
""" % (
        TC_MIN_C + ZERO_C_K,
        TC_MAX_C + ZERO_C_K,
        TC_MIN_V,
        TC_MAX_V,
        forward["step"],
        inverse["step"],
    )
    generated += "/// Forward-spline coefficients in `V` with shape `(n_grid,)`.\n"
    generated += format_array(
        "TC_FORWARD_COEFFICIENTS_V",
        forward["coefficients_f32"],
        declaration="static",
        attributes="""#[cfg_attr(
    all(target_os = "none", feature = "tcm"),
    unsafe(link_section = ".dtcm.tc_ktype.tables")
)]
""",
    )
    generated += "/// Inverse-spline coefficients in `K` with shape `(n_grid,)`.\n"
    generated += format_array(
        "TC_INVERSE_COEFFICIENTS_K",
        inverse["coefficients_f32"],
        declaration="static",
        attributes="""#[cfg_attr(
    all(target_os = "none", feature = "tcm"),
    unsafe(link_section = ".dtcm.tc_ktype.tables")
)]
""",
    )
    TC_OUTPUT.write_text(generated)


def write_report(rtd, forward, inverse, roundtrip_error):
    """Write the human-readable fit and validation report.

    Args:
        rtd: Pt100 fit result mapping.
        forward: Forward type-K spline result mapping.
        inverse: Inverse type-K spline result mapping.
        roundtrip_error: Maximum forward/inverse spline round-trip error in `K`.
    """
    REPORT_OUTPUT.write_text(
        f"""# Rev7 calculation fit report

Generated by `fit_calcs.py` from the IEC 60751 Callendar--Van Dusen coefficients
and the NIST ITS-90 type-K reference coefficients (NIST Monograph 175).

## Pt100 global inverse

- Domain: {RTD_MIN_C:.0f} to {RTD_MAX_C:.0f} degC ({RTD_MIN_R:.9f} to {RTD_MAX_R:.9f} ohm)
- Polynomial: one degree-{rtd['degree']} power-basis polynomial, centered at {RTD_RESISTANCE_ORIGIN:.9f} ohm and scaled by {RTD_RESISTANCE_SCALE:.12g} /ohm
- Maximum locally optimized error before f32 reduction: {rtd['f64_max'][0]:.9g} K at {rtd['f64_max'][1]:.9g} ohm
- Maximum locally optimized emitted-f32 error: {rtd['f32_max'][0]:.9g} K at {rtd['f32_max'][1]:.9g} ohm
- Emitted-f32 RMS error: {rtd['rms']:.9g} K
- Minimum polynomial derivative: {rtd['derivative_min']:.9g} K/ohm (strictly positive)
- Maximum forward/inverse round-trip error on the validation sweep: {rtd['roundtrip']:.9g} K
- Outside the standard domain, the evaluator uses the exact CVD inverse endpoint tangent and the fitted endpoint value; continuity is by construction.

## K-type regular cubic B-splines

- Domain: {TC_MIN_C:.0f} to {TC_MAX_C:.0f} degC ({TC_MIN_V:.12g} to {TC_MAX_V:.12g} V)
- Evaluator: `interpn::MultiBsplineRegular` with its zero-third-derivative ghost-coefficient boundary convention and linearized extrapolation.
- Fit: Levenberg--Marquardt over {FIT_SAMPLES_PER_SPAN} samples per span, seeded from the `interpn` spline through exact reference values at its regular grid nodes.
- Forward: {forward['intervals']} spans / {forward['intervals'] + 1} coefficients; f64 maximum {forward['f64_max'][0]:.9g} K-equivalent, emitted-f32 maximum {forward['f32_max'][0]:.9g} K-equivalent, RMS {forward['rms']:.9g} K-equivalent.
- Inverse: {inverse['intervals']} spans / {inverse['intervals'] + 1} coefficients; f64 maximum {inverse['f64_max'][0]:.9g} K, emitted-f32 maximum {inverse['f32_max'][0]:.9g} K, RMS {inverse['rms']:.9g} K.
- Minimum derivatives: forward {forward['derivative_min'][0]:.9g} V/K, inverse {inverse['derivative_min'][0]:.9g} K/V; both are strictly positive.
- Maximum forward/inverse round-trip error on the dense validation grid: {roundtrip_error:.9g} K.

For each regular span, maximum absolute error was sought with 17 seeds and
bounded local optimizations between every neighboring seed. Endpoints, knots,
and the NIST branch point were checked explicitly. Both the fitted-f64 and the
operation-rounded emitted-f32 evaluators were checked. The acceptance threshold
is {ACCEPTANCE_ERROR_K:.4f} K, leaving numerical margin below the {MAX_TEMPERATURE_ERROR_K:.2f} K requirement.
"""
    )


def main():
    """Fit, validate, and emit all shared rev7 engineering conversions."""
    inverse_temperature_c = np.linspace(TC_MIN_C, TC_MAX_C, 2_000_001)
    inverse_temperature_k = inverse_temperature_c + ZERO_C_K
    inverse_voltage = nist_voltage_v(inverse_temperature_c)

    rtd = fit_rtd_inverse()
    forward = choose_tc_spline(
        "forward", inverse_voltage, inverse_temperature_k
    )
    inverse = choose_tc_spline(
        "inverse", inverse_voltage, inverse_temperature_k
    )

    forward_spline = spline_from_coefficients(
        TC_MIN_C + ZERO_C_K,
        TC_MAX_C + ZERO_C_K,
        forward["coefficients_f32"],
    )
    inverse_spline = spline_from_coefficients(
        TC_MIN_V,
        TC_MAX_V,
        inverse["coefficients_f32"],
    )
    roundtrip_temperatures = np.linspace(
        TC_MIN_C + ZERO_C_K,
        TC_MAX_C + ZERO_C_K,
        1_000_001,
        dtype=np.float32,
    )
    forward_voltage = evaluate_spline_array(forward_spline, roundtrip_temperatures)
    roundtrip = evaluate_spline_array(inverse_spline, forward_voltage)
    roundtrip_error = float(np.max(np.abs(roundtrip - roundtrip_temperatures)))

    write_rtd_data(rtd)
    write_tc_data(forward, inverse)
    write_report(rtd, forward, inverse, roundtrip_error)

    print(
        f"Pt100 degree={rtd['degree']}, max f32 error={rtd['f32_max'][0]:.9g} K"
    )
    print(
        f"K forward coefficients={forward['intervals'] + 1}, "
        f"max f32 error={forward['f32_max'][0]:.9g} K"
    )
    print(
        f"K inverse coefficients={inverse['intervals'] + 1}, "
        f"max f32 error={inverse['f32_max'][0]:.9g} K"
    )
    print(f"wrote {RTD_OUTPUT}, {TC_OUTPUT}, and {REPORT_OUTPUT}")


if __name__ == "__main__":
    main()

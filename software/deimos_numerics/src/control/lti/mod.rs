//! Linear time-invariant system analysis and alternate SISO representations.
//!
//! The broader control module is built around numerical primitives such as
//! Lyapunov/Stein solvers and balanced truncation. This `lti` layer sits above
//! that foundation and groups the user-facing model-analysis and
//! transfer-function-style APIs that are specifically about linear
//! time-invariant systems.
//!
//! The implementation is dense-first overall, with selected sparse state-space
//! workflows available:
//!
//! - dense state-space analysis lives in `analysis`
//! - dense sampled responses live in `response`
//! - sampled response metrics live in `response_metrics`
//! - fixed-timestep digital filtering helpers live in `sim`
//! - real-coefficient SISO alternate representations live in
//!   `transfer_function`, `zpk`, and `sos`
//! - explicit continuous delay-aware process models live in `process_models`
//! - classical loop-analysis helpers live in `loop_analysis` and `root_locus`
//! - IIR design and order-selection helpers live in `filter_design`
//! - sparse CSC-backed state-space models support transfer evaluation,
//!   frequency response, and discrete-time simulation through the same
//!   conceptual API
//!
//! Broader sparse/operator-backed analysis, especially continuous-time
//! matrix-function actions and large-scale stability diagnostics, still belongs
//! beyond the scope of this layer until the required Krylov and
//! matrix-function machinery is added.
//!
//! # Two Intuitions
//!
//! 1. **Signal-flow view.** This is the part of the crate that answers
//!    user-facing system questions: What are the poles? What does the step
//!    response look like? What happens if I cascade two filters? How do I run
//!    `filtfilt` on sampled data?
//! 2. **Representation view.** This is also the interoperability layer between
//!    several mathematically equivalent descriptions of the same SISO system:
//!    state space, transfer function, zero-pole-gain, second-order sections,
//!    and delay-aware process models.
//!
//! # Glossary
//!
//! - **DC gain:** Steady-state gain, evaluated at `s = 0` or `z = 1`.
//! - **SOS:** Second-order sections, the canonical cascade form for designing,
//!   storing, and composing realized IIR filters.
//! - **Delta-SOS:** A derived discrete-time execution form of SOS, used when
//!   low normalized cutoffs make ordinary section recurrences ill-conditioned
//!   near `z = 1`.
//! - **ZPK:** Zero-pole-gain representation.
//! - **`S`, `T`, `KS`, `PS`:** Classical loop-sensitivity channels.
//! - **Bode / Nyquist / Nichols:** Standard frequency-domain plotting data.
//! - **FOPDT / SOPDT:** First-/second-order-plus-dead-time process models.
//!
//! # Mathematical Formulation
//!
//! The layer revolves around transfer maps:
//!
//! - continuous: `G(s) = C (s I - A)^-1 B + D`
//! - discrete: `G(z) = C (z I - A)^-1 B + D`
//!
//! SISO alternate representations encode the same transfer map in different
//! coordinates:
//!
//! - polynomial ratio form (`TransferFunction`)
//! - zero/pole/root form (`Zpk`)
//! - sectioned factorization (`Sos`)
//! - delta-operator execution form of a discrete sectioned factorization
//!   (`DeltaSos`)
//! - explicit delay process models (`FopdtModel`, `SopdtModel`)
//!
//! # Implementation Notes
//!
//! - Dense state space is the most complete representation and often serves as
//!   the bridge between alternate forms.
//! - `Sos` is the canonical realized IIR representation for design,
//!   storage, conversion, and algebra.
//! - `DeltaSos` is a derived discrete runtime basis for the same transfer map,
//!   intended specifically for low-cutoff execution where ordinary SOS
//!   coefficients approach tiny perturbations of `[1, -2, 1]`.
//! - Digital runtime filtering is intentionally implemented only on
//!   `DiscreteStateSpace`, `DiscreteSos`, `DeltaSos`, and `Fir`, which are the
//!   numerically credible execution forms.
//! - Frequency-domain helper surfaces are generally sampled-grid based rather
//!   than symbolic.
//! - Continuous delay remains explicit in dedicated process-model types rather
//!   than being folded into rational transfer functions.
//!
//! # Feature Matrix
//!
//! | Feature | Dense continuous | Dense discrete | Sparse continuous | Sparse discrete | TF/ZPK/SOS/FIR |
//! | --- | --- | --- | --- | --- | --- |
//! | State-space modeling | yes | yes | yes | yes | n/a |
//! | Pole / stability analysis | yes | yes | partial | partial | yes (SISO) |
//! | DC gain / transfer evaluation | yes | yes | yes | yes | yes |
//! | Time-domain simulation | yes | yes | partial | yes | FIR yes, SOS yes, Delta-SOS yes, TF/ZPK via conversion |
//! | Response metrics | yes | yes | no | no | yes (SISO) |
//! | Loop analysis (`S`, `T`, margins, Nyquist, Nichols) | yes (SISO) | yes (SISO) | no | no | yes (SISO) |
//! | Root locus | yes (SISO) | yes (SISO) | no | no | yes (SISO) |
//! | IIR filter design | analog only | digital only | n/a | n/a | yes |
//! | FIR / Savitzky-Golay | no | yes | n/a | n/a | FIR only |
//! | Explicit delay-aware process models | yes | limited | no | no | process-model types |

mod analysis;
mod delta_sos;
mod error;
mod filter_design;
mod fir;
mod loop_analysis;
mod plot_data;
mod process_models;
mod response;
mod response_metrics;
mod root_locus;
mod sim;
mod sos;
pub mod state_space;
mod transfer_function;
mod util;
mod zpk;

pub use delta_sos::{DeltaSection, DeltaSos};
pub use error::LtiError;
pub use filter_design::{
    AnalogFilterFamily, AnalogFilterSpec, AnalogOrderSelectionSpec, ButterworthOrderResult,
    Chebyshev1OrderResult, DigitalFilterFamily, DigitalFilterSpec, DigitalOrderSelectionSpec,
    FilterDesignError, FilterShape, buttord_analog, buttord_digital, cheb1ord_analog,
    cheb1ord_digital, design_analog_filter_sos, design_analog_filter_tf, design_analog_filter_zpk,
    design_digital_filter_sos, design_digital_filter_tf, design_digital_filter_zpk,
};
pub use fir::{Fir, FirFilterState, SavGolSpec, design_savgol};
pub use loop_analysis::{LoopCrossovers, LoopMargins, NicholsData, NyquistData};
pub use plot_data::{BodeData, PoleZeroData};
pub use process_models::{
    FopdtModel, FopdtStepResponseJacobian, SopdtModel, SopdtStepResponseJacobian,
};
pub use response::{
    ContinuousImpulseResponse, ContinuousSimulation, DiscreteSimulation, SampledResponse,
};
pub use response_metrics::{StepResponseMetricParams, StepResponseMetrics};
pub use root_locus::{RootLocusBranch, RootLocusData};
pub use sim::{
    DeltaSosFilterState, FiltFiltPadLen, FiltFiltPadMode, FiltFiltParams, FilteredSignal,
    SosFilterState, StatefulFilteredSignal,
};
pub use sos::{ContinuousSos, DiscreteSos, SecondOrderSection, Sos};
pub use transfer_function::{
    ContinuousTransferFunction, DiscreteTransferFunction, TransferFunction,
};
pub use zpk::{ContinuousZpk, DiscreteZpk, Zpk};

pub use state_space::{
    ContinuousStateSpace, ContinuousTime, DiscreteStateSpace, DiscreteTime,
    SparseContinuousStateSpace, SparseDiscreteStateSpace, SparseStateSpace, StateSpace,
    StateSpaceError,
};

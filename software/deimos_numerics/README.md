# deimos_numerics

Numerical methods and control systems analysis.

## Feature Status

| Feature area | Sparse | Dense | Continuous | Discrete | no-std | no-alloc | wasm |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Linear solves | ✅ | ✅ | n/a | n/a | ✅ | n/a | ✅ |
| Matrix decompositions | 💡 | ✅ | n/a | n/a | ✅ | n/a | ✅ |
| Matrix equations | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| LTI models and analysis | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Filter design | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Control synthesis | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Estimation | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |
| Embedded fixed runtime | n/a | ✅ | n/a | ✅ | ✅ | ✅ | ✅ |
| Identification and realization | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |
| Model reduction | n/a | ✅ | ✅ | 💡 | ✅ | n/a | ✅ |

## Numerical Methods

### Linear solves

| Feature | Sparse | Dense | Continuous | Discrete | no-std | no-alloc | wasm |
| --- | --- | --- | --- | --- | --- | --- | --- |
| LU factorization | ✅ | ✅ | n/a | n/a | ✅ | n/a | ✅ |
| Iterative refinement | ✅ | n/a | n/a | n/a | ✅ | n/a | ✅ |
| Cholesky / LDLT | ✅ | ✅ | n/a | n/a | ✅ | n/a | ✅ |
| BiCGSTAB | ✅ | n/a | n/a | n/a | ✅ | n/a | ✅ |
| Equilibration | ✅ | 💡 | n/a | n/a | ✅ | n/a | ✅ |
| Preconditioners | ✅ | n/a | n/a | n/a | ✅ | n/a | ✅ |
| Schur-complement operators | ✅ | n/a | n/a | n/a | ✅ | n/a | ✅ |

### Matrix decompositions

| Feature | Sparse | Dense | Continuous | Discrete | no-std | no-alloc | wasm |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Eigenvalues and eigenvectors | ✅ | ✅ | n/a | n/a | ✅ | n/a | ✅ |
| Self-adjoint eigen decomposition | ✅ | ✅ | n/a | n/a | ✅ | n/a | ✅ |
| Generalized eigen decomposition | 💡 | ✅ | n/a | n/a | ✅ | n/a | ✅ |
| Singular value decomposition | ✅ | ✅ | n/a | n/a | ✅ | n/a | ✅ |
| Matrix-free operator adapters | ✅ | n/a | n/a | n/a | ✅ | n/a | ✅ |
| Convergence diagnostics | ✅ | ✅ | n/a | n/a | ✅ | n/a | ✅ |

## Control Systems & Digital Signal Processing

### Embedded fixed runtime

| Feature | Sparse | Dense | Continuous | Discrete | no-std | no-alloc | wasm |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Fixed-size matrix / vector storage | n/a | ✅ | n/a | ✅ | ✅ | ✅ | ✅ |
| Fixed discrete state-space runtime | n/a | ✅ | n/a | ✅ | ✅ | ✅ | ✅ |
| Fixed FIR runtime | n/a | ✅ | n/a | ✅ | ✅ | ✅ | ✅ |
| Fixed delta-SOS runtime | n/a | ✅ | n/a | ✅ | ✅ | ✅ | ✅ |
| Fixed PID runtime | n/a | ✅ | n/a | ✅ | ✅ | ✅ | ✅ |
| Fractional-delay FIR taps | n/a | ✅ | n/a | ✅ | ✅ | ✅ | ✅ |
| Fixed discrete Kalman filter | n/a | ✅ | n/a | ✅ | ✅ | ✅ | ✅ |
| Fixed steady-state Kalman filter | n/a | ✅ | n/a | ✅ | ✅ | ✅ | ✅ |

### Matrix equations

| Feature | Sparse | Dense | Continuous | Discrete | no-std | no-alloc | wasm |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Lyapunov solve | ✅ | ✅ | ✅ | n/a | ✅ | n/a | ✅ |
| Stein solve | ✅ | ✅ | n/a | ✅ | ✅ | n/a | ✅ |
| Controllability Gramians | ✅ | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Observability Gramians | ✅ | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Algebraic Riccati equations | 💡 | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |

### LTI models and analysis

| Feature | Sparse | Dense | Continuous | Discrete | no-std | no-alloc | wasm |
| --- | --- | --- | --- | --- | --- | --- | --- |
| State-space models | ✅ | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Transfer functions | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Zero-pole-gain models | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Second-order sections | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| FIR models | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |
| Pole and stability analysis | ✅ | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Frequency-response data | ✅ | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Step, impulse, and simulation data | 💡 | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Loop margins / Nyquist / Nichols | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Root locus | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Explicit process models | n/a | ✅ | ✅ | 💡 | ✅ | n/a | ✅ |

### Filter design

| Feature | Sparse | Dense | Continuous | Discrete | no-std | no-alloc | wasm |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Butterworth IIR design | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Chebyshev I IIR design | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Bessel IIR design | n/a | ✅ | ✅ | n/a | ✅ | n/a | ✅ |
| Lowpass / highpass transforms | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Bandpass / bandstop transforms | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Minimum-order selection | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| SOS output | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Transfer-function output | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Savitzky-Golay FIR design | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |
| Delta-SOS runtime form | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |

### Control synthesis

| Feature | Sparse | Dense | Continuous | Discrete | no-std | no-alloc | wasm |
| --- | --- | --- | --- | --- | --- | --- | --- |
| LQR / DLQR | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| LQG / DLQG | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Pole placement | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| PID runtime | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |
| Frequency-domain PID design | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Process-model PID design | n/a | ✅ | ✅ | n/a | ✅ | n/a | ✅ |
| Step-response PID design | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |
| OKID / ERA PID design | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |

### Estimation

| Feature | Sparse | Dense | Continuous | Discrete | no-std | no-alloc | wasm |
| --- | --- | --- | --- | --- | --- | --- | --- |
| LQE / DLQE design | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Fixed-gain observer runtime | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Time-varying Kalman filter | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |
| Steady-state Kalman filter | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |
| EKF runtime | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |
| UKF runtime | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |

### Identification and realization

| Feature | Sparse | Dense | Continuous | Discrete | no-std | no-alloc | wasm |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Markov sequences | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |
| Block-Hankel assembly | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |
| Shifted Hankel pairs | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |
| ERA from Markov data | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |
| ERA from shifted Hankel data | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |
| OKID from sampled I/O data | n/a | ✅ | n/a | ✅ | ✅ | n/a | ✅ |

### Model reduction

| Feature | Sparse | Dense | Continuous | Discrete | no-std | no-alloc | wasm |
| --- | --- | --- | --- | --- | --- | --- | --- |
| HSVD from dense Gramians | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| HSVD from low-rank factors | ✅ | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Balanced realization | n/a | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Balanced truncation | ✅ | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Reduced-order diagnostics | ✅ | ✅ | ✅ | ✅ | ✅ | n/a | ✅ |
| Sparse state-space reduction front end | 💡 | n/a | 💡 | 💡 | ✅ | n/a | ✅ |

## License

Licensed under either of

* Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

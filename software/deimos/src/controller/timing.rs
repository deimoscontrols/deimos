//! Control for maintaining time sync between the controller and peripherals

/// PID controller for correcting peripheral cycle timing
/// with simple saturation to prevent excessive integral windup.
///
/// Because the true error is known to be almost exactly linear
/// after the initial discrepancy is resolved, the windup saturation
/// limit can be chosen to be compatible with the peripheral clock error
/// without driving excessive overshoot.
pub struct TimingPID {
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,

    pub v: f64,
    pub integral: f64,
    pub max_integral: f64,
}

impl TimingPID {
    /// Get the next output from the controller given an observed
    /// timing error value.
    ///
    /// The output splits the phase (PD) and period (I)
    /// portions so that the steady rate correction
    /// can be preserved by the peripheral if it misses a
    /// packet from the controller.
    pub fn update(&mut self, v: f64) -> (f64, f64) {
        let d = v - self.v;

        self.integral += v;
        self.integral = self.integral.max(-self.max_integral).min(self.max_integral);
        self.v = v;

        let out_phase = self.kp * v + self.kd * d;
        let out_period = self.ki * self.integral;

        return (out_period, out_phase);
    }
}

/// One-pole low-pass smoother for click-free parameter changes.
///
/// `current` decays toward `target` at a rate set by the time constant in ms.
/// Audio-thread safe: no allocations, no locks.
#[derive(Clone, Copy)]
pub struct SmoothedParam {
    target: f32,
    current: f32,
    coef: f32,
}

impl SmoothedParam {
    pub fn new(initial: f32, sample_rate: f32, time_constant_ms: f32) -> Self {
        let tau = (time_constant_ms.max(0.01)) * 0.001;
        let coef = (-1.0 / (sample_rate * tau)).exp();
        Self { target: initial, current: initial, coef }
    }

    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    pub fn snap(&mut self, value: f32) {
        self.target = value;
        self.current = value;
    }

    /// Advance by one sample, return the new value.
    #[inline]
    pub fn next(&mut self) -> f32 {
        self.current = self.target + (self.current - self.target) * self.coef;
        self.current
    }

    pub fn current(&self) -> f32 {
        self.current
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    /// Reset coefficient when sample rate changes.
    pub fn reconfigure(&mut self, sample_rate: f32, time_constant_ms: f32) {
        let tau = (time_constant_ms.max(0.01)) * 0.001;
        self.coef = (-1.0 / (sample_rate * tau)).exp();
    }
}

#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approaches_target_monotonically() {
        let mut p = SmoothedParam::new(0.0, 48_000.0, 10.0);
        p.set_target(1.0);
        let mut prev = 0.0;
        for _ in 0..10_000 {
            let v = p.next();
            assert!(v >= prev - 1e-6);
            prev = v;
        }
        assert!((p.current() - 1.0).abs() < 1e-3);
    }

    #[test]
    fn snap_jumps_immediately() {
        let mut p = SmoothedParam::new(0.0, 48_000.0, 10.0);
        p.snap(0.5);
        assert_eq!(p.next(), 0.5);
    }
}

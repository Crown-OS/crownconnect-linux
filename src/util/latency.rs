use std::time::Duration;

/// A bounded window of latency samples, summarised as median and 99th percentile.
#[derive(Debug, Clone)]
pub struct LatencyWindow {
    samples_us: Vec<u32>,
    capacity: usize,
    next: usize,
}

/// The median and 99th percentile of a [`LatencyWindow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatencySummary {
    pub median: Duration,
    pub p99: Duration,
    pub samples: usize,
}

impl LatencyWindow {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            samples_us: Vec::with_capacity(capacity),
            capacity: capacity.max(1),
            next: 0,
        }
    }

    pub fn record(&mut self, latency: Duration) {
        let micros = u32::try_from(latency.as_micros()).unwrap_or(u32::MAX);
        if self.samples_us.len() < self.capacity {
            self.samples_us.push(micros);
        } else if let Some(slot) = self.samples_us.get_mut(self.next) {
            *slot = micros;
        }
        self.next = (self.next + 1) % self.capacity;
    }

    pub fn summary(&self) -> Option<LatencySummary> {
        let mut sorted = self.samples_us.clone();
        sorted.sort_unstable();
        let at = |percent: usize| {
            let index = (sorted.len() * percent).div_ceil(100).saturating_sub(1);
            sorted
                .get(index)
                .map(|&micros| Duration::from_micros(u64::from(micros)))
        };
        Some(LatencySummary {
            median: at(50)?,
            p99: at(99)?,
            samples: sorted.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarises_median_and_p99() {
        let mut window = LatencyWindow::with_capacity(100);
        (1..=100).for_each(|ms| window.record(Duration::from_millis(ms)));
        let summary = window.summary();
        assert_eq!(summary.map(|s| s.median), Some(Duration::from_millis(50)));
        assert_eq!(summary.map(|s| s.p99), Some(Duration::from_millis(99)));
    }

    #[test]
    fn overwrites_oldest_samples_when_full() {
        let mut window = LatencyWindow::with_capacity(2);
        [1, 2, 30]
            .map(Duration::from_millis)
            .into_iter()
            .for_each(|d| window.record(d));
        assert_eq!(
            window.summary().map(|s| s.p99),
            Some(Duration::from_millis(30))
        );
        assert_eq!(window.summary().map(|s| s.samples), Some(2));
    }

    #[test]
    fn empty_window_has_no_summary() {
        assert_eq!(LatencyWindow::with_capacity(4).summary(), None);
    }
}

//! Latency histograms (microseconds) and the markdown report.

#[derive(Default, Clone)]
pub struct Hist(pub Vec<u32>);

#[derive(Default, Clone, Copy)]
pub struct Summary {
    pub n: usize,
    pub p50: u32,
    pub p95: u32,
    pub p99: u32,
    pub max: u32,
    pub mean: u32,
}

impl Hist {
    pub fn push(&mut self, us: u64) {
        self.0.push(us.min(u64::from(u32::MAX)) as u32);
    }

    pub fn summary(&self) -> Summary {
        if self.0.is_empty() {
            return Summary::default();
        }
        let mut v = self.0.clone();
        v.sort_unstable();
        let at = |p: f64| v[(((v.len() - 1) as f64) * p).round() as usize];
        let sum: u64 = v.iter().map(|&x| u64::from(x)).sum();
        Summary {
            n: v.len(),
            p50: at(0.50),
            p95: at(0.95),
            p99: at(0.99),
            max: *v.last().expect("non-empty"),
            mean: (sum / v.len() as u64) as u32,
        }
    }
}

fn ms(us: u32) -> String {
    format!("{:.2}", f64::from(us) / 1000.0)
}

pub fn table_header() -> &'static str {
    "| stage | n | mean ms | p50 ms | p95 ms | p99 ms | max ms |\n|---|---:|---:|---:|---:|---:|---:|"
}

pub fn table_row(name: &str, h: &Hist) -> String {
    let s = h.summary();
    format!("| {name} | {} | {} | {} | {} | {} | {} |", s.n, ms(s.mean), ms(s.p50), ms(s.p95), ms(s.p99), ms(s.max))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let mut h = Hist::default();
        for i in 1..=100u64 {
            h.push(i * 100);
        }
        let s = h.summary();
        assert_eq!(s.n, 100);
        assert_eq!(s.max, 10_000);
        assert!((s.p50 as i64 - 5_000).abs() <= 100);
        assert!((s.p99 as i64 - 9_900).abs() <= 100);
        assert_eq!(Hist::default().summary().n, 0);
    }
}

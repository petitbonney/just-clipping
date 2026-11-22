//! Per-second signal helpers shared by the analysis steps.

/// Number of one-second bins covering `duration_s`.
pub fn bins(duration_s: f64) -> usize {
    duration_s.max(0.0).ceil() as usize
}

/// Centered moving average over `window` seconds (odd windows are exact).
pub fn smooth(values: &[f64], window: usize) -> Vec<f64> {
    if window <= 1 || values.is_empty() {
        return values.to_vec();
    }
    let half = window / 2;
    let mut prefix = Vec::with_capacity(values.len() + 1);
    prefix.push(0.0);
    for v in values {
        prefix.push(prefix.last().unwrap() + v);
    }
    (0..values.len())
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(values.len());
            (prefix[hi] - prefix[lo]) / (hi - lo) as f64
        })
        .collect()
}

/// Centered rolling median over `window` seconds. Used as the "normal
/// level" a moment is compared against.
pub fn rolling_median(values: &[f64], window: usize) -> Vec<f64> {
    let half = window / 2;
    let mut sorted: Vec<f64> = Vec::new();
    let mut out = Vec::with_capacity(values.len());
    let (mut lo, mut hi) = (0usize, 0usize);
    for i in 0..values.len() {
        let want_lo = i.saturating_sub(half);
        let want_hi = (i + half + 1).min(values.len());
        while hi < want_hi {
            let v = values[hi];
            let pos = sorted.partition_point(|x| *x < v);
            sorted.insert(pos, v);
            hi += 1;
        }
        while lo < want_lo {
            let v = values[lo];
            let pos = sorted.partition_point(|x| *x < v);
            sorted.remove(pos);
            lo += 1;
        }
        let n = sorted.len();
        out.push(if n % 2 == 1 {
            sorted[n / 2]
        } else {
            (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
        });
    }
    out
}

pub fn clamp01(v: f64) -> f64 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Maps a ratio to 0..1 on a log scale: 1x (normal) → 0, `full`x → 1.
pub fn ratio_score(ratio: f64, full: f64) -> f64 {
    if ratio <= 1.0 {
        return 0.0;
    }
    clamp01(ratio.ln() / full.ln())
}

/// Rounds for compact JSON.
pub fn round(values: &[f64], decimals: i32) -> Vec<f64> {
    let k = 10f64.powi(decimals);
    values.iter().map(|v| (v * k).round() / k).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smooths_with_edges() {
        let s = smooth(&[0.0, 3.0, 0.0, 0.0], 3);
        assert_eq!(s, vec![1.5, 1.0, 1.0, 0.0]);
        assert_eq!(smooth(&[1.0, 2.0], 1), vec![1.0, 2.0]);
    }

    #[test]
    fn rolling_median_matches_naive() {
        let values: Vec<f64> = (0..200).map(|i| ((i * 37) % 23) as f64).collect();
        let fast = rolling_median(&values, 31);
        for (i, got) in fast.iter().enumerate() {
            let lo = i.saturating_sub(15);
            let hi = (i + 16).min(values.len());
            let mut w = values[lo..hi].to_vec();
            w.sort_by(f64::total_cmp);
            let n = w.len();
            let want = if n % 2 == 1 {
                w[n / 2]
            } else {
                (w[n / 2 - 1] + w[n / 2]) / 2.0
            };
            assert_eq!(*got, want, "at {i}");
        }
    }

    #[test]
    fn ratio_scores() {
        assert_eq!(ratio_score(0.5, 8.0), 0.0);
        assert_eq!(ratio_score(1.0, 8.0), 0.0);
        assert!((ratio_score(8.0, 8.0) - 1.0).abs() < 1e-9);
        assert_eq!(ratio_score(100.0, 8.0), 1.0);
        assert!((ratio_score(2.0, 8.0) - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn bins_round_up() {
        assert_eq!(bins(40.0), 40);
        assert_eq!(bins(40.2), 41);
        assert_eq!(bins(-1.0), 0);
    }
}

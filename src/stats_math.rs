pub fn cpu_pct(cpu_total: u64, precpu_total: u64, system: u64, presystem: u64, online: u64) -> f64 {
    let cpu_delta = cpu_total.saturating_sub(precpu_total) as f64;
    let system_delta = system.saturating_sub(presystem) as f64;
    if system_delta <= 0.0 || online == 0 {
        return 0.0;
    }
    (cpu_delta / system_delta) * online as f64 * 100.0
}

pub fn mem_used(usage: u64, inactive_file: u64) -> u64 {
    usage.saturating_sub(inactive_file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_percentage_matches_docker_formula() {
        // 1 full core busy over the interval on a 4-core box.
        let pct = cpu_pct(
            2_000_000_000,
            1_000_000_000,
            8_000_000_000,
            4_000_000_000,
            4,
        );
        assert!((pct - 100.0).abs() < 0.001, "got {pct}");
    }

    #[test]
    fn cpu_zero_system_delta_is_zero() {
        assert_eq!(cpu_pct(10, 5, 100, 100, 4), 0.0);
    }

    #[test]
    fn mem_subtracts_inactive_file() {
        assert_eq!(mem_used(1000, 400), 600);
        assert_eq!(mem_used(300, 400), 0);
    }
}

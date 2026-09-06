use std::{collections::BTreeMap, time::Instant};

pub fn resources() -> serde_json::Value {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let values: BTreeMap<_, _> = status.lines().filter_map(|l| l.split_once(':')).collect();
    let value = |key| {
        values
            .get(key)
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.parse::<u64>().ok())
    };
    let pss = std::fs::read_to_string("/proc/self/smaps_rollup")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|l| {
                l.strip_prefix("Pss:")
                    .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            })
        });
    let io = std::fs::read_to_string("/proc/self/io").unwrap_or_default();
    let io: BTreeMap<_, _> = io
        .lines()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k, v.trim().parse::<u64>().unwrap_or(0)))
        .collect();
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // Read this benchmark process's counters only.
    let usage = unsafe {
        assert_eq!(libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()), 0);
        usage.assume_init()
    };
    serde_json::json!({"rss_kib":value("VmRSS"),"peak_rss_kib":value("VmHWM"),"pss_kib":pss,
        "threads":value("Threads"),"fds":std::fs::read_dir("/proc/self/fd").unwrap().count(),
        "minor_faults":usage.ru_minflt,"major_faults":usage.ru_majflt,
        "voluntary_context_switches":usage.ru_nvcsw,"involuntary_context_switches":usage.ru_nivcsw,
        "read_bytes":io.get("read_bytes"),"write_bytes":io.get("write_bytes")})
}

pub fn sample(name: &str, values: &[f64]) {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    assert!(n > 0);
    let percentile = |p: f64| {
        sorted[((n as f64 * p).ceil() as usize)
            .saturating_sub(1)
            .min(n - 1)]
    };
    let median = if n.is_multiple_of(2) {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    } else {
        sorted[n / 2]
    };
    println!(
        "{}",
        serde_json::json!({"metric":name,"unit":"ms","n":n,"min":sorted[0],
        "median":median,"p95":percentile(0.95),"max":sorted[n-1],"mean":sorted.iter().sum::<f64>()/n as f64,"samples_ms":values})
    );
}

pub fn elapsed(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

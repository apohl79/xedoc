use std::time::Duration;

/// Convert a [`std::time::Duration`] into a human-readable, compact string.
///
/// Formatting rules:
/// * < 1 s  ->  "{milli}ms"
/// * < 60 s ->  "{sec:.2}s" (two decimal places)
/// * >= 60 s ->  "{min}m {sec:02}s"
pub fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis() as i64;
    format_elapsed_millis(millis)
}

fn format_elapsed_millis(millis: i64) -> String {
    if millis < 1000 {
        format!("{millis}ms")
    } else if millis < 60_000 {
        format!("{:.2}s", millis as f64 / 1000.0)
    } else {
        let minutes = millis / 60_000;
        let seconds = (millis % 60_000) / 1000;
        format!("{minutes}m {seconds:02}s")
    }
}

use std::time::{SystemTime, UNIX_EPOCH};
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub fn parse_duration_seconds(value: &str) -> anyhow::Result<i64> {
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!("duration is empty");
    }
    let (number, unit) = value.split_at(
        value
            .find(|ch: char| !ch.is_ascii_digit())
            .unwrap_or(value.len()),
    );
    let amount = number.parse::<i64>()?;
    let multiplier = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        other => anyhow::bail!("unsupported duration unit {other:?}; use s, m, h, or d"),
    };
    Ok(amount.saturating_mul(multiplier))
}

use rand::rngs::OsRng;
use rand::Rng;

/// Now, in Unix seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// `len` random lowercase hex characters, from the OS's CSPRNG.
pub fn random_hex(len: usize) -> String {
    let bytes: Vec<u8> = (0..len.div_ceil(2)).map(|_| OsRng.r#gen()).collect();
    let mut hex = hex::encode(bytes);
    hex.truncate(len);
    hex
}

/// A gift card code, `XXXX-XXXX-XXXX-XXXX` in A–Z and 0–9: about 82 bits.
pub fn giftcard_code() -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut code = String::with_capacity(19);
    for i in 0..16 {
        if i > 0 && i % 4 == 0 {
            code.push('-');
        }
        code.push(CHARS[OsRng.gen_range(0..CHARS.len())] as char);
    }
    code
}

const DAY: i64 = 86_400;

/// Midnight UTC on the day of `t`.
pub fn day_start(t: i64) -> i64 {
    t.div_euclid(DAY) * DAY
}

/// Midnight UTC on the Monday of the week of `t`. 1970-01-01 was a Thursday.
pub fn week_start(t: i64) -> i64 {
    let days = t.div_euclid(DAY);
    (days - (days + 3).rem_euclid(7)) * DAY
}

/// Midnight UTC on the 1st of the month of `t`.
pub fn month_start(t: i64) -> i64 {
    // The day of the month, by Howard Hinnant's `civil_from_days`.
    let days = t.div_euclid(DAY);
    let z = days + 719_468;
    let doe = z - z.div_euclid(146_097) * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day_of_month = doy - (153 * mp + 2) / 5 + 1;
    (days - (day_of_month - 1)) * DAY
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_period_starts() {
        // 2026-11-19 15:04:05 UTC, a Thursday.
        let t = 1_795_100_645;
        assert_eq!(super::day_start(t), 1_795_046_400);
        // Monday 2026-11-16.
        assert_eq!(super::week_start(t), 1_794_787_200);
        // 2026-11-01.
        assert_eq!(super::month_start(t), 1_793_491_200);
        // 2024-02-29, a leap day, and a Thursday.
        assert_eq!(super::month_start(1_709_211_600), 1_706_745_600);
        assert_eq!(super::week_start(1_709_211_600), 1_708_905_600);
        assert_eq!(super::week_start(super::week_start(t)), super::week_start(t));
        assert_eq!(super::month_start(super::month_start(t)), super::month_start(t));
    }

    #[test]
    fn makes_codes() {
        let code = super::giftcard_code();
        assert_eq!(code.len(), 19);
        assert_eq!(code.matches('-').count(), 3);
        assert_eq!(super::random_hex(12).len(), 12);
    }
}

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

#[cfg(test)]
mod tests {
    #[test]
    fn makes_codes() {
        let code = super::giftcard_code();
        assert_eq!(code.len(), 19);
        assert_eq!(code.matches('-').count(), 3);
        assert_eq!(super::random_hex(12).len(), 12);
    }
}

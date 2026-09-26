use rustix::rand::{getrandom, GetRandomFlags};

/// `bytes` random bytes from the kernel, as lowercase hex.
///
/// # Errors
///
/// Fails only if the kernel's random source is unavailable.
pub fn random_hex(bytes: usize) -> std::io::Result<String> {
    let mut buffer = vec![0_u8; bytes];
    fill(&mut buffer)?;
    Ok(buffer.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// A random `u32` from the kernel.
///
/// # Errors
///
/// Fails only if the kernel's random source is unavailable.
pub fn random_u32() -> std::io::Result<u32> {
    let mut buffer = [0_u8; 4];
    fill(&mut buffer)?;
    Ok(u32::from_ne_bytes(buffer))
}

fn fill(buffer: &mut [u8]) -> std::io::Result<()> {
    let mut filled = 0;
    while let Some(rest) = buffer.get_mut(filled..).filter(|rest| !rest.is_empty()) {
        filled += getrandom(rest, GetRandomFlags::empty())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_distinct_hex_of_the_requested_length() -> std::io::Result<()> {
        let first = random_hex(8)?;
        assert_eq!(first.len(), 16);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, random_hex(8)?);
        Ok(())
    }
}

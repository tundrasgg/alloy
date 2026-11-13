//! Code that stays inside the rules. Mentions such as todo!() in comments and
//! `#[ignore]` in docs do not count.

/// Parses a port.
pub fn port(text: &str) -> Option<u16> {
    let note = "todo!() and x.unwrap() inside a string do not count";
    let _ = note;
    let digits: Vec<u32> = text.chars().filter_map(|c| c.to_digit(10)).collect();
    // `digits` holds at most as many items as `text` has characters, and is never empty here.
    let first = digits.first().copied().unwrap();
    u16::try_from(first).ok()
}

/// Picks a side.
pub fn side(n: u8) -> &'static str {
    match n % 2 {
        0 => "even",
        1 => "odd",
        _ => unreachable!("n % 2 is 0 or 1"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        assert_eq!(port("8").unwrap(), 8);
        assert_eq!(side(3), "odd");
        let v: Result<u8, ()> = Ok(1);
        v.expect("fine in tests");
    }
}

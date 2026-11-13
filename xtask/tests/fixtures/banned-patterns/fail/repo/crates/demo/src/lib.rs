//! Every banned pattern once.

pub fn a() -> u8 {
    todo!()
}

pub fn b() -> u8 {
    unimplemented!()
}

pub fn c(n: u8) -> u8 {
    match n {
        0 => 1,
        _ => unreachable!(),
    }
}

pub fn d(text: &str) -> u8 {
    let n: u8 = text.parse().unwrap();
    n
}

pub fn e(text: &str) -> u8 {
    let x = 1;
    text.parse().expect("a number")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn slow() {}

    // #[test]
    // fn disabled() {}
}

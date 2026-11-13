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

pub fn f(x: Option<u8>) -> u8 {
    if checks::test(1) {
        return x.unwrap();
    }
    0
}

pub struct S {
    #[cfg(test)]
    pub a: u8,
}

pub fn after_field(x: Option<u8>) -> u8 {
    x.unwrap()
}

pub fn in_match(v: u8, x: Option<u8>) -> u8 {
    match v {
        #[cfg(test)]
        0 => 0,
        _ => {
            x.unwrap()
        }
    }
}

#[rustfmt::skip]
pub fn one_line(x: Option<u8>) -> u8 { #[cfg(test)] let _a = 0; x.unwrap() }

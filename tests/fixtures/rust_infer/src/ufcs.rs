use std::fmt;

pub struct Code(pub u8);

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

pub trait Make {
    fn make(n: u8) -> Self;
}

impl Make for Code {
    fn make(n: u8) -> Self {
        Code(n)
    }
}

pub fn show(c: &Code, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    fmt::Debug::fmt(c, f)
}

pub fn build(s: String) -> Code {
    Make::make(s.len() as u8)
}

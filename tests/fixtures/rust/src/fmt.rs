//! Shares its name with `std::fmt`, which this file imports.
use std::fmt;

/// Not `std::fmt::Error`.
pub struct Error;

pub fn show() -> Result<(), fmt::Error> {
    Ok(())
}

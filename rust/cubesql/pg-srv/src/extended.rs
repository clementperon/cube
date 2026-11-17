//! Implementation for Extended Query

#[cfg(feature = "with-jiff")]
use crate::{DateValue, TimestampValue};

#[derive(Debug, PartialEq)]
pub enum BindValue {
    String(String),
    Int64(i64),
    Float64(f64),
    Bool(bool),
    #[cfg(feature = "with-jiff")]
    Timestamp(TimestampValue),
    #[cfg(feature = "with-jiff")]
    Date(DateValue),
    Null,
}

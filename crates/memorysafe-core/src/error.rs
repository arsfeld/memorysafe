use thiserror::Error;

// Eq is not derived because OutOfRange carries an f32, which does not implement Eq.
#[derive(Debug, Error, PartialEq)]
pub enum CoreError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} exceeds {max} bytes")]
    TooLong { field: &'static str, max: usize },
    #[error("{field} contains an illegal character at byte {index}")]
    IllegalChar { field: &'static str, index: usize },
    #[error("{field} must be within [0.0, 1.0], got {value}")]
    OutOfRange { field: &'static str, value: f32 },
}

//! Test support: a random generator of valid operations and small
//! operation constructors. Enabled for this crate's tests and, via the
//! `testing` feature, for the tests of dependent crates.

pub mod creator;
pub mod random;

pub use random::{
    AnnotationOption, Parameters, RandomProvider, RandomProviderImpl, generate_random_op,
};

#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod catalogue;

pub use catalogue::{Admitted, CATALOGUE, EncoderRefusal, Entry, License, encoder_by_name, lookup};

// The faces this adapter implements, and the vocabulary they speak.
pub use mediaencode;

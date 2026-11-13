//! Conformance suite for `alloy-wire` (ARFC 0003 sections 3.1, 3.4 and 3.5).
//!
//! The suite checks the codec through its public API only. It does not read the
//! codec's source. Every byte vector in it is built by hand from the tables of
//! the ARFC, and every rule it checks is written here a second time, from the
//! ARFC alone, as a model the codec is compared against.
//!
//! This library holds what the test target shares: the byte layouts of the
//! ARFC's tables (`layout`) and the model of which bytes a version 1 receiver
//! accepts (`model`). The tests are in `tests/conformance_wire/`.

#![forbid(unsafe_code)]

pub mod layout;
pub mod model;

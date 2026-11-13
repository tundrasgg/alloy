//! Conformance vectors for `alloy-wire`: ARFC 0003 sections 3.1 (handshake
//! messages and payloads), 3.4 (transport packet and inner frame) and 3.5
//! (fragment frame).
//!
//! Every test is named after the clause it checks. Byte vectors are built by
//! hand from the ARFC's tables; the codec is reached only through its public
//! API. Property tests run on fixed seeds (`kit::SEEDS`).

mod fragment;
mod frame;
mod handshake;
mod kit;
mod message;
mod transport;

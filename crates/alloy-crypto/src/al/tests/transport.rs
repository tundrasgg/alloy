//! Transport sealing and opening (ARFC 0003 section 3.4).

use proptest::prelude::*;

use super::super::{COUNTER_LIMIT, MAX_QUEUES, OpenError, SealError, SessionKeys};
use super::Pair;

fn sessions() -> (SessionKeys, SessionKeys) {
    Pair::new().run()
}

fn header(counter: u64) -> [u8; 9] {
    let mut header = [4, 0x10, 0x20, 0x30, 0x40, 0, 0, 0, 0];
    header[5..].copy_from_slice(&(counter as u32).to_le_bytes());
    header
}

fn sealed(keys: &SessionKeys, queue: u16, counter: u64, frame: &[u8]) -> Vec<u8> {
    let mut buf = frame.to_vec();
    buf.extend_from_slice(&[0u8; 16]);
    keys.send
        .seal_in_place(queue, counter, &header(counter), &mut buf)
        .expect("seal");
    buf
}

#[test]
fn seal_limits() {
    let (initiator, _) = sessions();
    let frame = [0u8; 1];
    let mut buf = [0u8; 17];
    assert_eq!(
        initiator
            .send
            .seal_in_place(0, COUNTER_LIMIT, &header(0), &mut buf),
        Err(SealError::CounterExhausted)
    );
    assert_eq!(buf, [0u8; 17], "nothing is written on error");
    assert_eq!(
        initiator
            .send
            .seal_in_place(0, u64::MAX, &header(0), &mut buf),
        Err(SealError::CounterExhausted)
    );
    assert_eq!(
        initiator
            .send
            .seal_in_place(MAX_QUEUES, 0, &header(0), &mut buf),
        Err(SealError::QueueOutOfRange)
    );
    let mut short = [0u8; 15];
    assert_eq!(
        initiator.send.seal_in_place(0, 0, &header(0), &mut short),
        Err(SealError::BufferTooShort)
    );
    // The last counter and queue allowed seal.
    let ok = sealed(&initiator, MAX_QUEUES - 1, COUNTER_LIMIT - 1, &frame);
    assert_eq!(ok.len(), 17);
}

#[test]
fn every_tampered_byte_fails_to_open() {
    let (initiator, responder) = sessions();
    let frame = b"\x60 an ipv6 packet of some length";
    let packet = sealed(&initiator, 2, 1000, frame);
    for index in 0..packet.len() {
        let mut tampered = packet.clone();
        tampered[index] ^= 1;
        let before = tampered.clone();
        assert_eq!(
            responder
                .recv
                .open_in_place(2, 1000, &header(1000), &mut tampered)
                .map(|_| ()),
            Err(OpenError),
            "ciphertext byte {index}"
        );
        assert_eq!(tampered, before, "buffer unchanged on failure");
    }
    for index in 0..9 {
        let mut other_header = header(1000);
        other_header[index] ^= 1;
        let mut buf = packet.clone();
        assert!(
            responder
                .recv
                .open_in_place(2, 1000, &other_header, &mut buf)
                .is_err(),
            "header byte {index}"
        );
    }
    let mut buf = packet.clone();
    assert!(
        responder
            .recv
            .open_in_place(3, 1000, &header(1000), &mut buf)
            .is_err()
    );
    let mut buf = packet.clone();
    assert!(
        responder
            .recv
            .open_in_place(2, 1001, &header(1000), &mut buf)
            .is_err()
    );
    // The same low 32 bits with a different full counter is a different
    // nonce.
    let mut buf = packet.clone();
    let wrapped = 1000 + (1u64 << 32);
    assert!(
        responder
            .recv
            .open_in_place(2, wrapped, &header(wrapped), &mut buf)
            .is_err()
    );
    let mut short = [0u8; 15];
    assert_eq!(
        responder
            .recv
            .open_in_place(2, 1000, &header(1000), &mut short)
            .map(|_| ()),
        Err(OpenError)
    );
}

#[test]
fn a_direction_does_not_open_its_own_packets() {
    let (initiator, responder) = sessions();
    let mut packet = sealed(&initiator, 0, 0, b"\x00");
    assert!(
        initiator
            .recv
            .open_in_place(0, 0, &header(0), &mut packet)
            .is_err()
    );
    let mut packet = sealed(&responder, 0, 0, b"\x00");
    assert!(
        responder
            .recv
            .open_in_place(0, 0, &header(0), &mut packet)
            .is_err()
    );
}

#[test]
fn an_empty_frame_seals_to_a_bare_tag() {
    let (initiator, responder) = sessions();
    let mut packet = sealed(&initiator, 0, 0, &[]);
    assert_eq!(packet.len(), 16);
    let opened = responder
        .recv
        .open_in_place(0, 0, &header(0), &mut packet)
        .expect("open");
    assert!(opened.is_empty());
}

proptest! {
    #[test]
    fn seal_then_open_round_trips(
        queue in 0u16..MAX_QUEUES,
        counter in 0u64..COUNTER_LIMIT,
        frame in proptest::collection::vec(any::<u8>(), 1..1500),
    ) {
        let (initiator, responder) = sessions();
        for (from, to) in [(&initiator, &responder), (&responder, &initiator)] {
            let mut packet = sealed(from, queue, counter, &frame);
            prop_assert_eq!(packet.len(), frame.len() + 16);
            let opened = to
                .recv
                .open_in_place(queue, counter, &header(counter), &mut packet)
                .expect("open");
            prop_assert_eq!(&opened[..], &frame[..]);
        }
    }

    // Distinct (queue, counter) pairs give distinct nonces, so the same frame
    // seals to different bytes.
    #[test]
    fn distinct_nonces_give_distinct_ciphertexts(
        a in (0u16..MAX_QUEUES, 0u64..COUNTER_LIMIT),
        b in (0u16..MAX_QUEUES, 0u64..COUNTER_LIMIT),
    ) {
        prop_assume!(a != b);
        let (initiator, _) = sessions();
        let frame = [0x45u8; 32];
        let mut first = frame.to_vec();
        first.extend_from_slice(&[0u8; 16]);
        initiator.send.seal_in_place(a.0, a.1, &header(0), &mut first).expect("seal");
        let mut second = frame.to_vec();
        second.extend_from_slice(&[0u8; 16]);
        initiator.send.seal_in_place(b.0, b.1, &header(0), &mut second).expect("seal");
        prop_assert_ne!(first, second);
    }
}

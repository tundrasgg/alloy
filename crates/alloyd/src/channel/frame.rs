//! The carriage of a node message (ARFC 0002 section 3.13): the payload of
//! one UDP datagram in one IPv6 packet between infrastructure addresses,
//! sealed as an IPv6 frame on VNI 0 of an AL session.

use std::net::Ipv6Addr;

use super::proto::MAX_FORWARDED_MESSAGE;

/// The UDP port of the node channel, at both ends.
pub const NODE_PORT: u16 = 7477;

/// The IPv6 header length, in bytes. The packet has no extension header.
const IPV6_HEADER: usize = 40;

/// The UDP header length, in bytes.
const UDP_HEADER: usize = 8;

/// The IPv6 next header value of UDP.
const NEXT_HEADER_UDP: u8 = 17;

/// The hop limit every node message is sent with.
const HOP_LIMIT: u8 = 64;

/// Why a packet is not a node message for this receiver. The receiver drops
/// it silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// Not an IPv6 packet carrying exactly one UDP datagram with no extension
    /// header, or its lengths disagree.
    #[error("the packet is not an IPv6 packet holding one UDP datagram")]
    Shape,
    /// The source is not the infrastructure address of the session's mesh
    /// key.
    #[error("the source address is not the sender's infrastructure address")]
    Source,
    /// The destination is not this node's infrastructure address.
    #[error("the destination address is not this node's infrastructure address")]
    Destination,
    /// A UDP port other than 7477.
    #[error("a UDP port is not 7477")]
    Port,
    /// The UDP checksum does not verify.
    #[error("the UDP checksum does not verify")]
    Checksum,
    /// The payload is longer than a node message may be.
    #[error("the payload is longer than any node message")]
    TooLong,
}

/// The IPv6 packet that carries `payload` from `src` to `dst`: traffic class
/// 0, flow label 0, next header 17, hop limit 64, no extension header, both
/// UDP ports 7477, and the checksum of RFC 8200 section 8.1.
///
/// # Errors
///
/// [`FrameError::TooLong`] for a payload over the largest node message.
pub fn encode(src: Ipv6Addr, dst: Ipv6Addr, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.len() > MAX_FORWARDED_MESSAGE {
        return Err(FrameError::TooLong);
    }
    let udp_len = UDP_HEADER + payload.len();
    let udp_len16 = u16::try_from(udp_len).map_err(|_| FrameError::TooLong)?;
    let mut packet = Vec::with_capacity(IPV6_HEADER + udp_len);
    packet.extend_from_slice(&[0x60, 0, 0, 0]);
    packet.extend_from_slice(&udp_len16.to_be_bytes());
    packet.push(NEXT_HEADER_UDP);
    packet.push(HOP_LIMIT);
    packet.extend_from_slice(&src.octets());
    packet.extend_from_slice(&dst.octets());
    packet.extend_from_slice(&NODE_PORT.to_be_bytes());
    packet.extend_from_slice(&NODE_PORT.to_be_bytes());
    packet.extend_from_slice(&udp_len16.to_be_bytes());
    packet.extend_from_slice(&[0, 0]);
    packet.extend_from_slice(payload);
    let sum = checksum(&src, &dst, &packet[IPV6_HEADER..]);
    packet[IPV6_HEADER + 6..IPV6_HEADER + 8].copy_from_slice(&sum.to_be_bytes());
    Ok(packet)
}

/// The payload of `packet` when this receiver accepts it: an IPv6 packet
/// with one UDP datagram, from `src`, the infrastructure address that the
/// session's mesh key has in the receiver's cluster, to `own`, the
/// receiver's own, both ports 7477, and a checksum that verifies. The
/// payload still has to decode as a node message.
///
/// # Errors
///
/// The [`FrameError`] of the first rule `packet` breaks.
pub fn accept(packet: &[u8], src: Ipv6Addr, own: Ipv6Addr) -> Result<&[u8], FrameError> {
    if packet.len() < IPV6_HEADER + UDP_HEADER || packet[0] >> 4 != 6 {
        return Err(FrameError::Shape);
    }
    let payload_len = usize::from(u16::from_be_bytes([packet[4], packet[5]]));
    if packet[6] != NEXT_HEADER_UDP || payload_len != packet.len() - IPV6_HEADER {
        return Err(FrameError::Shape);
    }
    let udp = &packet[IPV6_HEADER..];
    if usize::from(u16::from_be_bytes([udp[4], udp[5]])) != udp.len() {
        return Err(FrameError::Shape);
    }
    let from = address(&packet[8..24]);
    let to = address(&packet[24..40]);
    if from != src {
        return Err(FrameError::Source);
    }
    if to != own {
        return Err(FrameError::Destination);
    }
    if u16::from_be_bytes([udp[0], udp[1]]) != NODE_PORT
        || u16::from_be_bytes([udp[2], udp[3]]) != NODE_PORT
    {
        return Err(FrameError::Port);
    }
    let stated = u16::from_be_bytes([udp[6], udp[7]]);
    let mut zeroed = udp.to_vec();
    zeroed[6] = 0;
    zeroed[7] = 0;
    if stated == 0 || checksum(&from, &to, &zeroed) != stated {
        return Err(FrameError::Checksum);
    }
    let payload = &udp[UDP_HEADER..];
    if payload.len() > MAX_FORWARDED_MESSAGE {
        return Err(FrameError::TooLong);
    }
    Ok(payload)
}

fn address(bytes: &[u8]) -> Ipv6Addr {
    let mut octets = [0u8; 16];
    octets.copy_from_slice(bytes);
    Ipv6Addr::from(octets)
}

/// The UDP checksum over the IPv6 pseudo-header and `udp`, whose checksum
/// field is zero (RFC 8200 section 8.1, RFC 768): a computed 0 is sent as
/// `0xFFFF`.
fn checksum(src: &Ipv6Addr, dst: &Ipv6Addr, udp: &[u8]) -> u16 {
    let mut sum: u64 = 0;
    let mut add = |bytes: &[u8]| {
        for pair in bytes.chunks(2) {
            let high = u64::from(pair[0]) << 8;
            let low = pair.get(1).copied().map_or(0, u64::from);
            sum += high | low;
        }
    };
    add(&src.octets());
    add(&dst.octets());
    // A UDP length above 65,535 cannot be framed, so 32 bits always hold it.
    add(&u32::try_from(udp.len()).unwrap_or(u32::MAX).to_be_bytes());
    add(&[0, 0, 0, NEXT_HEADER_UDP]);
    add(udp);
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    let folded = !(sum as u16);
    if folded == 0 { 0xFFFF } else { folded }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Ipv6Addr = Ipv6Addr::new(0xfd01, 0x0203, 0x0405, 0, 1, 2, 3, 4);
    const B: Ipv6Addr = Ipv6Addr::new(0xfd01, 0x0203, 0x0405, 0, 5, 6, 7, 8);

    #[test]
    fn a_framed_message_is_accepted_by_its_receiver() {
        for len in [0, 1, 2, 3, 1_000, MAX_FORWARDED_MESSAGE] {
            let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let packet = encode(A, B, &payload).unwrap();
            assert_eq!(packet.len(), 48 + len);
            assert_eq!(&packet[..4], [0x60, 0, 0, 0]);
            assert_eq!(packet[6..8], [17, 64]);
            assert_eq!(accept(&packet, A, B), Ok(payload.as_slice()));
        }
        assert_eq!(
            encode(A, B, &vec![0; MAX_FORWARDED_MESSAGE + 1]),
            Err(FrameError::TooLong)
        );
    }

    #[test]
    fn the_checksum_matches_a_reference() {
        // The 16-bit words of RFC 8200 section 8.1 for this packet, written
        // out: `::1` twice (1 each), the upper-layer length 10, next header
        // 17, both ports 7477 (0x1D35), the UDP length 10, and "hi".
        let src = Ipv6Addr::LOCALHOST;
        let dst = Ipv6Addr::LOCALHOST;
        let packet = encode(src, dst, b"hi").unwrap();
        let mut sum: u32 = 2 + 10 + 17 + 0x1D35 + 0x1D35 + 10 + 0x6869;
        while sum >> 16 != 0 {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }
        let expected = !(sum as u16);
        assert_eq!(u16::from_be_bytes([packet[46], packet[47]]), expected);
    }

    #[test]
    fn every_rule_is_checked() {
        let packet = encode(A, B, b"message").unwrap();
        assert_eq!(accept(&packet, B, B), Err(FrameError::Source));
        assert_eq!(accept(&packet, A, A), Err(FrameError::Destination));
        let mut bad = packet.clone();
        bad[0] = 0x45;
        assert_eq!(accept(&bad, A, B), Err(FrameError::Shape));
        let mut bad = packet.clone();
        bad[6] = 6;
        assert_eq!(accept(&bad, A, B), Err(FrameError::Shape));
        assert_eq!(
            accept(&packet[..packet.len() - 1], A, B),
            Err(FrameError::Shape)
        );
        let mut bad = packet.clone();
        bad[41] = 1;
        assert_eq!(accept(&bad, A, B), Err(FrameError::Port));
        let mut bad = packet.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert_eq!(accept(&bad, A, B), Err(FrameError::Checksum));
        let mut bad = packet;
        bad[46] = 0;
        bad[47] = 0;
        assert_eq!(accept(&bad, A, B), Err(FrameError::Checksum));
    }

    #[test]
    fn accept_is_total() {
        let packet = encode(A, B, b"abc").unwrap();
        for len in 0..packet.len() {
            let _ = accept(&packet[..len], A, B);
        }
        let _ = accept(&[0x60; 47], A, B);
    }
}

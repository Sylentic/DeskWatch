//! Networks whose clients may use the kiosk page without the token.
//!
//! Only the TCP peer address is ever looked at, never a header such as
//! `X-Forwarded-For`, which any client can set. Behind a reverse proxy or
//! Docker's port mapping the peer is the proxy or the Docker gateway, not the
//! real client; see docs/kiosk.md before trusting a range there.

use std::net::IpAddr;

/// One `address/prefix` range, such as `192.168.0.0/16` or `fd00::/8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Network {
    addr: IpAddr,
    prefix: u8,
}

impl Network {
    /// Parse `address/prefix`. A bare address means a single host.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let (addr_text, prefix_text) = match text.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (text, None),
        };
        let addr: IpAddr = addr_text
            .parse()
            .map_err(|_| format!("{text:?} is not an IP address range like 192.168.0.0/16"))?;
        // Compare IPv4 as IPv4 even when written as ::ffff:a.b.c.d.
        let addr = addr.to_canonical();
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix =
            match prefix_text {
                Some(p) => p.parse::<u8>().ok().filter(|p| *p <= max).ok_or_else(|| {
                    format!("{text:?}: the prefix must be a number from 0 to {max}")
                })?,
                None => max,
            };
        Ok(Self { addr, prefix })
    }

    /// Is `ip` inside this range? IPv4-mapped IPv6 peers count as IPv4.
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                masked(u32::from(net).into(), u32::from(ip).into(), self.prefix, 32)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                masked(u128::from(net), u128::from(ip), self.prefix, 128)
            }
            _ => false,
        }
    }
}

/// Do the first `prefix` of `bits` bits of `a` and `b` match?
fn masked(a: u128, b: u128, prefix: u8, bits: u32) -> bool {
    let prefix = u32::from(prefix);
    if prefix == 0 {
        return true;
    }
    let shift = bits - prefix;
    (a >> shift) == (b >> shift)
}

/// Parse a whole config list. The first bad entry is the error.
pub fn parse_all(list: &[String]) -> Result<Vec<Network>, String> {
    list.iter().map(|text| Network::parse(text)).collect()
}

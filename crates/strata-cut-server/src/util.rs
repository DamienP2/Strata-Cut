//! Small helpers shared by the modules: the time, the local ports, addresses, keys.

/// Now, in ms since 1970.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// A free local port on the loopback (TCP, or UDP).
pub fn free_port(tcp: bool) -> Option<u16> {
    if tcp {
        std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?.local_addr().ok().map(|a| a.port())
    } else {
        std::net::UdpSocket::bind(("127.0.0.1", 0)).ok()?.local_addr().ok().map(|a| a.port())
    }
}

/// A text for an address's query (RFC 3986: only the unreserved characters as they are).
pub fn urlencode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// Arguments as owned texts.
pub fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// A short random key (the taps of a source).
pub fn rand_key() -> u64 {
    uuid::Uuid::new_v4().as_u128() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(urlencode("a b&é~"), "a%20b%26%C3%A9~");
        assert!(free_port(true).is_some() && free_port(false).is_some());
        assert_ne!(rand_key(), rand_key());
    }
}

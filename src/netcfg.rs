//! Where is the network pointed right now?
//!
//! A gate is only a gate if the kernel is routing through it. On a laptop the
//! default route changes under you (café wifi, phone hotspot, the TollGate), so
//! the daemon reads the live route rather than a configured address.

/// Parse `ip route` for the IPv4 default gateway.
pub fn parse_default_gateway(table: &str) -> Option<String> {
    for line in table.lines() {
        let mut words = line.split_whitespace();
        if words.next() != Some("default") {
            continue;
        }
        // "default via 10.0.0.1 dev wlan0 …" — "via" is absent on point-to-point
        let mut via_next = false;
        for w in words {
            if via_next {
                return Some(w.to_string());
            }
            if w == "via" {
                via_next = true;
            }
        }
    }
    None
}

/// The address a TollGate serves its portal on, per the v1 module.
pub fn gate_url(address: &str) -> String {
    if address.starts_with("http") {
        address.to_string()
    } else {
        format!("http://{address}:2121/")
    }
}

pub fn default_gateway() -> Option<String> {
    let out = std::process::Command::new("ip").args(["route", "show", "default"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_default_gateway(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_v4_default_route() {
        let table = "default via 192.168.50.1 dev wlan0 proto dhcp src 192.168.50.44 metric 600\n\
                     192.168.50.0/24 dev wlan0 proto kernel scope link src 192.168.50.44 metric 600\n";
        assert_eq!(parse_default_gateway(table).as_deref(), Some("192.168.50.1"));
    }

    #[test]
    fn a_route_without_via_is_not_a_gateway() {
        assert_eq!(parse_default_gateway("default dev ppp0 scope link\n"), None);
        assert_eq!(parse_default_gateway("192.168.1.0/24 dev eth0 scope link\n"), None);
    }

    #[test]
    fn an_address_becomes_the_portal_url() {
        assert_eq!(gate_url("192.168.50.205"), "http://192.168.50.205:2121/");
        assert_eq!(gate_url("http://198.51.100.7:2121/"), "http://198.51.100.7:2121/");
    }
}

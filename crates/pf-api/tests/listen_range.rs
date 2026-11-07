//! AC-5 of `r1.s2.w5`: `listen_allowed` accepts loopback (`127.0.0.0/8`, `::1`) and the tailnet
//! (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`), and nothing else. The edges of each range are tested.

use std::net::IpAddr;

use pf_api::guard::listen_allowed;

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
}

#[test]
fn the_first_and_last_address_of_each_range_are_allowed() {
    for text in [
        "127.0.0.0",
        "127.0.0.1",
        "127.255.255.255",
        "::1",
        "100.64.0.0",
        "100.100.1.1",
        "100.127.255.255",
        "fd7a:115c:a1e0::",
        "fd7a:115c:a1e0::1",
        "fd7a:115c:a1e0:ffff:ffff:ffff:ffff:ffff",
    ] {
        assert!(listen_allowed(ip(text)), "{text} must be allowed");
    }
}

#[test]
fn the_addresses_just_outside_each_range_are_refused() {
    for text in [
        "126.255.255.255",
        "128.0.0.0",
        "100.63.255.255",
        "100.128.0.0",
        "fd7a:115c:a1df:ffff::1",
        "fd7a:115c:a1e1::",
        "fd7a:115d:a1e0::1",
        "fd7b:115c:a1e0::1",
    ] {
        assert!(!listen_allowed(ip(text)), "{text} must be refused");
    }
}

#[test]
fn the_unspecified_lan_and_public_addresses_are_refused() {
    for text in [
        "0.0.0.0",
        "::",
        "192.168.1.5",
        "10.0.0.1",
        "172.16.0.1",
        "169.254.1.1",
        "8.8.8.8",
        "fe80::1",
        "2001:db8::1",
        "::2",
    ] {
        assert!(!listen_allowed(ip(text)), "{text} must be refused");
    }
}

#[test]
fn an_ipv4_mapped_address_counts_as_its_ipv4_form() {
    assert!(listen_allowed(ip("::ffff:100.64.0.1")));
    assert!(listen_allowed(ip("::ffff:127.0.0.1")));
    assert!(!listen_allowed(ip("::ffff:192.168.1.5")));
    assert!(!listen_allowed(ip("::ffff:0.0.0.0")));
    assert!(!listen_allowed(ip("::ffff:100.128.0.0")));
}

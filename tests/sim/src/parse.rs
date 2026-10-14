//! `Config::parse` against the inputs kontrol-flash or a hand edit can produce.

#[path = "../../../firmware/src/app.rs"]
#[allow(dead_code)]
mod app;
#[allow(dead_code)]
mod clock; // app.rs reads the clock elsewhere, so the binary needs a time driver to link

use app::Config;

const VALID: &str =
    "ssid=home\npass=secret\ntoken=tk_abcdefghijklmnopqrstuvwxyz012\nserver=https://ntfy.example\ntopic=pc\nmac=aa:bb:cc:dd:ee:ff";

/// VALID with `key`'s line replaced by `line`, or dropped when `line` is empty.
fn with(key: &str, line: &str) -> &'static str {
    let text: Vec<&str> =
        VALID.lines().map(|l| if l.starts_with(&format!("{key}=")) { line } else { l }).filter(|l| !l.is_empty()).collect();
    text.join("\n").leak()
}

#[test]
fn a_valid_config_parses() {
    let config = Config::parse(VALID).expect("VALID parses");
    assert_eq!((config.ssid, config.pass), ("home", "secret"));
}

#[test]
fn each_missing_key_is_rejected() {
    for key in ["ssid", "pass", "token", "server", "topic", "mac"] {
        assert!(Config::parse(with(key, "")).is_none(), "{key} missing, and it parsed");
    }
}

#[test]
fn an_unknown_key_or_a_line_without_equals_is_rejected() {
    assert!(Config::parse(&*format!("{VALID}\nchannel=6").leak()).is_none());
    assert!(Config::parse(&*format!("{VALID}\nmac aa:bb:cc:dd:ee:ff").leak()).is_none());
}

#[test]
fn ssid_and_pass_at_cyw43_limits_parse_and_past_them_are_rejected() {
    let ssid = |n: usize| with("ssid", format!("ssid={}", "s".repeat(n)).leak());
    let pass = |n: usize| with("pass", format!("pass={}", "p".repeat(n)).leak());
    assert!(Config::parse(ssid(32)).is_some() && Config::parse(ssid(33)).is_none());
    assert!(Config::parse(pass(64)).is_some() && Config::parse(pass(65)).is_none());
}

#[test]
fn malformed_macs_are_rejected() {
    for mac in [
        "aa:bb:cc:dd:ee", "aa:bb:cc:dd:ee:ff:00", "a:bb:cc:dd:ee:ff", "aaa:b:cc:dd:ee:ff", "gg:bb:cc:dd:ee:ff",
        "aa-bb-cc-dd-ee-ff",
    ] {
        assert!(Config::parse(with("mac", format!("mac={mac}").leak())).is_none(), "{mac} parsed");
    }
}

#[test]
fn values_too_long_for_their_buffers_are_rejected() {
    // The status URL is the subscribe URL less `-commands/json?since=2m`, so it fits whenever that one does.
    let server = format!("server=https://{}.example", "n".repeat(168)); // the subscribe URL 210 bytes, status 187
    assert!(Config::parse(with("server", server.leak())).is_none(), "a subscribe URL past 192 bytes parsed");
    assert!(
        Config::parse(with("token", format!("token=tk_{}", "t".repeat(55)).leak())).is_none(),
        "an auth header past 64 bytes parsed"
    );
}

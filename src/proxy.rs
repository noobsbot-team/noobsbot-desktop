//! `[network]` proxy (E8.2 Network): environment for the apps we start, and the GTK proxy settings.

use crate::config::{Network, ProxyMode};

/// `http_proxy`, `https_proxy`, `all_proxy` and `no_proxy` for a manual proxy; nothing otherwise (a PAC file can't be
/// said in the environment, and "none" leaves what the session already has).
pub fn env(network: &Network) -> Vec<(&'static str, String)> {
    if network.proxy != ProxyMode::Manual {
        return Vec::new();
    }
    let url = |scheme: &str, server: &str| match server.trim() {
        "" => None,
        s if s.contains("://") => Some(s.to_owned()),
        s => Some(format!("{scheme}://{s}")),
    };
    let servers = [
        ("http_proxy", url("http", &network.proxy_http)),
        ("https_proxy", url("http", &network.proxy_https)),
        ("all_proxy", url("socks5", &network.proxy_socks)),
    ];
    let mut env: Vec<_> = servers.into_iter().filter_map(|(name, url)| Some((name, url?))).collect();
    let bypass = hosts(&network.proxy_bypass);
    if !env.is_empty() && !bypass.is_empty() {
        env.push(("no_proxy", bypass.join(",")));
    }
    env
}

fn hosts(list: &str) -> Vec<&str> {
    list.split([',', ' ']).filter(|h| !h.is_empty()).collect()
}

/// `gsettings set` arguments (schema, key, GVariant text) for `org.gnome.system.proxy`.
fn gsettings(network: &Network) -> Vec<[String; 3]> {
    let key = |schema: &str, key: &str, value: String| [format!("org.gnome.system.proxy{schema}"), key.to_owned(), value];
    let quoted = |s: &str| format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"));
    let mode = match network.proxy {
        ProxyMode::None => "none",
        ProxyMode::Manual => "manual",
        ProxyMode::Auto => "auto",
    };
    let ignore: Vec<String> = hosts(&network.proxy_bypass).into_iter().map(quoted).collect();
    let mut keys = vec![
        key("", "mode", quoted(mode)),
        key("", "autoconfig-url", quoted(network.proxy_pac.trim())),
        key("", "ignore-hosts", format!("[{}]", ignore.join(", "))),
    ];
    for (schema, server) in [(".http", &network.proxy_http), (".https", &network.proxy_https), (".socks", &network.proxy_socks)] {
        let server = server.trim().split_once("://").map_or(server.trim(), |(_, rest)| rest).trim_end_matches('/');
        let (host, port) = server.rsplit_once(':').filter(|(_, p)| p.parse::<u16>().is_ok()).unwrap_or((server, "0"));
        keys.push(key(schema, "host", quoted(host)));
        keys.push(key(schema, "port", port.to_owned()));
    }
    keys
}

/// Writes the GTK proxy settings on a thread (a real session only, never nested), so the frame never waits.
pub fn gsettings_in_background(network: &Network) {
    let keys = gsettings(network);
    std::thread::spawn(move || {
        for [schema, key, value] in &keys {
            if !std::process::Command::new("gsettings").args(["set", schema, key, value]).status().is_ok_and(|s| s.success()) {
                tracing::warn!("gsettings {schema} {key}: failed");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manual() -> Network {
        Network {
            proxy: ProxyMode::Manual,
            proxy_http: "proxy.lan:3128".into(),
            proxy_https: "".into(),
            proxy_socks: "socks5h://tor:9050".into(),
            proxy_bypass: "localhost, 127.0.0.0/8 .lan".into(),
            ..Default::default()
        }
    }

    #[test]
    fn env_for_a_manual_proxy_only() {
        let env = env(&manual());
        assert_eq!(
            env,
            [
                ("http_proxy", "http://proxy.lan:3128".to_string()),
                ("all_proxy", "socks5h://tor:9050".to_string()),
                ("no_proxy", "localhost,127.0.0.0/8,.lan".to_string())
            ]
        );
        assert!(super::env(&Network { proxy: ProxyMode::Auto, ..manual() }).is_empty());
        assert!(super::env(&Network { proxy: ProxyMode::None, ..manual() }).is_empty());
        // A bypass list alone sets nothing.
        assert!(super::env(&Network { proxy_http: "".into(), proxy_socks: "".into(), ..manual() }).is_empty());
    }

    #[test]
    fn gtk_keys() {
        let keys = gsettings(&manual());
        let get = |schema: &str, key: &str| keys.iter().find(|k| k[0] == format!("org.gnome.system.proxy{schema}") && k[1] == key).map(|k| k[2].as_str());
        assert_eq!(get("", "mode"), Some("'manual'"));
        assert_eq!(get("", "ignore-hosts"), Some("['localhost', '127.0.0.0/8', '.lan']"));
        assert_eq!((get(".http", "host"), get(".http", "port")), (Some("'proxy.lan'"), Some("3128")));
        assert_eq!((get(".socks", "host"), get(".socks", "port")), (Some("'tor'"), Some("9050")));
        assert_eq!((get(".https", "host"), get(".https", "port")), (Some("''"), Some("0")));
    }
}

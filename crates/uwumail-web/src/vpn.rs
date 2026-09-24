//! The VPN the portal sets up beside the server: gluetun, in the `vpn` profile of compose.yaml.
//!
//! The settings live in the database, keys included, so they can be changed without typing the key
//! again. With the machine's helper (`deploy/host/`) the portal hands them over and the helper writes
//! `.env.vpn` and starts the container; without it the portal shows the file to copy. The helper checks
//! everything again, because it is the side with the rights; the checks here are for good error messages.

use std::collections::BTreeMap;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

/// The settings key the VPN is stored under.
pub const VPN_KEY: &str = "vpn.config";
/// Where the server reaches gluetun's HTTP proxy, from the container next to it.
pub const GLUETUN_PROXY: &str = "http://gluetun:8888";
const VALUE_MAX: usize = 1024;
const OVPN_MAX: usize = 64 * 1024;

/// A provider gluetun knows, and which kinds of connection it offers there.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: &'static str,
    pub name: &'static str,
    pub wireguard: bool,
    pub openvpn: bool,
    /// WireGuard needs the address the provider gave the key (`WIREGUARD_ADDRESSES`).
    pub needs_addresses: bool,
}

const fn provider(id: &'static str, name: &'static str, wireguard: bool, needs_addresses: bool) -> Provider {
    Provider { id, name, wireguard, openvpn: true, needs_addresses }
}

/// Every provider gluetun supports, `custom` for any WireGuard or OpenVPN server.
pub const PROVIDERS: &[Provider] = &[
    provider("nordvpn", "NordVPN", true, false),
    provider("mullvad", "Mullvad", true, true),
    provider("protonvpn", "Proton VPN", true, false),
    provider("surfshark", "Surfshark", true, true),
    provider("ivpn", "IVPN", true, true),
    provider("airvpn", "AirVPN", true, true),
    provider("windscribe", "Windscribe", true, true),
    provider("fastestvpn", "FastestVPN", true, true),
    provider("private internet access", "Private Internet Access", false, false),
    provider("expressvpn", "ExpressVPN", false, false),
    provider("cyberghost", "CyberGhost", false, false),
    provider("ipvanish", "IPVanish", false, false),
    provider("purevpn", "PureVPN", false, false),
    provider("torguard", "TorGuard", false, false),
    provider("vyprvpn", "VyprVPN", false, false),
    provider("hidemyass", "HideMyAss", false, false),
    provider("privado", "Privado", false, false),
    provider("privatevpn", "PrivateVPN", false, false),
    provider("perfect privacy", "Perfect Privacy", false, false),
    provider("giganews", "Giganews", false, false),
    provider("slickvpn", "SlickVPN", false, false),
    provider("vpn unlimited", "VPN Unlimited", false, false),
    provider("vpnsecure", "VPNSecure", false, false),
    provider("custom", "Custom", true, true),
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VpnKind {
    #[default]
    Wireguard,
    Openvpn,
}

/// The VPN as stored, secrets included. Never sent to the browser as it is: see [`VpnConfig::shown`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VpnConfig {
    pub provider: String,
    pub kind: VpnKind,
    /// Comma-separated, as gluetun takes them. Empty: the provider's choice.
    pub countries: String,
    pub regions: String,
    pub cities: String,
    pub hostnames: String,
    pub wireguard_private_key: String,
    pub wireguard_preshared_key: String,
    /// The address the provider gave the key, e.g. `10.64.0.2/32`.
    pub wireguard_addresses: String,
    /// Only for `custom`: the server's public key, address and port.
    pub wireguard_public_key: String,
    pub wireguard_endpoint_ip: String,
    pub wireguard_endpoint_port: Option<u16>,
    pub openvpn_user: String,
    pub openvpn_password: String,
    /// Only for `custom`: the .ovpn file.
    pub openvpn_config: String,
}

/// A change from the browser. Secrets left out stay as they are; an empty one is removed.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VpnChange {
    pub provider: String,
    pub kind: VpnKind,
    pub countries: String,
    pub regions: String,
    pub cities: String,
    pub hostnames: String,
    pub wireguard_private_key: Option<String>,
    pub wireguard_preshared_key: Option<String>,
    pub wireguard_addresses: String,
    pub wireguard_public_key: String,
    pub wireguard_endpoint_ip: String,
    pub wireguard_endpoint_port: Option<u16>,
    pub openvpn_user: String,
    pub openvpn_password: Option<String>,
    pub openvpn_config: Option<String>,
}

/// Which secrets are set, for the form.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VpnSecrets {
    pub wireguard_private_key: bool,
    pub wireguard_preshared_key: bool,
    pub openvpn_password: bool,
    pub openvpn_config: bool,
}

fn trimmed(value: &str) -> String {
    value.trim().to_owned()
}

/// A comma-separated list with single spaces, e.g. `Switzerland, Netherlands`.
fn list(value: &str) -> String {
    value.split(',').map(str::trim).filter(|item| !item.is_empty()).collect::<Vec<_>>().join(",")
}

impl VpnConfig {
    pub fn provider(&self) -> Option<&'static Provider> {
        PROVIDERS.iter().find(|provider| provider.id == self.provider)
    }

    /// The config with a change applied: secrets only when given.
    pub fn changed(&self, change: VpnChange) -> VpnConfig {
        let secret = |new: Option<String>, old: &str| new.map_or_else(|| old.to_owned(), |value| trimmed(&value));
        VpnConfig {
            provider: trimmed(&change.provider).to_lowercase(),
            kind: change.kind,
            countries: list(&change.countries),
            regions: list(&change.regions),
            cities: list(&change.cities),
            hostnames: list(&change.hostnames),
            wireguard_private_key: secret(change.wireguard_private_key, &self.wireguard_private_key),
            wireguard_preshared_key: secret(change.wireguard_preshared_key, &self.wireguard_preshared_key),
            wireguard_addresses: list(&change.wireguard_addresses),
            wireguard_public_key: trimmed(&change.wireguard_public_key),
            wireguard_endpoint_ip: trimmed(&change.wireguard_endpoint_ip),
            wireguard_endpoint_port: change.wireguard_endpoint_port,
            openvpn_user: trimmed(&change.openvpn_user),
            openvpn_password: change.openvpn_password.unwrap_or_else(|| self.openvpn_password.clone()),
            openvpn_config: change
                .openvpn_config
                .map_or_else(|| self.openvpn_config.clone(), |text| text.replace("\r\n", "\n").trim().to_owned()),
        }
    }

    /// The config without its secrets, and which of them are set.
    pub fn shown(&self) -> (VpnConfig, VpnSecrets) {
        let secrets = VpnSecrets {
            wireguard_private_key: !self.wireguard_private_key.is_empty(),
            wireguard_preshared_key: !self.wireguard_preshared_key.is_empty(),
            openvpn_password: !self.openvpn_password.is_empty(),
            openvpn_config: !self.openvpn_config.is_empty(),
        };
        let shown = VpnConfig {
            wireguard_private_key: String::new(),
            wireguard_preshared_key: String::new(),
            openvpn_password: String::new(),
            openvpn_config: String::new(),
            ..self.clone()
        };
        (shown, secrets)
    }

    /// Whether this is complete enough to connect. The error names the field (`vpnInvalid` messages).
    pub fn check(&self) -> Result<(), String> {
        let provider = self.provider().ok_or_else(|| "choose a VPN provider".to_owned())?;
        let custom = provider.id == "custom";
        for (name, value) in self.values() {
            if value.len() > VALUE_MAX || value.contains('\'') || value.chars().any(char::is_control) {
                return Err(format!("{name} may not contain quotes or line breaks"));
            }
        }
        match self.kind {
            VpnKind::Wireguard => {
                if !provider.wireguard {
                    return Err(format!("{} only works with OpenVPN in gluetun", provider.name));
                }
                if !is_wireguard_key(&self.wireguard_private_key) {
                    return Err("the WireGuard private key is missing or not a key (44 characters ending in =)".into());
                }
                if !self.wireguard_preshared_key.is_empty() && !is_wireguard_key(&self.wireguard_preshared_key) {
                    return Err("the WireGuard preshared key is not a key".into());
                }
                if provider.needs_addresses && self.wireguard_addresses.is_empty() {
                    return Err(format!("{} needs the address of the key, e.g. 10.64.0.2/32", provider.name));
                }
                for address in self.wireguard_addresses.split(',').filter(|a| !a.is_empty()) {
                    let ip = address.split_once('/').map_or(address, |(ip, _)| ip);
                    if ip.parse::<IpAddr>().is_err() {
                        return Err(format!("{address} is not an address like 10.64.0.2/32"));
                    }
                }
                if custom {
                    if self.wireguard_endpoint_ip.parse::<IpAddr>().is_err() {
                        return Err("the server's address has to be an IP address".into());
                    }
                    if self.wireguard_endpoint_port.is_none_or(|port| port == 0) {
                        return Err("the server's port is missing".into());
                    }
                    if !is_wireguard_key(&self.wireguard_public_key) {
                        return Err("the server's public key is missing or not a key".into());
                    }
                }
            }
            VpnKind::Openvpn => {
                if !provider.openvpn {
                    return Err(format!("{} does not offer OpenVPN in gluetun", provider.name));
                }
                if custom {
                    if self.openvpn_config.is_empty() {
                        return Err("a custom OpenVPN server needs its .ovpn file".into());
                    }
                    if self.openvpn_config.len() > OVPN_MAX {
                        return Err("the .ovpn file is too big".into());
                    }
                    if let Some(line) = ovpn_refused_line(&self.openvpn_config) {
                        return Err(format!(
                            "the .ovpn file asks for more than a connection and cannot be used: {line}"
                        ));
                    }
                } else if self.openvpn_user.is_empty() || self.openvpn_password.is_empty() {
                    return Err("OpenVPN needs the service user name and password of your VPN account".into());
                }
            }
        }
        Ok(())
    }

    fn values(&self) -> [(&'static str, &str); 11] {
        [
            ("the countries", &self.countries),
            ("the regions", &self.regions),
            ("the cities", &self.cities),
            ("the server names", &self.hostnames),
            ("the private key", &self.wireguard_private_key),
            ("the preshared key", &self.wireguard_preshared_key),
            ("the addresses", &self.wireguard_addresses),
            ("the public key", &self.wireguard_public_key),
            ("the server address", &self.wireguard_endpoint_ip),
            ("the user name", &self.openvpn_user),
            ("the password", &self.openvpn_password),
        ]
    }

    /// gluetun's variables for this VPN, as `.env.vpn` holds them.
    pub fn env(&self) -> BTreeMap<&'static str, String> {
        let mut env = BTreeMap::new();
        let mut put = |key: &'static str, value: &str| {
            if !value.is_empty() {
                env.insert(key, value.to_owned());
            }
        };
        put("VPN_SERVICE_PROVIDER", &self.provider);
        let custom = self.provider == "custom";
        match self.kind {
            VpnKind::Wireguard => {
                put("VPN_TYPE", "wireguard");
                put("WIREGUARD_PRIVATE_KEY", &self.wireguard_private_key);
                put("WIREGUARD_PRESHARED_KEY", &self.wireguard_preshared_key);
                put("WIREGUARD_ADDRESSES", &self.wireguard_addresses);
                if custom {
                    put("WIREGUARD_PUBLIC_KEY", &self.wireguard_public_key);
                    put("WIREGUARD_ENDPOINT_IP", &self.wireguard_endpoint_ip);
                    put(
                        "WIREGUARD_ENDPOINT_PORT",
                        &self.wireguard_endpoint_port.map(|p| p.to_string()).unwrap_or_default(),
                    );
                }
            }
            VpnKind::Openvpn => {
                put("VPN_TYPE", "openvpn");
                put("OPENVPN_USER", &self.openvpn_user);
                put("OPENVPN_PASSWORD", &self.openvpn_password);
            }
        }
        if !custom {
            put("SERVER_COUNTRIES", &self.countries);
            put("SERVER_REGIONS", &self.regions);
            put("SERVER_CITIES", &self.cities);
            put("SERVER_HOSTNAMES", &self.hostnames);
        }
        env
    }

    /// The .ovpn file to hand over, for a custom OpenVPN server.
    pub fn ovpn(&self) -> Option<&str> {
        (self.kind == VpnKind::Openvpn && self.provider == "custom" && !self.openvpn_config.is_empty())
            .then_some(self.openvpn_config.as_str())
    }

    /// `.env.vpn` as a person would copy it onto the machine.
    pub fn env_file(&self) -> String {
        let mut text = String::from("# The VPN for UwUMail (gluetun), from the portal: Settings -> VPN & proxy.\n");
        for (key, value) in self.env() {
            text.push_str(&format!("{key}='{value}'\n"));
        }
        if self.ovpn().is_some() {
            text.push_str("OPENVPN_CUSTOM_CONFIG='/gluetun/custom/custom.ovpn'\n");
        }
        text
    }
}

fn is_wireguard_key(key: &str) -> bool {
    key.len() == 44
        && key.ends_with('=')
        && key[..43].chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
}

/// The first line of an .ovpn file that does more than connect: runs a program, loads a plugin, or reads or
/// writes a file of its own choosing. The helper refuses the same.
pub fn ovpn_refused_line(text: &str) -> Option<String> {
    const REFUSED: &[&str] = &[
        "up",
        "down",
        "route-up",
        "route-pre-down",
        "ipchange",
        "client-connect",
        "client-disconnect",
        "learn-address",
        "auth-user-pass-verify",
        "tls-verify",
        "script-security",
        "plugin",
        "log",
        "log-append",
        "status",
        "writepid",
        "cd",
        "chroot",
        "daemon",
        "config",
        "askpass",
        "tmp-dir",
        "iproute",
        "tls-crypt-v2-verify",
        "tls-export-cert",
        "setenv",
        "setenv-safe",
        "pull-filter",
        "dhcp-option",
    ];
    text.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let directive = words.next()?.trim_start_matches('-').to_ascii_lowercase();
        let refused = REFUSED.contains(&directive.as_str())
            || directive.starts_with("management")
            || (directive == "auth-user-pass"
                && words.next().is_some_and(|word| !word.starts_with('#') && !word.starts_with(';')));
        refused.then(|| line.trim().to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa=";

    fn nord() -> VpnConfig {
        VpnConfig {
            provider: "nordvpn".into(),
            kind: VpnKind::Wireguard,
            countries: "Switzerland".into(),
            wireguard_private_key: KEY.into(),
            ..VpnConfig::default()
        }
    }

    #[test]
    fn a_provider_with_a_key_becomes_gluetun_variables() {
        let config = nord();
        config.check().unwrap();
        let env = config.env();
        assert_eq!(env["VPN_SERVICE_PROVIDER"], "nordvpn");
        assert_eq!(env["VPN_TYPE"], "wireguard");
        assert_eq!(env["SERVER_COUNTRIES"], "Switzerland");
        assert!(!env.contains_key("WIREGUARD_ENDPOINT_IP"), "only for custom");
        assert!(config.env_file().contains(&format!("WIREGUARD_PRIVATE_KEY='{KEY}'")));
    }

    #[test]
    fn what_cannot_connect_is_explained() {
        assert!(VpnConfig::default().check().is_err(), "no provider");
        let short = VpnConfig { wireguard_private_key: "abc".into(), ..nord() };
        assert!(short.check().unwrap_err().contains("private key"));
        let mullvad = VpnConfig { provider: "mullvad".into(), ..nord() };
        assert!(mullvad.check().unwrap_err().contains("address"), "Mullvad needs the key's address");
        let pia = VpnConfig { provider: "private internet access".into(), ..nord() };
        assert!(pia.check().unwrap_err().contains("OpenVPN"));
        let quote = VpnConfig { countries: "x'\nFOO=1".into(), ..nord() };
        assert!(quote.check().is_err(), "nothing that could end a value in .env.vpn");
        let custom = VpnConfig { provider: "custom".into(), wireguard_addresses: "10.64.0.2/32".into(), ..nord() };
        assert!(custom.check().unwrap_err().contains("IP address"));
        let custom = VpnConfig {
            wireguard_endpoint_ip: "203.0.113.10".into(),
            wireguard_endpoint_port: Some(51820),
            wireguard_public_key: KEY.into(),
            ..custom
        };
        custom.check().unwrap();
        assert_eq!(custom.env()["WIREGUARD_ENDPOINT_PORT"], "51820");
        assert!(!custom.env().contains_key("SERVER_COUNTRIES"));
    }

    #[test]
    fn secrets_stay_unless_they_are_given() {
        let config = VpnConfig { openvpn_password: "pw".into(), ..nord() };
        let (shown, secrets) = config.shown();
        assert!(shown.wireguard_private_key.is_empty() && secrets.wireguard_private_key && secrets.openvpn_password);
        let change = VpnChange {
            provider: " NordVPN ".into(),
            countries: " Switzerland ,, Netherlands ".into(),
            openvpn_password: Some(String::new()),
            ..VpnChange::default()
        };
        let changed = config.changed(change);
        assert_eq!(changed.provider, "nordvpn");
        assert_eq!(changed.countries, "Switzerland,Netherlands");
        assert_eq!(changed.wireguard_private_key, KEY, "left out: kept");
        assert!(changed.openvpn_password.is_empty(), "empty: removed");
    }

    #[test]
    fn an_ovpn_file_may_only_connect() {
        let plain = "client\nremote 203.0.113.1 1194\nauth-user-pass\n<ca>\nMIIB\n</ca>\n";
        assert_eq!(ovpn_refused_line(plain), None);
        assert_eq!(ovpn_refused_line("client\n  up /bin/sh"), Some("up /bin/sh".into()));
        assert!(ovpn_refused_line("--plugin /x.so").is_some());
        assert!(ovpn_refused_line("auth-user-pass /etc/shadow").is_some());
        assert!(ovpn_refused_line("management 0.0.0.0 7505").is_some());
        let custom = VpnConfig {
            provider: "custom".into(),
            kind: VpnKind::Openvpn,
            openvpn_config: "client\nscript-security 2".into(),
            ..VpnConfig::default()
        };
        assert!(custom.check().is_err());
        let custom = VpnConfig { openvpn_config: plain.trim().into(), ..custom };
        custom.check().unwrap();
        assert_eq!(custom.ovpn(), Some(plain.trim()));
        assert!(custom.env_file().contains("OPENVPN_CUSTOM_CONFIG"));
    }
}

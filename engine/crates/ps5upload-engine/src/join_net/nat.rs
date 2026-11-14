//! The router's port for internet guests: UPnP-IGD first, then PCP / NAT-PMP. The mapped address is
//! one more candidate (a guest whose own NAT is strict can still reach it). Best effort, within
//! [`BUDGET`]: a router that says nothing costs a join nothing.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::NonZeroU16;
use std::time::Duration;

use igd_next::aio::tokio::Tokio;
use igd_next::aio::Gateway;
use igd_next::{PortMappingProtocol, SearchOptions};

/// How long a mapping may take to make.
pub(crate) const BUDGET: Duration = Duration::from_secs(2);
/// The lease asked for; renewed at half of it.
pub(crate) const LEASE: Duration = Duration::from_secs(3600);
/// How the mapping shows on the router's page.
const DESCRIPTION: &str = "PS5Upload Play Together";

/// Where to look for a router.
#[derive(Clone, Debug)]
pub(crate) struct Search {
    /// SSDP's multicast address (a test points it at its fake router).
    pub ssdp: SocketAddr,
    /// Try NAT-PMP / PCP when UPnP finds nothing.
    pub pmp: bool,
}

impl Default for Search {
    fn default() -> Self {
        Search {
            ssdp: "239.255.255.250:1900".parse().expect("address"),
            pmp: true,
        }
    }
}

pub(crate) enum Via {
    Upnp {
        gateway: Box<Gateway<Tokio>>,
        lease: u32,
    },
    Pmp(Box<crab_nat::PortMapping>),
}

/// A port held open on the router for `local`.
pub(crate) struct Mapping {
    pub local: SocketAddr,
    pub external: SocketAddr,
    pub via: Via,
}

impl Mapping {
    pub(crate) fn how(&self) -> &'static str {
        match self.via {
            Via::Upnp { .. } => "upnp",
            Via::Pmp(_) => "nat-pmp",
        }
    }
}

/// Maps `local` (an IPv4 address on this computer and the ICE socket's port) on the router, within
/// [`BUDGET`].
pub(crate) async fn map(local: SocketAddr, search: &Search) -> Option<Mapping> {
    let IpAddr::V4(ip) = local.ip() else {
        return None;
    };
    // (The tests' fake router is on loopback.)
    if (ip.is_loopback() && !cfg!(test)) || ip.is_unspecified() {
        return None;
    }
    let r = tokio::time::timeout(BUDGET, async {
        if let Some(m) = upnp(local, search.ssdp).await {
            return Some(m);
        }
        if search.pmp {
            return pmp(local).await;
        }
        None
    })
    .await
    .ok()
    .flatten();
    if let Some(m) = &r {
        crate::log_at!(
            Info,
            "together",
            "join: internet port mapped on the router by {} (port {}){}",
            m.how(),
            m.external.port(),
            if super::rtc::outside_shared(m.external.ip()) {
                ", but its outside address is not the internet's: another router or the provider's shared address is in front"
            } else {
                ""
            }
        );
    } else {
        // Info: the first question when a friend can't connect (#822).
        crate::log_at!(
            Info,
            "together",
            "join: no router mapping for the internet port (no UPnP or NAT-PMP answered)"
        );
    }
    r
}

async fn upnp(local: SocketAddr, ssdp: SocketAddr) -> Option<Mapping> {
    let opts = SearchOptions {
        bind_addr: SocketAddr::new(local.ip(), 0),
        broadcast_address: ssdp,
        timeout: Some(BUDGET),
        single_search_timeout: Some(BUDGET),
    };
    let gateway = igd_next::aio::tokio::search_gateway(opts).await.ok()?;
    let ext_ip = gateway.get_external_ip().await.ok()?;
    if ext_ip.is_unspecified() {
        return None;
    }
    // The same port outside as inside, when the router allows: then the host candidate and the
    // mapped one differ only in the address.
    let mut lease = LEASE.as_secs() as u32;
    let mut r = gateway
        .add_port(
            PortMappingProtocol::UDP,
            local.port(),
            local,
            lease,
            DESCRIPTION,
        )
        .await;
    if matches!(r, Err(igd_next::AddPortError::OnlyPermanentLeasesSupported)) {
        lease = 0; // removed on close
        r = gateway
            .add_port(
                PortMappingProtocol::UDP,
                local.port(),
                local,
                lease,
                DESCRIPTION,
            )
            .await;
    }
    r.ok()?;
    Some(Mapping {
        local,
        external: SocketAddr::new(ext_ip, local.port()),
        via: Via::Upnp {
            gateway: Box::new(gateway),
            lease,
        },
    })
}

/// The router as NAT-PMP and PCP reach it: the subnet's first address (a home router nearly always
/// is), from the interface `local` is on.
pub(crate) fn gateway_guess(local: Ipv4Addr) -> Option<Ipv4Addr> {
    if_addrs::get_if_addrs().ok()?.into_iter().find_map(|i| {
        let if_addrs::IfAddr::V4(v4) = i.addr else {
            return None;
        };
        (v4.ip == local).then(|| first_host(v4.ip, v4.netmask))?
    })
}

fn first_host(ip: Ipv4Addr, mask: Ipv4Addr) -> Option<Ipv4Addr> {
    let m = u32::from(mask);
    if m.leading_ones() > 30 {
        return None;
    }
    let gw = Ipv4Addr::from((u32::from(ip) & m) + 1);
    (gw != ip).then_some(gw)
}

async fn pmp(local: SocketAddr) -> Option<Mapping> {
    let IpAddr::V4(ip) = local.ip() else {
        return None;
    };
    let gw = gateway_guess(ip)?;
    let port = NonZeroU16::new(local.port())?;
    let timeout_config = Some(crab_nat::TimeoutConfig {
        initial_timeout: Duration::from_millis(250),
        max_retries: 2,
        max_retry_timeout: Some(Duration::from_millis(750)),
    });
    let m = crab_nat::PortMapping::new(
        gw.into(),
        local.ip(),
        crab_nat::InternetProtocol::Udp,
        port,
        crab_nat::PortMappingOptions {
            external_port: Some(port),
            lifetime_seconds: Some(LEASE.as_secs() as u32),
            timeout_config,
        },
    )
    .await
    .ok()?;
    let ext_ip = match m.mapping_type() {
        crab_nat::PortMappingType::Pcp { external_ip, .. } => external_ip,
        crab_nat::PortMappingType::NatPmp => IpAddr::V4(
            crab_nat::natpmp::external_address(gw.into(), timeout_config)
                .await
                .ok()?,
        ),
    };
    Some(Mapping {
        local,
        external: SocketAddr::new(ext_ip, m.external_port().get()),
        via: Via::Pmp(Box::new(m)),
    })
}

/// Renews the lease (a permanent UPnP one needs nothing). False when the router no longer takes
/// it.
pub(crate) async fn renew(m: &mut Mapping) -> bool {
    let local = m.local;
    let ext = m.external.port();
    match &mut m.via {
        Via::Upnp { lease: 0, .. } => true,
        Via::Upnp { gateway, lease } => tokio::time::timeout(
            BUDGET,
            gateway.add_port(PortMappingProtocol::UDP, ext, local, *lease, DESCRIPTION),
        )
        .await
        .is_ok_and(|r| r.is_ok()),
        Via::Pmp(p) => tokio::time::timeout(BUDGET, p.renew())
            .await
            .is_ok_and(|r| r.is_ok()),
    }
}

/// Takes the mapping off the router.
pub(crate) async fn remove(m: Mapping) {
    let ext = m.external.port();
    let ok = match m.via {
        Via::Upnp { gateway, .. } => {
            tokio::time::timeout(BUDGET, gateway.remove_port(PortMappingProtocol::UDP, ext))
                .await
                .is_ok_and(|r| r.is_ok())
        }
        Via::Pmp(p) => tokio::time::timeout(BUDGET, p.try_drop())
            .await
            .is_ok_and(|r| r.is_ok()),
    };
    crate::log_at!(
        Debug,
        "together",
        "join: internet port mapping removed from the router: {ok}"
    );
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn the_router_is_guessed_as_the_subnets_first_address() {
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        assert_eq!(
            first_host(ip("192.168.1.37"), ip("255.255.255.0")),
            Some(ip("192.168.1.1"))
        );
        assert_eq!(
            first_host(ip("10.0.5.9"), ip("255.255.0.0")),
            Some(ip("10.0.0.1"))
        );
        assert_eq!(first_host(ip("10.0.0.1"), ip("255.255.255.0")), None);
        assert_eq!(first_host(ip("10.0.0.7"), ip("255.255.255.255")), None);
    }

    /// A router on this computer that answers SSDP and IGD's SOAP calls, and records them.
    pub(crate) struct FakeIgd {
        pub ssdp: SocketAddr,
        pub calls: Arc<Mutex<Vec<String>>>,
    }

    const DESC: &str = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0"><device>
<deviceType>urn:schemas-upnp-org:device:InternetGatewayDevice:1</deviceType>
<deviceList><device><deviceType>urn:schemas-upnp-org:device:WANDevice:1</deviceType>
<deviceList><device><deviceType>urn:schemas-upnp-org:device:WANConnectionDevice:1</deviceType>
<serviceList><service>
<serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
<serviceId>urn:upnp-org:serviceId:WANIPConn1</serviceId>
<SCPDURL>/scpd.xml</SCPDURL><controlURL>/ctl</controlURL><eventSubURL>/evt</eventSubURL>
</service></serviceList></device></deviceList></device></deviceList></device></root>"#;

    fn scpd() -> String {
        let arg =
            |n: &str| format!("<argument><name>{n}</name><direction>in</direction></argument>");
        let add = [
            "NewRemoteHost",
            "NewExternalPort",
            "NewProtocol",
            "NewInternalPort",
            "NewInternalClient",
            "NewEnabled",
            "NewPortMappingDescription",
            "NewLeaseDuration",
        ]
        .map(arg)
        .concat();
        let del = ["NewRemoteHost", "NewExternalPort", "NewProtocol"]
            .map(arg)
            .concat();
        format!(
            r#"<?xml version="1.0"?><scpd xmlns="urn:schemas-upnp-org:service-1-0"><actionList>
<action><name>AddPortMapping</name><argumentList>{add}</argumentList></action>
<action><name>DeletePortMapping</name><argumentList>{del}</argumentList></action>
<action><name>GetExternalIPAddress</name><argumentList><argument><name>NewExternalIPAddress</name><direction>out</direction></argument></argumentList></action>
</actionList></scpd>"#
        )
    }

    fn soap(inner: &str) -> String {
        format!(
            r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body>{inner}</s:Body></s:Envelope>"#
        )
    }

    impl FakeIgd {
        pub(crate) async fn start(external_ip: &'static str) -> FakeIgd {
            use axum::routing::{get, post};
            let calls = Arc::new(Mutex::new(Vec::new()));
            let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let http_addr = http.local_addr().unwrap();
            let c = calls.clone();
            let app = axum::Router::new()
                .route("/desc.xml", get(|| async { DESC }))
                .route("/scpd.xml", get(|| async { scpd() }))
                .route(
                    "/ctl",
                    post(move |headers: axum::http::HeaderMap, body: String| {
                        let c = c.clone();
                        async move {
                            let action = headers
                                .get("soapaction")
                                .and_then(|v| v.to_str().ok())
                                .and_then(|v| v.trim_matches('"').rsplit('#').next())
                                .unwrap_or("")
                                .to_string();
                            let field = |n: &str| {
                                body.split(&format!("<{n}>"))
                                    .nth(1)
                                    .and_then(|r| r.split(&format!("</{n}>")).next())
                                    .unwrap_or("")
                                    .to_string()
                            };
                            let line = match action.as_str() {
                                "AddPortMapping" => format!(
                                    "add {} {} {}:{} lease={}",
                                    field("NewProtocol"),
                                    field("NewExternalPort"),
                                    field("NewInternalClient"),
                                    field("NewInternalPort"),
                                    field("NewLeaseDuration")
                                ),
                                "DeletePortMapping" => format!(
                                    "delete {} {}",
                                    field("NewProtocol"),
                                    field("NewExternalPort")
                                ),
                                a => a.to_string(),
                            };
                            c.lock().unwrap().push(line);
                            let ns = "urn:schemas-upnp-org:service:WANIPConnection:1";
                            soap(&match action.as_str() {
                                "GetExternalIPAddress" => format!(
                                    r#"<u:GetExternalIPAddressResponse xmlns:u="{ns}"><NewExternalIPAddress>{external_ip}</NewExternalIPAddress></u:GetExternalIPAddressResponse>"#
                                ),
                                a => format!(r#"<u:{a}Response xmlns:u="{ns}"></u:{a}Response>"#),
                            })
                        }
                    }),
                );
            tokio::spawn(async move {
                let _ = axum::serve(http, app).await;
            });
            let udp = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let ssdp = udp.local_addr().unwrap();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                while let Ok((n, from)) = udp.recv_from(&mut buf).await {
                    if !String::from_utf8_lossy(&buf[..n]).starts_with("M-SEARCH") {
                        continue;
                    }
                    let answer = format!(
                        "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\nUSN: uuid:fake::urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\nLOCATION: http://{http_addr}/desc.xml\r\n\r\n"
                    );
                    let _ = udp.send_to(answer.as_bytes(), from).await;
                }
            });
            FakeIgd { ssdp, calls }
        }

        pub(crate) fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn upnp_maps_the_ice_port_renews_and_removes_it() {
        let igd = FakeIgd::start("198.51.100.7").await;
        // An address that is not loopback (map() leaves loopback alone); the fake router is asked
        // from 127.0.0.1 regardless, since the search binds to the address given.
        let local: SocketAddr = "127.0.0.1:41234".parse().unwrap();
        let mut m = upnp(local, igd.ssdp).await.expect("mapped");
        assert_eq!(m.external, "198.51.100.7:41234".parse().unwrap());
        assert_eq!(m.how(), "upnp");
        assert!(renew(&mut m).await);
        remove(m).await;
        let calls = igd.calls();
        let lease = LEASE.as_secs();
        assert_eq!(
            calls,
            vec![
                "GetExternalIPAddress".to_string(),
                format!("add UDP 41234 127.0.0.1:41234 lease={lease}"),
                format!("add UDP 41234 127.0.0.1:41234 lease={lease}"),
                "delete UDP 41234".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn no_router_costs_at_most_the_budget() {
        // An SSDP address nobody answers on: nothing mapped, and quickly.
        let quiet = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let t0 = std::time::Instant::now();
        let s = Search {
            ssdp: quiet.local_addr().unwrap(),
            pmp: false,
        };
        assert!(map("192.0.2.10:5000".parse().unwrap(), &s).await.is_none());
        assert!(t0.elapsed() < BUDGET + Duration::from_secs(1));
    }
}

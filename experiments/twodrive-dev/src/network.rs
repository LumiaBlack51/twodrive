//! Application-scoped physical egress, with an explicit system-route fallback.
//!
//! iroh exposes UDP binds but not its relay TCP connector. A private loopback
//! CONNECT adapter supplies the latter without terminating the relay's TLS.
mod diagnostics;
pub use diagnostics::Diagnostics;
mod dns;
mod lookup;
mod proxy;
mod system_dns;
mod system_proxy;

use anyhow::{Context, ensure};
use clap::ValueEnum;
use iroh::{Endpoint, endpoint::Builder};
use netdev::Interface;
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
};
use tokio::net::{TcpSocket, TcpStream, UdpSocket};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum RouteMode {
    /// Prefer physical egress; report and permit system routing on failure.
    #[default]
    Auto,
    /// Apply physical egress constraints with automatic fallback disabled.
    Direct,
    /// Use system routing and configured HTTP proxy settings deliberately.
    System,
}

#[derive(Clone, Debug)]
pub struct Physical {
    pub name: String,
    pub index: u32,
    pub ipv4: Option<Ipv4Addr>,
    pub ipv6: Option<Ipv6Addr>,
    pub dns_servers: Vec<IpAddr>,
    pub allow_fallback: bool,
}

fn usable(ip: IpAddr) -> bool {
    !ip.is_unspecified()
        && !ip.is_loopback()
        && !ip.is_multicast()
        && match ip {
            IpAddr::V4(ip) => !ip.is_link_local() && !fake_ip(ip),
            IpAddr::V6(ip) => !ip.is_unicast_link_local(),
        }
}

pub(crate) fn fake_ip(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 198 && (octets[1] == 18 || octets[1] == 19)
}

pub fn select_physical(
    mut interfaces: Vec<Interface>,
    mode: RouteMode,
    requested: Option<&str>,
) -> anyhow::Result<Physical> {
    if let Some(requested) = requested {
        interfaces.retain(|iface| {
            iface.name == requested
                || iface.friendly_name.as_deref() == Some(requested)
                || iface.index.to_string() == requested
        });
    }
    select_with(interfaces, mode, Interface::is_physical)
}

fn select_with(
    mut interfaces: Vec<Interface>,
    mode: RouteMode,
    is_physical: impl Fn(&Interface) -> bool,
) -> anyhow::Result<Physical> {
    interfaces.retain(|iface| {
        iface.is_up()
            && !iface.is_loopback()
            && is_physical(iface)
            && !iface.is_tun()
            && (iface.ipv4.iter().any(|ip| usable(ip.addr().into()))
                || iface.ipv6.iter().any(|ip| usable(ip.addr().into())))
    });
    interfaces.sort_by_key(|iface| (!iface.default, iface.gateway.is_none(), iface.index));
    let iface = interfaces
        .into_iter()
        .next()
        .context("no usable physical network interface")?;
    Ok(Physical {
        name: iface.name,
        index: iface.index,
        ipv4: iface
            .ipv4
            .iter()
            .map(|ip| ip.addr())
            .find(|ip| usable((*ip).into())),
        ipv6: iface
            .ipv6
            .iter()
            .map(|ip| ip.addr())
            .find(|ip| usable((*ip).into())),
        dns_servers: iface
            .dns_servers
            .into_iter()
            .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
            .collect(),
        allow_fallback: mode == RouteMode::Auto,
    })
}

impl Physical {
    fn source(&self, target: IpAddr) -> anyhow::Result<IpAddr> {
        match target {
            IpAddr::V4(ip) => {
                ensure!(!fake_ip(ip), "fake-IP destination requires system routing");
                self.ipv4
                    .map(IpAddr::V4)
                    .context("physical IPv4 unavailable")
            }
            IpAddr::V6(_) => self
                .ipv6
                .map(IpAddr::V6)
                .context("physical IPv6 unavailable"),
        }
    }

    fn protect(&self, socket: &socket2::SockRef<'_>, target: IpAddr) -> anyhow::Result<()> {
        #[cfg(target_os = "linux")]
        {
            // Some hosts deny SO_BINDTODEVICE to unprivileged processes.
            // Only auto mode permits the weaker source-address constraint.
            if let Err(error) = socket.bind_device(Some(self.name.as_bytes())) {
                ensure!(
                    self.allow_fallback && error.kind() == std::io::ErrorKind::PermissionDenied,
                    "physical interface binding failed: {error}"
                );
                eprintln!(
                    "network: interface binding unavailable; using source address only; system/proxy routing remains possible"
                );
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            use windows_sys::Win32::Networking::WinSock::{
                IP_UNICAST_IF, IPPROTO_IP, IPPROTO_IPV6, IPV6_UNICAST_IF, SOCKET_ERROR,
                WSAGetLastError, setsockopt,
            };
            // Zero means unspecified. IPv4 encodes only a 24-bit index;
            // larger values are not a valid interface-selection request.
            ensure!(
                self.index != 0 && (!target.is_ipv4() || self.index <= 0x00ff_ffff),
                "invalid physical interface index"
            );
            let (level, option, index) = if target.is_ipv4() {
                (IPPROTO_IP, IP_UNICAST_IF, self.index.to_be())
            } else {
                (IPPROTO_IPV6, IPV6_UNICAST_IF, self.index)
            };
            // Winsock expects a network-order interface index for IPv4,
            // and a host-order index for IPv6.
            let result = unsafe {
                setsockopt(
                    socket.as_raw_socket() as usize,
                    level,
                    option,
                    (&index as *const u32).cast(),
                    std::mem::size_of::<u32>() as i32,
                )
            };
            if result == SOCKET_ERROR {
                anyhow::bail!("physical interface binding failed: Winsock {}", unsafe {
                    WSAGetLastError()
                });
            }
        }
        let _ = (socket, target);
        Ok(())
    }

    pub(crate) async fn tcp(&self, target: SocketAddr) -> anyhow::Result<TcpStream> {
        // Numeric loopback stays local and never needs an Internet bypass.
        let local_source = match target.ip() {
            IpAddr::V4(_) => self.ipv4.map(IpAddr::V4),
            IpAddr::V6(_) => self.ipv6.map(IpAddr::V6),
        };
        if target.ip().is_loopback() && !local_source.is_some_and(|ip| ip.is_loopback()) {
            return Ok(TcpStream::connect(target).await?);
        }
        let socket = if target.is_ipv4() {
            TcpSocket::new_v4()?
        } else {
            TcpSocket::new_v6()?
        };
        self.protect(&socket2::SockRef::from(&socket), target.ip())?;
        socket.bind(SocketAddr::new(self.source(target.ip())?, 0))?;
        let stream =
            tokio::time::timeout(std::time::Duration::from_secs(5), socket.connect(target))
                .await??;
        stream.set_nodelay(true)?;
        Ok(stream)
    }

    pub(crate) async fn udp(&self, target: SocketAddr) -> anyhow::Result<UdpSocket> {
        let socket = UdpSocket::bind(SocketAddr::new(self.source(target.ip())?, 0)).await?;
        self.protect(&socket2::SockRef::from(&socket), target.ip())?;
        socket.connect(target).await?;
        Ok(socket)
    }

    pub(crate) fn fallback(&self, component: &str, reason: &str) {
        eprintln!("network: route=system/proxy fallback; component={component}; {reason}");
    }
}

/// Configure DNS, UDP, relay TCP and signed address publication together.
pub(crate) async fn configure(
    builder: Builder,
    mode: RouteMode,
    requested: Option<&str>,
    discovery: bool,
    relay_only: bool,
    relays: &[String],
) -> anyhow::Result<Builder> {
    if mode == RouteMode::System {
        eprintln!("network: route=system/proxy; physical bypass disabled");
        let builder = builder.dns_resolver(iroh::dns::DnsResolver::custom(
            system_dns::SystemResolver::default(),
        ));
        return Ok(system_proxy::configure(builder, relays));
    }
    let physical = Arc::new(select_physical(netdev::get_interfaces(), mode, requested)?);
    let backend = Arc::new(dns::Backend::new(physical.clone()));
    let adapter = proxy::Adapter::start(physical.clone(), backend.clone()).await?;
    let resolver = iroh::dns::DnsResolver::custom(dns::Resolver {
        backend: backend.clone(),
        _adapter: adapter.clone(),
    });
    let mut builder = builder
        .clear_ip_transports()
        .clear_address_lookup()
        .dns_resolver(resolver.clone())
        .proxy_url(adapter.url.clone());
    if !relay_only {
        if let Some(ip) = physical.ipv4 {
            builder = builder.bind_addr(SocketAddr::new(ip.into(), 0))?;
        }
        if let Some(ip) = physical.ipv6 {
            builder = builder.bind_addr(SocketAddr::new(ip.into(), 0))?;
        }
    }
    if discovery {
        builder = builder
            .address_lookup(iroh::address_lookup::DnsAddressLookup::n0_dns())
            .address_lookup(lookup::PublisherBuilder {
                url: adapter.url.clone(),
                allow_fallback: physical.allow_fallback,
            });
    }
    eprintln!(
        "network: route=physical interface={} fallback={}",
        physical.name,
        if physical.allow_fallback {
            "reported-system/proxy"
        } else {
            "disabled"
        }
    );
    Ok(builder)
}

/// Close a failed attempt before registering the same identity on another path.
pub(crate) async fn close_failed(endpoint: Endpoint) {
    endpoint.close().await;
}

/// Own the physical DNS/CONNECT adapter for a synchronous HTTPS client.
/// No fallback occurs inside this connector; callers choose fallback before writes.
pub struct HttpConnector {
    pub proxy: reqwest::Proxy,
    pub interface: String,
    _adapter: Arc<proxy::Adapter>,
    _runtime: tokio::runtime::Runtime,
}
impl HttpConnector {
    pub fn direct() -> anyhow::Result<Self> {
        let physical = Arc::new(select_physical(
            netdev::get_interfaces(),
            RouteMode::Direct,
            None,
        )?);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let adapter = runtime.block_on(proxy::Adapter::start(
            physical.clone(),
            Arc::new(dns::Backend::new(physical.clone())),
        ))?;
        let proxy = reqwest::Proxy::https(adapter.url.as_str())?;
        Ok(Self {
            proxy,
            interface: physical.name.clone(),
            _adapter: adapter,
            _runtime: runtime,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn local(allow_fallback: bool) -> Arc<Physical> {
        Arc::new(Physical {
            name: "lo".into(),
            index: 1,
            ipv4: Some(Ipv4Addr::new(127, 0, 0, 2)),
            ipv6: None,
            dns_servers: vec![],
            allow_fallback,
        })
    }
    #[test]
    fn empty_selection_and_fake_ip_cannot_claim_physical_egress() {
        assert!(select_physical(vec![], RouteMode::Auto, None).is_err());
        assert!(
            select_physical(
                netdev::get_interfaces(),
                RouteMode::Auto,
                Some("twodrive-nonexistent-interface-for-test")
            )
            .is_err()
        );
        assert!(
            local(false)
                .source(Ipv4Addr::new(198, 18, 0, 1).into())
                .is_err()
        );
        assert!(
            local(false)
                .source(Ipv4Addr::new(198, 19, 255, 255).into())
                .is_err()
        );
        assert!(
            local(false)
                .source(Ipv4Addr::new(203, 0, 113, 1).into())
                .is_ok()
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn virtual_default_down_and_fake_ip_interfaces_are_not_selected() {
        let mut wifi = Interface::dummy();
        wifi.name = "wifi".into();
        wifi.index = 3;
        wifi.flags = libc::IFF_UP as u32;
        wifi.ipv4.push("192.0.2.2/24".parse().unwrap());
        let mut virtual_default = wifi.clone();
        virtual_default.name = "tun".into();
        virtual_default.default = true;
        let mut down = wifi.clone();
        down.name = "down".into();
        down.default = true;
        down.flags = 0;
        let mut fake = wifi.clone();
        fake.name = "fake".into();
        fake.default = true;
        fake.ipv4 = vec!["198.18.0.1/24".parse().unwrap()];
        let selected = select_with(
            vec![virtual_default, down, fake, wifi.clone()],
            RouteMode::Auto,
            |iface| iface.name != "tun",
        )
        .unwrap();
        assert_eq!(selected.name, "wifi");
        assert!(selected.allow_fallback);
        let mut preferred = wifi.clone();
        preferred.name = "preferred".into();
        preferred.default = true;
        let selected = select_with(vec![wifi, preferred], RouteMode::Direct, |_| true).unwrap();
        assert_eq!(selected.name, "preferred");
        assert!(!selected.allow_fallback);
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn tcp_and_dns_udp_use_the_selected_source_address() {
        let physical = local(false);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        // Baseline: the old unbound connector violates the selected-source
        // invariant on this same listener.
        let unbound = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (_, source) = listener.accept().await.unwrap();
        assert_ne!(source.ip(), Ipv4Addr::new(127, 0, 0, 2));
        drop(unbound);
        let stream = physical.tcp(listener.local_addr().unwrap()).await.unwrap();
        let (_, source) = listener.accept().await.unwrap();
        assert_eq!(source.ip(), Ipv4Addr::new(127, 0, 0, 2));
        drop(stream);
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let socket = physical.udp(listener.local_addr().unwrap()).await.unwrap();
        socket.send(b"test").await.unwrap();
        let mut data = [0; 8];
        let (size, source) = listener.recv_from(&mut data).await.unwrap();
        assert_eq!(size, 4);
        assert_eq!(source.ip(), Ipv4Addr::new(127, 0, 0, 2));
    }

    #[tokio::test]
    async fn ipv6_only_interface_keeps_numeric_loopback_local() {
        let physical = Physical {
            ipv4: None,
            ipv6: Some(Ipv6Addr::LOCALHOST),
            ..(*local(false)).clone()
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        assert!(physical.tcp(listener.local_addr().unwrap()).await.is_ok());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn winsock_tcp_and_dns_udp_bind_the_requested_interface() {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Networking::WinSock::{IP_UNICAST_IF, IPPROTO_IP, getsockopt};
        let iface = netdev::get_interfaces()
            .into_iter()
            .find(|iface| iface.is_up() && !iface.ipv4.is_empty())
            .expect("Windows must have an IPv4 adapter for socket tests");
        let ip = iface.ipv4[0].addr();
        let physical = Physical {
            name: iface.name,
            index: iface.index,
            ipv4: Some(ip),
            ipv6: None,
            dns_servers: vec![],
            allow_fallback: false,
        };
        fn assert_index(socket: &socket2::SockRef<'_>, index: u32) {
            let mut value = 0u32;
            let mut size = std::mem::size_of::<u32>() as i32;
            let result = unsafe {
                getsockopt(
                    socket.as_raw_socket() as usize,
                    IPPROTO_IP,
                    IP_UNICAST_IF,
                    (&mut value as *mut u32).cast(),
                    &mut size,
                )
            };
            assert_eq!(result, 0);
            // Unlike setsockopt, getsockopt returns host byte order.
            assert_eq!(value, index);
        }
        let tcp = TcpSocket::new_v4().unwrap();
        physical
            .protect(&socket2::SockRef::from(&tcp), ip.into())
            .unwrap();
        tcp.bind(SocketAddr::new(ip.into(), 0)).unwrap();
        assert_eq!(tcp.local_addr().unwrap().ip(), ip);
        assert_index(&socket2::SockRef::from(&tcp), physical.index);
        let udp = UdpSocket::bind(SocketAddr::new(ip.into(), 0))
            .await
            .unwrap();
        physical
            .protect(&socket2::SockRef::from(&udp), ip.into())
            .unwrap();
        assert_index(&socket2::SockRef::from(&udp), physical.index);
        for index in [0, u32::MAX] {
            let invalid = Physical {
                index,
                ..physical.clone()
            };
            assert!(
                invalid
                    .protect(&socket2::SockRef::from(&tcp), ip.into())
                    .is_err()
            );
        }
    }
}

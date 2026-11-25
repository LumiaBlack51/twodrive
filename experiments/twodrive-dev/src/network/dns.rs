use super::{Physical, proxy::Adapter};
use anyhow::{Context, ensure};
use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};
use iroh::dns::{BoxIter, DnsError, DnsResolver, TxtRecordData};
use n0_error::e;
use simple_dns::{CLASS, Name, Packet, PacketFlag, Question, RCODE, TYPE, rdata::RData};
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Records = Vec<RData<'static>>;
type Cache = HashMap<(String, TYPE), (Instant, Records)>;

#[derive(Debug)]
pub struct Backend {
    pub physical: Arc<Physical>,
    cache: Mutex<Cache>,
}

impl Backend {
    pub fn new(physical: Arc<Physical>) -> Self {
        Self {
            physical,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub async fn query(&self, host: &str, kind: TYPE) -> anyhow::Result<Records> {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let key = (host.clone(), kind);
        if let Some((expires, records)) = self.cache.lock().unwrap().get(&key)
            && *expires > Instant::now()
        {
            return Ok(records.clone());
        }
        let id = rand::random::<u16>();
        let mut query = Packet::new_query(id);
        query.set_flags(PacketFlag::RECURSION_DESIRED);
        query.questions.push(Question::new(
            Name::new(&host)?,
            kind.into(),
            CLASS::IN.into(),
            false,
        ));
        let bytes = query.build_bytes_vec()?;
        let mut servers = self.physical.dns_servers.clone();
        // DHCP nameservers are tried too, but never the local stub/fake-IP DNS.
        servers.extend([
            "1.1.1.1".parse::<IpAddr>().unwrap(),
            "8.8.8.8".parse::<IpAddr>().unwrap(),
            "2606:4700:4700::1111".parse::<IpAddr>().unwrap(),
        ]);
        servers.sort();
        servers.dedup();
        let calls = FuturesUnordered::new();
        for server in servers {
            let bytes = &bytes;
            let host = &host;
            calls.push(async move {
                tokio::time::timeout(Duration::from_secs(3), async {
                    let response =
                        exchange(&self.physical, SocketAddr::new(server, 53), bytes).await?;
                    validate_response(&response, id, host, kind)
                })
                .await
                .context("physical DNS timeout")?
            });
        }
        let (records, ttl) = prefer_records(calls).await?;
        if ttl > 0 {
            let mut cache = self.cache.lock().unwrap();
            if cache.len() >= 512 {
                cache.clear();
            }
            cache.insert(
                key,
                (
                    Instant::now() + Duration::from_secs(ttl.min(300).into()),
                    records.clone(),
                ),
            );
        }
        Ok(records)
    }

    pub async fn ips(&self, host: &str) -> anyhow::Result<Vec<IpAddr>> {
        if let Ok(ip) = host.parse() {
            return Ok(vec![ip]);
        }
        let (v4, v6) = tokio::join!(self.query(host, TYPE::A), self.query(host, TYPE::AAAA));
        let lookup_failed = v4.is_err() || v6.is_err();
        let mut ips = Vec::new();
        for data in v4.into_iter().flatten().chain(v6.into_iter().flatten()) {
            match data {
                RData::A(data) => ips.push(Ipv4Addr::from(data.address).into()),
                RData::AAAA(data) => ips.push(Ipv6Addr::from(data.address).into()),
                _ => {}
            }
        }
        if ips.is_empty() && lookup_failed && self.physical.allow_fallback {
            self.physical.fallback("DNS", "physical DNS lookup failed");
            ips.extend(
                tokio::net::lookup_host((host, 0))
                    .await?
                    .map(|addr| addr.ip()),
            );
        }
        ensure!(!ips.is_empty(), "no resolved destination addresses");
        Ok(ips)
    }
}

// Recursive servers may have different cached generations during publication.
// A quick NXDOMAIN must not cancel another server's available relay record.
// Every candidate already has a bounded transport deadline. Return a valid
// negative only after the remaining candidates fail or also return negatives.
async fn prefer_records<F>(mut calls: FuturesUnordered<F>) -> anyhow::Result<(Records, u32)>
where
    F: std::future::Future<Output = anyhow::Result<(Records, u32)>>,
{
    let mut negative = None;
    while let Some(result) = calls.next().await {
        if let Ok((records, ttl)) = result {
            if !records.is_empty() {
                return Ok((records, ttl));
            }
            // Do not retain a negative longer than any participating server.
            negative = Some(negative.map_or(ttl, |previous: u32| previous.min(ttl)));
        }
    }
    if let Some(ttl) = negative {
        return Ok((Vec::new(), ttl));
    }
    anyhow::bail!("physical DNS resolution failed")
}

async fn exchange(
    physical: &Physical,
    server: SocketAddr,
    bytes: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let socket = physical.udp(server).await?;
    socket.send(bytes).await?;
    let mut response = vec![0; 65535];
    let count = socket.recv(&mut response).await?;
    response.truncate(count);
    // Retry large TXT answers over the same constrained TCP path. Never
    // accept a truncated packet as an empty or partial discovery result.
    if response.len() >= 12 && response[..2] == bytes[..2] && response[2] & 0x82 == 0x82 {
        let mut stream = physical.tcp(server).await?;
        stream.write_u16(bytes.len().try_into()?).await?;
        stream.write_all(bytes).await?;
        response.resize(stream.read_u16().await?.into(), 0);
        stream.read_exact(&mut response).await?;
    }
    Ok(response)
}

fn validate_response(
    bytes: &[u8],
    id: u16,
    host: &str,
    kind: TYPE,
) -> anyhow::Result<(Records, u32)> {
    let packet = Packet::parse(bytes)?;
    ensure!(
        packet.id() == id && packet.has_flags(PacketFlag::RESPONSE),
        "DNS transaction mismatch"
    );
    ensure!(
        !packet.has_flags(PacketFlag::TRUNCATION)
            && matches!(packet.rcode(), RCODE::NoError | RCODE::NameError),
        "DNS response unusable"
    );
    ensure!(packet.questions.len() == 1, "DNS question count mismatch");
    let question = &packet.questions[0];
    ensure!(
        question
            .qname
            .to_string()
            .trim_end_matches('.')
            .eq_ignore_ascii_case(host)
            && question.qtype == kind.into()
            && question.qclass == CLASS::IN.into(),
        "DNS question mismatch"
    );
    // RFC 2308: NXDOMAIN/NODATA is a valid negative response, not a
    // transport failure. Respect the SOA's negative TTL (including zero).
    let negative_ttl = packet
        .name_servers
        .iter()
        .filter_map(|record| {
            if let RData::SOA(soa) = &record.rdata {
                Some(record.ttl.min(soa.minimum).min(300))
            } else {
                None
            }
        })
        .min()
        .unwrap_or(0);
    if packet.rcode() == RCODE::NameError {
        return Ok((Vec::new(), negative_ttl));
    }
    let mut records = Vec::new();
    let mut ttl = 300;
    let mut owner = host.trim_end_matches('.').to_ascii_lowercase();
    let mut visited = HashSet::new();
    loop {
        ensure!(
            visited.len() < 16 && visited.insert(owner.clone()),
            "DNS alias loop or excessive depth"
        );
        let alias = packet.answers.iter().find_map(|answer| {
            if answer.class == CLASS::IN
                && answer
                    .name
                    .to_string()
                    .trim_end_matches('.')
                    .eq_ignore_ascii_case(&owner)
                && let RData::CNAME(name) = &answer.rdata
            {
                Some((name.to_string(), answer.ttl))
            } else {
                None
            }
        });
        let Some((name, alias_ttl)) = alias else {
            break;
        };
        ttl = ttl.min(alias_ttl);
        owner = name.trim_end_matches('.').to_ascii_lowercase();
    }
    for answer in packet.answers {
        // Ignore unrelated additional owners, even when the transaction and
        // question match. Follow only the queried name's CNAME chain.
        let matches = matches!(
            (&answer.rdata, kind),
            (RData::A(_), TYPE::A) | (RData::AAAA(_), TYPE::AAAA) | (RData::TXT(_), TYPE::TXT)
        );
        if !matches
            || answer.class != CLASS::IN
            || !answer
                .name
                .to_string()
                .trim_end_matches('.')
                .eq_ignore_ascii_case(&owner)
        {
            continue;
        }
        if let RData::A(value) = &answer.rdata {
            ensure!(
                !super::fake_ip(Ipv4Addr::from(value.address)),
                "DNS returned fake-IP range"
            );
        }
        ttl = ttl.min(answer.ttl);
        records.push(answer.rdata.into_owned());
    }
    if records.is_empty() {
        ttl = negative_ttl;
    }
    Ok((records, ttl))
}

#[derive(Clone, Debug)]
pub struct Resolver {
    pub backend: Arc<Backend>,
    pub _adapter: Arc<Adapter>,
}

impl iroh::dns::Resolver for Resolver {
    fn lookup_ipv4(
        &self,
        host: String,
    ) -> futures_util::future::BoxFuture<'static, Result<BoxIter<Ipv4Addr>, DnsError>> {
        let backend = self.backend.clone();
        async move {
            match backend.query(&host, TYPE::A).await {
                Ok(records) => Ok(Box::new(records.into_iter().filter_map(|data| {
                    if let RData::A(a) = data {
                        Some(Ipv4Addr::from(a.address))
                    } else {
                        None
                    }
                })) as BoxIter<_>),
                Err(_) if backend.physical.allow_fallback => {
                    backend
                        .physical
                        .fallback("DNS A", "physical DNS lookup failed");
                    let values = DnsResolver::new()
                        .lookup_ipv4(host, Duration::from_secs(3))
                        .await?
                        .filter_map(|ip| {
                            if let IpAddr::V4(ip) = ip {
                                Some(ip)
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>();
                    Ok(Box::new(values.into_iter()) as BoxIter<_>)
                }
                Err(_) => Err(e!(DnsError::NoResponse)),
            }
        }
        .boxed()
    }
    fn lookup_ipv6(
        &self,
        host: String,
    ) -> futures_util::future::BoxFuture<'static, Result<BoxIter<Ipv6Addr>, DnsError>> {
        let backend = self.backend.clone();
        async move {
            match backend.query(&host, TYPE::AAAA).await {
                Ok(records) => Ok(Box::new(records.into_iter().filter_map(|data| {
                    if let RData::AAAA(a) = data {
                        Some(Ipv6Addr::from(a.address))
                    } else {
                        None
                    }
                })) as BoxIter<_>),
                Err(_) if backend.physical.allow_fallback => {
                    backend
                        .physical
                        .fallback("DNS AAAA", "physical DNS lookup failed");
                    let values = DnsResolver::new()
                        .lookup_ipv6(host, Duration::from_secs(3))
                        .await?
                        .filter_map(|ip| {
                            if let IpAddr::V6(ip) = ip {
                                Some(ip)
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>();
                    Ok(Box::new(values.into_iter()) as BoxIter<_>)
                }
                Err(_) => Err(e!(DnsError::NoResponse)),
            }
        }
        .boxed()
    }
    fn lookup_txt(
        &self,
        host: String,
    ) -> futures_util::future::BoxFuture<'static, Result<BoxIter<TxtRecordData>, DnsError>> {
        let backend = self.backend.clone();
        async move {
            match backend.query(&host, TYPE::TXT).await {
                Ok(records) => Ok(Box::new(records.into_iter().filter_map(|data| {
                    if let RData::TXT(txt) = data {
                        Some(
                            txt.iter_raw()
                                .map(|(key, value)| {
                                    let mut bytes = key.to_vec();
                                    if let Some(value) = value {
                                        bytes.push(b'=');
                                        bytes.extend_from_slice(value);
                                    }
                                    bytes.into_boxed_slice()
                                })
                                .collect::<TxtRecordData>(),
                        )
                    } else {
                        None
                    }
                })) as BoxIter<_>),
                Err(_) if backend.physical.allow_fallback => {
                    backend
                        .physical
                        .fallback("DNS TXT", "physical DNS lookup failed");
                    let values = DnsResolver::new()
                        .lookup_txt(host, Duration::from_secs(3))
                        .await?
                        .collect::<Vec<_>>();
                    Ok(Box::new(values.into_iter()) as BoxIter<_>)
                }
                Err(_) => Err(e!(DnsError::NoResponse)),
            }
        }
        .boxed()
    }
    fn clear_cache(&self) {
        self.backend.cache.lock().unwrap().clear();
    }
    fn reset(&self) -> Box<dyn iroh::dns::Resolver> {
        self.clear_cache();
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use simple_dns::{CLASS, ResourceRecord, rdata::A};
    #[tokio::test]
    async fn fast_negative_does_not_hide_later_positive_dns_record() {
        let calls = FuturesUnordered::new();
        let (release, waited) = tokio::sync::oneshot::channel();
        calls.push(
            async move {
                release.send(()).unwrap();
                Ok((Vec::new(), 0))
            }
            .boxed(),
        );
        calls.push(
            async move {
                waited.await.unwrap();
                tokio::task::yield_now().await;
                Ok((
                    vec![RData::A(A {
                        address: u32::from(Ipv4Addr::new(203, 0, 113, 2)),
                    })],
                    30,
                ))
            }
            .boxed(),
        );
        let (records, ttl) = prefer_records(calls).await.unwrap();
        assert_eq!(
            records.len(),
            1,
            "a stale negative hid the published record"
        );
        assert_eq!(ttl, 30);
    }

    #[tokio::test]
    async fn negative_dns_remains_valid_when_no_server_has_records() {
        let calls = FuturesUnordered::new();
        for ttl in [60, 0] {
            calls.push(async move { Ok((Vec::new(), ttl)) }.boxed());
        }
        calls.push(async { anyhow::bail!("controlled timeout") }.boxed());
        let (records, ttl) = prefer_records(calls).await.unwrap();
        assert!(records.is_empty());
        assert_eq!(ttl, 0);
        let failures = FuturesUnordered::new();
        failures.push(async { anyhow::bail!("controlled timeout") });
        assert!(prefer_records(failures).await.is_err());
    }

    #[test]
    fn valid_nxdomain_is_not_a_physical_route_failure() {
        let mut response = Packet::new_reply(42);
        *response.rcode_mut() = RCODE::NameError;
        response.questions.push(Question::new(
            Name::new("missing.example").unwrap(),
            TYPE::TXT.into(),
            CLASS::IN.into(),
            false,
        ));
        let bytes = response.build_bytes_vec().unwrap();
        let (records, ttl) = validate_response(&bytes, 42, "missing.example", TYPE::TXT)
            .expect("NXDOMAIN is a valid negative response");
        assert!(records.is_empty());
        assert_eq!(ttl, 0, "negative answers without SOA must not be cached");
        assert!(validate_response(&bytes, 41, "missing.example", TYPE::TXT).is_err());
        assert!(validate_response(&bytes, 42, "other.example", TYPE::TXT).is_err());
        response.name_servers.push(ResourceRecord::new(
            Name::new("example").unwrap(),
            CLASS::IN,
            60,
            RData::SOA(simple_dns::rdata::SOA {
                mname: Name::new("ns.example").unwrap(),
                rname: Name::new("hostmaster.example").unwrap(),
                serial: 1,
                refresh: 60,
                retry: 60,
                expire: 60,
                minimum: 30,
            }),
        ));
        assert_eq!(
            validate_response(
                &response.build_bytes_vec().unwrap(),
                42,
                "missing.example",
                TYPE::TXT
            )
            .unwrap()
            .1,
            30
        );
        if let RData::SOA(soa) = &mut response.name_servers[0].rdata {
            soa.minimum = 0;
        }
        assert_eq!(
            validate_response(
                &response.build_bytes_vec().unwrap(),
                42,
                "missing.example",
                TYPE::TXT
            )
            .unwrap()
            .1,
            0
        );
        *response.rcode_mut() = RCODE::ServerFailure;
        assert!(
            validate_response(
                &response.build_bytes_vec().unwrap(),
                42,
                "missing.example",
                TYPE::TXT
            )
            .is_err()
        );
    }
    #[test]
    fn rejects_fake_ip_and_wrong_dns_transaction() {
        let mut response = Packet::new_reply(42);
        response.questions.push(Question::new(
            Name::new("relay.example").unwrap(),
            TYPE::A.into(),
            CLASS::IN.into(),
            false,
        ));
        response.answers.push(ResourceRecord::new(
            Name::new("relay.example").unwrap(),
            CLASS::IN,
            60,
            RData::A(A {
                address: u32::from(Ipv4Addr::new(198, 18, 0, 2)),
            }),
        ));
        let bytes = response.build_bytes_vec().unwrap();
        assert!(validate_response(&bytes, 42, "relay.example", TYPE::A).is_err());
        response.answers.clear();
        response.answers.push(ResourceRecord::new(
            Name::new("relay.example").unwrap(),
            CLASS::IN,
            60,
            RData::A(A {
                address: u32::from(Ipv4Addr::new(203, 0, 113, 2)),
            }),
        ));
        let bytes = response.build_bytes_vec().unwrap();
        assert!(validate_response(&bytes, 41, "relay.example", TYPE::A).is_err());
        assert!(validate_response(&bytes, 42, "other.example", TYPE::A).is_err());
        assert_eq!(
            validate_response(&bytes, 42, "relay.example", TYPE::A)
                .unwrap()
                .0
                .len(),
            1
        );
    }

    #[test]
    fn follows_cname_but_does_not_accept_unrelated_answers() {
        use simple_dns::rdata::CNAME;
        let mut response = Packet::new_reply(42);
        response.questions.push(Question::new(
            Name::new("relay.example").unwrap(),
            TYPE::A.into(),
            CLASS::IN.into(),
            false,
        ));
        response.answers.push(ResourceRecord::new(
            Name::new("unrelated.example").unwrap(),
            CLASS::IN,
            60,
            RData::A(A {
                address: u32::from(Ipv4Addr::new(203, 0, 113, 3)),
            }),
        ));
        let bytes = response.build_bytes_vec().unwrap();
        assert!(
            validate_response(&bytes, 42, "relay.example", TYPE::A)
                .unwrap()
                .0
                .is_empty()
        );
        response.answers.push(ResourceRecord::new(
            Name::new("relay.example").unwrap(),
            CLASS::IN,
            5,
            RData::CNAME(CNAME(Name::new("canonical.example").unwrap())),
        ));
        response.answers.push(ResourceRecord::new(
            Name::new("canonical.example").unwrap(),
            CLASS::IN,
            60,
            RData::A(A {
                address: u32::from(Ipv4Addr::new(203, 0, 113, 2)),
            }),
        ));
        let (records, ttl) = validate_response(
            &response.build_bytes_vec().unwrap(),
            42,
            "relay.example",
            TYPE::A,
        )
        .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(ttl, 5);
        response.answers.push(ResourceRecord::new(
            Name::new("canonical.example").unwrap(),
            CLASS::IN,
            5,
            RData::CNAME(CNAME(Name::new("relay.example").unwrap())),
        ));
        assert!(
            validate_response(
                &response.build_bytes_vec().unwrap(),
                42,
                "relay.example",
                TYPE::A
            )
            .is_err()
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn truncated_dns_retries_tcp_with_the_same_source_constraint() {
        let physical = Physical {
            name: "lo".into(),
            index: 1,
            ipv4: Some(Ipv4Addr::new(127, 0, 0, 2)),
            ipv6: None,
            dns_servers: vec![],
            allow_fallback: false,
        };
        // Port zero allocates in one protocol's namespace only. Keep both
        // listeners bound before starting the fixture.
        let mut attempts = 0;
        let (tcp, udp, addr) = loop {
            let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = tcp.local_addr().unwrap();
            match tokio::net::UdpSocket::bind(addr).await {
                Ok(udp) => break (tcp, udp, addr),
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && attempts < 32 => {
                    attempts += 1;
                }
                Err(e) => panic!("could not bind DNS fixture sockets: {e}"),
            }
        };
        let mut query = Packet::new_query(42);
        query.questions.push(Question::new(
            Name::new("relay.example").unwrap(),
            TYPE::A.into(),
            CLASS::IN.into(),
            false,
        ));
        let bytes = query.build_bytes_vec().unwrap();
        let service = tokio::spawn(async move {
            let mut buffer = [0; 512];
            let (_, source) = udp.recv_from(&mut buffer).await.unwrap();
            assert_eq!(source.ip(), Ipv4Addr::new(127, 0, 0, 2));
            let mut truncated = Packet::new_reply(42);
            truncated.set_flags(PacketFlag::TRUNCATION);
            udp.send_to(&truncated.build_bytes_vec().unwrap(), source)
                .await
                .unwrap();
            let (mut stream, source) = tcp.accept().await.unwrap();
            assert_eq!(source.ip(), Ipv4Addr::new(127, 0, 0, 2));
            let len = stream.read_u16().await.unwrap();
            let mut request = vec![0; usize::from(len)];
            stream.read_exact(&mut request).await.unwrap();
            let request = Packet::parse(&request).unwrap();
            let mut reply = Packet::new_reply(request.id());
            reply.questions = request.questions;
            reply.answers.push(ResourceRecord::new(
                Name::new("relay.example").unwrap(),
                CLASS::IN,
                60,
                RData::A(A {
                    address: u32::from(Ipv4Addr::new(203, 0, 113, 2)),
                }),
            ));
            let bytes = reply.build_bytes_vec().unwrap();
            stream
                .write_u16(bytes.len().try_into().unwrap())
                .await
                .unwrap();
            stream.write_all(&bytes).await.unwrap();
        });
        let response =
            tokio::time::timeout(Duration::from_secs(3), exchange(&physical, addr, &bytes))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(
            validate_response(&response, 42, "relay.example", TYPE::A)
                .unwrap()
                .0
                .len(),
            1
        );
        service.await.unwrap();
    }
}

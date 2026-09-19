//! Isolated agent wire service and real loopback SSH servers. No user credentials.

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use russh::keys::signature::Signer as _;
use russh::keys::ssh_key::encoding::Encode as _;
use russh::keys::ssh_key::{Algorithm, HashAlg, PrivateKey, Signature};
use russh::server::{self, Auth};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use super::*;
use crate::ssh_profiles::{SshAuthMode, SshProfileAuth};
use crate::ssh_session::route::{ResolvedRoute, RouteTransport};
use crate::ssh_session::{NoopSshEventHost, SshTestRequest};

type Factory = dyn Fn(Endpoint, &str) -> Result<DynamicAgent, SessionError> + Send + Sync;
tokio::task_local! { static FACTORY: Box<Factory>; }

pub(super) async fn with_factory<T>(
    factory: impl Fn(Endpoint, &str) -> Result<DynamicAgent, SessionError> + Send + Sync + 'static,
    future: impl Future<Output = T>,
) -> T {
    FACTORY.scope(Box::new(factory), future).await
}

pub(super) fn connect(endpoint: Endpoint, destination: &str) -> Result<DynamicAgent, SessionError> {
    FACTORY
        .try_with(|factory| factory(endpoint, destination))
        .unwrap_or_else(|_| Err("no agent installed in this test scope".into()))
}

pub(super) fn check(future: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        tokio::time::timeout(Duration::from_secs(40), future).await.expect("bounded SSH test");
    });
}

pub(super) fn key() -> Arc<PrivateKey> {
    Arc::new(PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap())
}

#[derive(Clone)]
pub(super) struct Identity {
    pub advertised: AgentIdentity,
    pub key: Arc<PrivateKey>,
}

impl Identity {
    pub fn plain(key: Arc<PrivateKey>) -> Self {
        Self { advertised: key.public_key().clone().into(), key }
    }

    pub fn certificate(key: Arc<PrivateKey>) -> Self {
        let ca = self::key();
        let mut builder = russh::keys::ssh_key::certificate::Builder::new_with_random_nonce(
            &mut rand::rng(),
            key.public_key(),
            0,
            u64::MAX,
        )
        .unwrap();
        builder.cert_type(russh::keys::ssh_key::certificate::CertType::User).unwrap();
        builder.valid_principal("fixture-user").unwrap();
        let certificate = builder.sign(ca.as_ref()).unwrap();
        Self { advertised: certificate.into(), key }
    }
}

#[derive(Clone, Copy, Default)]
pub(super) enum AgentBehavior {
    #[default]
    Normal,
    HangIdentities,
    RefuseSignature,
    HangSignature,
    DisconnectSignature,
}

#[derive(Default)]
pub(super) struct AgentStats {
    pub queries: AtomicUsize,
    pub signs: AtomicUsize,
    pub flags: Mutex<Vec<u32>>,
}

#[derive(Clone, Default)]
pub(super) struct Agent {
    pub identities: Vec<Identity>,
    pub behavior: AgentBehavior,
    pub stats: Arc<AgentStats>,
}

impl Agent {
    pub fn new(identities: Vec<Identity>) -> Self {
        Self { identities, ..Default::default() }
    }

    pub fn connect(&self) -> DynamicAgent {
        let (client, server) = tokio::io::duplex(256 * 1024);
        tokio::spawn(self.clone().serve(server));
        AgentClient::connect(client).dynamic()
    }

    pub async fn serve(self, mut stream: impl AsyncRead + AsyncWrite + Unpin) {
        while let Ok(length) = stream.read_u32().await {
            assert!(length <= 256 * 1024);
            let mut frame = vec![0; length as usize];
            if stream.read_exact(&mut frame).await.is_err() {
                return;
            }
            let mut response = Vec::new();
            match frame[0] {
                11 => {
                    self.stats.queries.fetch_add(1, Ordering::SeqCst);
                    if matches!(self.behavior, AgentBehavior::HangIdentities) {
                        std::future::pending::<()>().await;
                    }
                    response.push(12);
                    response.extend_from_slice(&(self.identities.len() as u32).to_be_bytes());
                    for identity in &self.identities {
                        put_string(&mut response, &identity_blob(&identity.advertised).unwrap());
                        put_string(&mut response, b"fixture");
                    }
                },
                13 => {
                    self.stats.signs.fetch_add(1, Ordering::SeqCst);
                    match self.behavior {
                        AgentBehavior::RefuseSignature => response.push(5),
                        AgentBehavior::DisconnectSignature => return,
                        AgentBehavior::HangSignature => std::future::pending::<()>().await,
                        _ => {
                            let mut request = &frame[1..];
                            let blob = take_string(&mut request);
                            let data = take_string(&mut request);
                            let flags = u32::from_be_bytes(request.try_into().unwrap());
                            self.stats.flags.lock().unwrap().push(flags);
                            let key = &self
                                .identities
                                .iter()
                                .find(|identity| {
                                    identity_blob(&identity.advertised).unwrap() == blob
                                })
                                .expect("only advertised identities may be signed")
                                .key;
                            let signature: Signature = if let Some(rsa) = key.key_data().rsa() {
                                let hash = match flags {
                                    2 => Some(HashAlg::Sha256),
                                    4 => Some(HashAlg::Sha512),
                                    0 => None,
                                    _ => panic!("invalid RSA flags"),
                                };
                                (rsa, hash).try_sign(data).unwrap()
                            } else {
                                assert_eq!(flags, 0);
                                key.try_sign(data).unwrap()
                            };
                            let mut encoded = Vec::new();
                            signature.encode(&mut encoded).unwrap();
                            response.push(14);
                            put_string(&mut response, &encoded);
                        },
                    }
                },
                _ => panic!("unexpected agent operation"),
            }
            if stream.write_u32(response.len() as u32).await.is_err()
                || stream.write_all(&response).await.is_err()
                || stream.flush().await.is_err()
            {
                return;
            }
        }
    }
}

fn put_string(frame: &mut Vec<u8>, bytes: &[u8]) {
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend_from_slice(bytes);
}

fn take_string<'a>(frame: &mut &'a [u8]) -> &'a [u8] {
    let length = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
    let value = &frame[4..4 + length];
    *frame = &frame[4 + length..];
    value
}

#[derive(Clone, Default)]
pub(super) struct ServerOptions {
    pub key: Option<PublicKey>,
    pub password: Option<String>,
    pub second_factor: bool,
    pub rsa_hash: Option<HashAlg>,
    pub forward: Option<(String, u16)>,
}

#[derive(Default)]
pub(super) struct ServerStats {
    pub offered: Mutex<Vec<PublicKey>>,
    pub passwords: AtomicUsize,
    pub accepted: AtomicUsize,
    pub certificates: AtomicUsize,
    pub closed: AtomicUsize,
}

#[derive(Clone)]
struct Server {
    options: ServerOptions,
    stats: Arc<ServerStats>,
}

impl server::Handler for Server {
    type Error = russh::Error;

    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        self.stats.offered.lock().unwrap().push(key.clone());
        Ok(
            if self
                .options
                .key
                .as_ref()
                .is_some_and(|expected| expected.key_data() == key.key_data())
            {
                Auth::Accept
            } else {
                Auth::reject()
            },
        )
    }

    async fn auth_publickey(&mut self, _user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        let accepted =
            self.options.key.as_ref().is_some_and(|expected| expected.key_data() == key.key_data());
        if accepted {
            self.stats.accepted.fetch_add(1, Ordering::SeqCst);
        }
        Ok(if accepted && self.options.second_factor {
            Auth::Reject {
                proceed_with_methods: Some(
                    [MethodKind::Password, MethodKind::KeyboardInteractive].as_slice().into(),
                ),
                partial_success: true,
            }
        } else if accepted {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_openssh_certificate(
        &mut self,
        user: &str,
        cert: &Certificate,
    ) -> Result<Auth, Self::Error> {
        self.stats.certificates.fetch_add(1, Ordering::SeqCst);
        self.auth_publickey(user, &PublicKey::new(cert.public_key().clone(), "")).await
    }

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        self.stats.passwords.fetch_add(1, Ordering::SeqCst);
        Ok(if self.options.password.as_deref() == Some(password) {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: russh::Channel<server::Msg>,
        host: &str,
        port: u32,
        _origin: &str,
        _origin_port: u32,
        reply: server::ChannelOpenHandle,
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if self.options.forward.as_ref() != Some(&(host.to_owned(), port as u16)) {
            reply.reject(russh::ChannelOpenFailure::AdministrativelyProhibited).await;
            return Ok(());
        }
        let mut stream = TcpStream::connect((host, port as u16)).await?;
        reply.accept().await;
        tokio::spawn(async move {
            let _ = tokio::io::copy_bidirectional(&mut channel.into_stream(), &mut stream).await;
        });
        Ok(())
    }
}

pub(super) struct Fixture {
    pub route: ResolvedRoute,
    pub stats: Arc<ServerStats>,
    pub directory: tempfile::TempDir,
    listener: tokio::task::JoinHandle<()>,
}

impl Fixture {
    pub async fn new(options: ServerOptions) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let host_key = key();
        let known_hosts = directory.path().join("known_hosts");
        russh::keys::known_hosts::learn_known_hosts_path(
            "127.0.0.1",
            address.port(),
            host_key.public_key(),
            &known_hosts,
        )
        .unwrap();
        let stats = Arc::new(ServerStats::default());
        let mut config = server::Config {
            keys: vec![host_key.as_ref().clone()],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        };
        if let Some(hash) = options.rsa_hash {
            config.preferred.key =
                vec![Algorithm::Ed25519, Algorithm::Rsa { hash: Some(hash) }].into();
        }
        let config = Arc::new(config);
        let handler = Server { options, stats: stats.clone() };
        let listener = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let config = config.clone();
                let handler = handler.clone();
                tokio::spawn(async move {
                    let stats = handler.stats.clone();
                    if let Ok(session) = server::run_stream(config, stream, handler).await {
                        let _ = session.await;
                    }
                    stats.closed.fetch_add(1, Ordering::SeqCst);
                });
            }
        });
        let destination =
            SshDestination::parse(&format!("fixture-user@127.0.0.1:{}", address.port())).unwrap();
        let route = ResolvedRoute {
            profile: SshProfileAuth {
                destination: destination.original.clone(),
                auth: SshAuthMode::Auto,
                private_keys: Vec::new(),
                label: None,
                icon: None,
                connection: Default::default(),
            },
            destination,
            transport: RouteTransport::Direct,
            known_hosts_path: Some(known_hosts),
        };
        Self { route, stats, directory, listener }
    }

    pub fn request(&self, password: Option<&str>) -> SshTestRequest {
        SshTestRequest {
            request_id: 1,
            destination: self.route.destination.original.clone(),
            auth: self.route.profile.auth,
            private_keys: self.route.profile.private_keys.clone(),
            password: password.map(str::to_owned),
            connection: self.route.profile.connection.clone(),
            proxy_password: None,
        }
    }

    pub async fn formal(&self) -> Result<(), SessionError> {
        let acquired = crate::ssh_session::authenticated_route(
            &self.route,
            None::<&NoopSshEventHost>,
            false,
            false,
        )
        .await?;
        assert!(!acquired.reused);
        crate::ssh_session::connection_pool().lock().await.remove(&acquired.key);
        Ok(())
    }

    pub async fn test(&self, password: Option<&str>) -> Result<(), SessionError> {
        crate::ssh_session::test_connect(&self.route, &self.request(password)).await
    }

    pub async fn attempt(&self) -> Result<Attempt, SessionError> {
        let mut transport = crate::ssh_session::open_transport(
            &self.route,
            Arc::new(russh::client::Config::default()),
            true,
            false,
        )
        .await?;
        transport.session.authenticate_none(&self.route.destination.user).await?;
        authenticate(
            &mut transport.session,
            &self.route.destination,
            &self.route.profile.private_keys,
        )
        .await
    }

    pub async fn closed(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while self.stats.closed.load(Ordering::SeqCst) < count {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("failed transport must close, not remain awaiting a signature");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

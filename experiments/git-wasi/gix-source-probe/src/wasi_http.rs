use std::{
    any::Any,
    borrow::Cow,
    cell::RefCell,
    future::Future,
    io,
    pin::Pin,
    rc::Rc,
    sync::Arc,
    task::{Context, Poll},
};

use hickory_resolver::{
    TokioResolver,
    config::{GOOGLE, LookupIpStrategy, ResolverConfig},
    net::{NetError, runtime::TokioRuntimeProvider},
};
use once_cell::sync::OnceCell;

use gix::{
    bstr::{BStr, ByteSlice},
    protocol::{
        async_trait::async_trait,
        futures_io::{AsyncRead, AsyncWrite},
        futures_lite::{AsyncReadExt, io::Cursor},
        transport::{
            Protocol, Service,
            client::{
                self, MessageKind, TransportWithoutIO, WriteMode,
                async_io::{RequestWriter, SetServiceResponse},
                capabilities::async_recv::Handshake,
            },
            packetline::{PacketLineRef, async_io::StreamingPeekableIter},
        },
    },
};

type ResponseFuture = Pin<Box<dyn Future<Output = io::Result<Vec<u8>>>>>;

#[derive(Clone, Debug, Default)]
struct WasiDnsResolver {
    state: Arc<OnceCell<TokioResolver>>,
}

impl reqwest::dns::Resolve for WasiDnsResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let resolver = self.clone();
        Box::pin(async move {
            let resolver = resolver.state.get_or_try_init(new_dns_resolver)?;
            let lookup = resolver.lookup_ip(name.as_str()).await?;
            let addresses: reqwest::dns::Addrs = Box::new(
                lookup
                    .iter()
                    .map(|address| std::net::SocketAddr::new(address, 0))
                    .collect::<Vec<_>>()
                    .into_iter(),
            );
            Ok(addresses)
        })
    }
}

fn new_dns_resolver() -> Result<TokioResolver, NetError> {
    let mut builder = TokioResolver::builder_with_config(
        ResolverConfig::udp_and_tcp(&GOOGLE),
        TokioRuntimeProvider::default(),
    );
    builder.options_mut().ip_strategy = LookupIpStrategy::Ipv4AndIpv6;
    builder.build()
}

pub fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(WasiDnsResolver::default())
        .build()
}

#[derive(Clone)]
struct BasicAuth {
    username: String,
    password: String,
}

pub struct Transport {
    client: reqwest::Client,
    url: String,
    desired_protocol: Protocol,
    actual_protocol: Protocol,
    service: Option<Service>,
    line_provider: Option<StreamingPeekableIter<DeferredResponse>>,
    basic_auth: Option<BasicAuth>,
}

impl Transport {
    pub fn new(url: String, desired_protocol: Protocol) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: client()?,
            url: url.trim_end_matches('/').to_owned(),
            desired_protocol,
            actual_protocol: desired_protocol,
            service: None,
            line_provider: None,
            basic_auth: None,
        })
    }

    pub fn with_basic_auth(mut self, username: String, password: String) -> Self {
        self.basic_auth = Some(BasicAuth { username, password });
        self
    }

    fn endpoint(&self, suffix: &str) -> String {
        format!("{}/{}", self.url, suffix.trim_start_matches('/'))
    }

    fn request_headers(&self, service: Service) -> Vec<(&'static str, String)> {
        let mut headers = vec![
            ("user-agent", "plainfeed-gix-wasip2-probe/0.0.0".into()),
            (
                "content-type",
                format!("application/x-{}-request", service.as_str()),
            ),
            (
                "accept",
                format!("application/x-{}-result", service.as_str()),
            ),
        ];
        if self.actual_protocol != Protocol::V1 {
            headers.push((
                "git-protocol",
                format!("version={}", self.actual_protocol as usize),
            ));
        }
        headers
    }

    fn authenticate(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.basic_auth {
            Some(auth) => request.basic_auth(&auth.username, Some(&auth.password)),
            None => request,
        }
    }
}

impl TransportWithoutIO for Transport {
    fn to_url(&self) -> Cow<'_, BStr> {
        Cow::Borrowed(self.url.as_bytes().as_bstr())
    }

    fn connection_persists_across_multiple_requests(&self) -> bool {
        false
    }

    fn configure(
        &mut self,
        _config: &dyn Any,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }
}

#[async_trait(?Send)]
impl client::async_io::Transport for Transport {
    async fn handshake<'a>(
        &mut self,
        service: Service,
        extra_parameters: &'a [(&'a str, Option<&'a str>)],
    ) -> Result<SetServiceResponse<'_>, client::Error> {
        let mut protocol_parameters = vec![format!("version={}", self.desired_protocol as usize)];
        protocol_parameters.extend(extra_parameters.iter().map(|(key, value)| match value {
            Some(value) => format!("{key}={value}"),
            None => (*key).to_owned(),
        }));

        let mut request = self
            .client
            .get(self.endpoint(&format!("info/refs?service={}", service.as_str())))
            .header("user-agent", "plainfeed-gix-wasip2-probe/0.0.0");
        // Git receive-pack still uses the original v0 capability advertisement.
        // GitHub honors an explicit `version=1` request by prepending a standalone
        // `version 1` packet, which gix-transport 0.50 currently mistakes for the
        // first ref and tries to split at a capability NUL. Omitting the header is
        // also what ordinary v0 push clients do and keeps the first ref and its
        // capabilities together.
        if service != Service::ReceivePack {
            request = request.header("git-protocol", protocol_parameters.join(":"));
        }
        let response = self
            .authenticate(request)
            .send()
            .await
            .map_err(reqwest_io_error)?;
        verify_response(&response, service, "advertisement")?;
        let body = response.bytes().await.map_err(reqwest_io_error)?.to_vec();

        self.line_provider = Some(StreamingPeekableIter::new(
            DeferredResponse::ready(body),
            &[PacketLineRef::Flush],
            false,
        ));
        let line_provider = self.line_provider.as_mut().expect("just initialized");

        let first_line =
            line_provider
                .peek_line()
                .await
                .ok_or(client::Error::ExpectedLine(
                    "capabilities, version or service",
                ))???;
        let first_text = first_line
            .as_text()
            .ok_or(client::Error::ExpectedLine("text"))?;
        if let Some(announced_service) = first_text.as_bstr().strip_prefix(b"# service=") {
            if announced_service != service.as_str().as_bytes() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "server announced an unexpected Git service",
                )
                .into());
            }
            line_provider.as_read().read_to_end(&mut Vec::new()).await?;
        }

        let Handshake {
            capabilities,
            refs,
            protocol,
        } = Handshake::from_lines_with_version_detection(line_provider).await?;
        self.actual_protocol = protocol;
        self.service = Some(service);
        Ok(SetServiceResponse {
            actual_protocol: protocol,
            capabilities,
            refs,
        })
    }

    fn request(
        &mut self,
        write_mode: WriteMode,
        on_into_read: MessageKind,
        trace: bool,
    ) -> Result<RequestWriter<'_>, client::Error> {
        let service = self.service.ok_or(client::Error::MissingHandshake)?;
        let body = Rc::new(RefCell::new(Vec::new()));
        let future_body = Rc::clone(&body);
        let client = self.client.clone();
        let url = self.endpoint(service.as_str());
        let headers = self.request_headers(service);
        let basic_auth = self.basic_auth.clone();
        let response_future = Box::pin(async move {
            let request_body = future_body.borrow().clone();
            let mut request = client.post(url).body(request_body);
            if let Some(auth) = basic_auth {
                request = request.basic_auth(auth.username, Some(auth.password));
            }
            for (name, value) in headers {
                request = request.header(name, value);
            }
            let response = request.send().await.map_err(reqwest_io_error)?;
            verify_response(&response, service, "result")?;
            Ok(response.bytes().await.map_err(reqwest_io_error)?.to_vec())
        });

        self.line_provider = Some(StreamingPeekableIter::new(
            DeferredResponse::pending(response_future),
            &[PacketLineRef::Flush],
            trace,
        ));
        let reader = self
            .line_provider
            .as_mut()
            .expect("request response reader initialized")
            .as_read_without_sidebands();
        Ok(RequestWriter::new_from_bufread(
            BodyWriter(body),
            Box::new(reader),
            write_mode,
            on_into_read,
            trace,
        ))
    }
}

fn verify_response(response: &reqwest::Response, service: Service, kind: &str) -> io::Result<()> {
    if !response.status().is_success() {
        let kind = if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            io::ErrorKind::PermissionDenied
        } else {
            io::ErrorKind::Other
        };
        return Err(io::Error::new(
            kind,
            format!("HTTP status {}", response.status()),
        ));
    }
    let expected = format!("application/x-{}-{kind}", service.as_str());
    let actual = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    if actual != Some(expected.as_str()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected content-type {expected:?}, got {actual:?}"),
        ));
    }
    Ok(())
}

fn reqwest_io_error(error: reqwest::Error) -> io::Error {
    io::Error::other(error)
}

struct BodyWriter(Rc<RefCell<Vec<u8>>>);

impl AsyncWrite for BodyWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.0.borrow_mut().extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

enum DeferredResponse {
    Pending(ResponseFuture),
    Ready(Cursor<Vec<u8>>),
}

impl DeferredResponse {
    fn pending(future: ResponseFuture) -> Self {
        Self::Pending(future)
    }

    fn ready(body: Vec<u8>) -> Self {
        Self::Ready(Cursor::new(body))
    }
}

impl AsyncRead for DeferredResponse {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            match &mut *self {
                Self::Pending(future) => match future.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(body)) => *self = Self::Ready(Cursor::new(body)),
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                },
                Self::Ready(cursor) => return Pin::new(cursor).poll_read(cx, buf),
            }
        }
    }
}

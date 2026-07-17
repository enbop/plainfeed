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

use futures_util::StreamExt;
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
use hickory_resolver::{
    TokioResolver,
    config::{GOOGLE, LookupIpStrategy, ResolverConfig},
    net::{NetError, runtime::TokioRuntimeProvider},
};
use once_cell::sync::OnceCell;

use crate::{BufferLimit, Credentials, Error, FetchLimits, Remote};

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

fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(WasiDnsResolver::default())
        .build()
}

pub(crate) struct Transport {
    client: reqwest::Client,
    url: String,
    desired_protocol: Protocol,
    actual_protocol: Protocol,
    service: Option<Service>,
    line_provider: Option<StreamingPeekableIter<DeferredResponse>>,
    credentials: Option<Credentials>,
    limits: FetchLimits,
}

impl Transport {
    pub(crate) fn new(remote: Remote, limits: FetchLimits) -> Result<Self, Error> {
        Self::with_protocol(remote, limits, Protocol::V2)
    }

    pub(crate) fn for_push(remote: Remote, limits: FetchLimits) -> Result<Self, Error> {
        Self::with_protocol(remote, limits, Protocol::V1)
    }

    fn with_protocol(
        remote: Remote,
        limits: FetchLimits,
        protocol: Protocol,
    ) -> Result<Self, Error> {
        Ok(Self {
            client: client()?,
            url: remote.url.trim_end_matches('/').to_owned(),
            desired_protocol: protocol,
            actual_protocol: protocol,
            service: None,
            line_provider: None,
            credentials: remote.credentials,
            limits,
        })
    }

    fn endpoint(&self, suffix: &str) -> String {
        format!("{}/{}", self.url, suffix.trim_start_matches('/'))
    }

    fn authenticate(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.credentials {
            Some(credentials) => {
                let (username, password) = credentials.parts();
                request.basic_auth(username, Some(password))
            }
            None => request,
        }
    }

    fn request_headers(&self, service: Service) -> Vec<(&'static str, String)> {
        let mut headers = vec![
            ("user-agent", "plainfeed/0.1".into()),
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
            .header("user-agent", "plainfeed/0.1");
        if service != Service::ReceivePack {
            request = request.header("git-protocol", protocol_parameters.join(":"));
        }
        let response = self
            .authenticate(request)
            .send()
            .await
            .map_err(reqwest_io_error)?;
        let body = read_response(
            response,
            service,
            "advertisement",
            self.limits.max_response_bytes,
        )
        .await?;

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
        let credentials = self.credentials.clone();
        let max_response_bytes = self.limits.max_response_bytes;
        let response_future = Box::pin(async move {
            let request_body = future_body.borrow().clone();
            let mut request = client.post(url).body(request_body);
            if let Some(credentials) = credentials {
                let (username, password) = credentials.parts();
                request = request.basic_auth(username, Some(password));
            }
            for (name, value) in headers {
                request = request.header(name, value);
            }
            let response = request.send().await.map_err(reqwest_io_error)?;
            read_response(response, service, "result", max_response_bytes).await
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

async fn read_response(
    response: reqwest::Response,
    service: Service,
    kind: &str,
    limit: usize,
) -> io::Result<Vec<u8>> {
    verify_response(&response, service, kind)?;
    if response
        .content_length()
        .is_some_and(|bytes| bytes > limit as u64)
    {
        return Err(limit_error(
            bytes_to_usize(response.content_length()),
            limit,
        ));
    }
    let mut buffer = BufferLimit::new(limit);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(reqwest_io_error)?;
        buffer
            .extend(&chunk)
            .map_err(|error| io::Error::other(error.to_string()))?;
    }
    Ok(buffer.into_inner())
}

fn bytes_to_usize(bytes: Option<u64>) -> usize {
    bytes
        .unwrap_or(usize::MAX as u64)
        .try_into()
        .unwrap_or(usize::MAX)
}

fn limit_error(bytes: usize, limit: usize) -> io::Error {
    io::Error::other(Error::ResponseTooLarge { bytes, limit }.to_string())
}

fn verify_response(response: &reqwest::Response, service: Service, kind: &str) -> io::Result<()> {
    if !response.status().is_success() {
        let error_kind = if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            io::ErrorKind::PermissionDenied
        } else {
            io::ErrorKind::Other
        };
        return Err(io::Error::new(
            error_kind,
            format!("Git HTTP returned status {}", response.status()),
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

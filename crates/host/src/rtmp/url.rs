use super::Error;
use std::net::SocketAddr;
/// URL and credentials are authored strings. DNS resolution is an explicit caller job.
#[derive(Clone, Debug)]
pub struct ResolvedTarget {
    pub(crate) address: SocketAddr,
    addresses: Vec<SocketAddr>,
    pub(crate) app: String,
    pub(crate) tc_url: String,
    pub(crate) stream_name: String,
    pub(crate) username: Option<String>,
    pub(crate) password: Option<String>,
    tls_server_name: Option<String>,
}
impl ResolvedTarget {
    pub fn new(
        address: SocketAddr,
        app: String,
        tc_url: String,
        stream_name: String,
        username: Option<String>,
        password: Option<String>,
    ) -> Result<Self, Error> {
        let endpoint = skylive_core::StreamingEndpoint::parse(&tc_url)
            .map_err(|_| Error::Invalid("tcUrl must contain a valid rtmp:// or rtmps:// server URL"))?;
        if endpoint.app() != app {
            return Err(Error::Invalid("application does not match the authored server URL"));
        }
        let tls_server_name = endpoint.is_secure().then(|| endpoint.host().to_owned());
        for value in [&app, &tc_url, &stream_name]
            .into_iter()
            .chain(username.iter())
            .chain(password.iter())
        {
            if value.len() > 16384 || value.contains('\0') {
                return Err(Error::Limit("connect string exceeds16KiB or contains NUL"));
            }
        }
        if app.is_empty() || stream_name.is_empty() {
            return Err(Error::Invalid("app and stream name must be nonempty"));
        }
        if username.is_some() != password.is_some() {
            return Err(Error::Invalid(
                "username and password must both be provided",
            ));
        }
        Ok(Self {
            address,
            addresses: vec![address],
            app,
            tc_url,
            stream_name,
            username,
            password,
            tls_server_name,
        })
    }
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    /// Bounded DNS results in resolver order. The original constructor keeps
    /// its single-address contract for already resolved clients.
    pub fn with_addresses(mut self, addresses: Vec<SocketAddr>) -> Result<Self, Error> {
        if addresses.is_empty() || addresses.len() > 16 {
            return Err(Error::Limit("RTMP address count must be 1..=16"));
        }
        let mut unique = Vec::with_capacity(addresses.len());
        for address in addresses {
            if !unique.contains(&address) { unique.push(address); }
        }
        self.address = unique[0];
        self.addresses = unique;
        Ok(self)
    }
    pub(crate) fn addresses(&self) -> &[SocketAddr] { &self.addresses }
    pub fn app(&self) -> &str {
        &self.app
    }
    pub fn tc_url(&self) -> &str {
        &self.tc_url
    }
    pub fn stream_name(&self) -> &str {
        &self.stream_name
    }
    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }
    pub fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }
    /// Certificate verification uses the authored host, never its resolved IP.
    pub fn tls_server_name(&self) -> Option<&str> {
        self.tls_server_name.as_deref()
    }
}

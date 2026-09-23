use anyhow::{bail, Context, Result};
use reqwest::{Client, Proxy};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Build a reqwest client with an optional provider-specific upstream proxy.
pub fn client(proxy_url: Option<&str>, timeout: Duration) -> Result<Client> {
    let mut builder = Client::builder().timeout(timeout);
    if let Some(proxy_url) = proxy_url {
        let proxy_url = proxy_url.trim();
        if proxy_url.is_empty() {
            bail!("provider proxy URL must not be empty");
        }
        builder = builder.proxy(
            Proxy::all(proxy_url)
                .with_context(|| format!("invalid provider proxy URL '{proxy_url}'"))?,
        );
    }
    builder.build().context("failed to build HTTP client")
}

/// Cache proxied clients by URL while sharing one direct client.
#[derive(Clone)]
pub struct ProviderClients {
    direct: Client,
    timeout: Duration,
    proxied: Arc<Mutex<HashMap<String, Client>>>,
}

impl ProviderClients {
    pub fn new(timeout: Duration) -> Self {
        Self {
            direct: client(None, timeout).expect("direct HTTP client"),
            timeout,
            proxied: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn direct(&self) -> Client {
        self.direct.clone()
    }

    pub fn for_proxy(&self, proxy_url: Option<&str>) -> Result<Client> {
        let Some(proxy_url) = proxy_url.map(str::trim).filter(|url| !url.is_empty()) else {
            return Ok(self.direct());
        };
        if let Some(client) = self
            .proxied
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(proxy_url)
            .cloned()
        {
            return Ok(client);
        }

        let client = client(Some(proxy_url), self.timeout)?;
        self.proxied
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(proxy_url.to_string(), client.clone());
        Ok(client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_clients_reject_invalid_proxy_urls() {
        let clients = ProviderClients::new(Duration::from_secs(1));

        assert!(clients.for_proxy(Some("not a proxy URL")).is_err());
    }

    #[test]
    fn provider_clients_accept_socks5h_proxy_urls() {
        let clients = ProviderClients::new(Duration::from_secs(1));

        assert!(clients.for_proxy(Some("socks5h://127.0.0.1:8235")).is_ok());
    }
}

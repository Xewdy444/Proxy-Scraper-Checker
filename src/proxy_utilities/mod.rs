use anyhow::{bail, Context, Result};
use futures::future;
use indicatif::ProgressBar;
use scraper::{Html, Selector};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::fmt;
use std::hash::Hash;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{AcquireError, Semaphore};

/// Represents the type of proxy.
#[derive(Debug, PartialEq)]
pub enum ProxyType {
    Http,
    Socks5,
}

/// Represents a proxy, which can be either an HTTP or SOCKS5 proxy.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Proxy {
    Http(String),
    Socks5(String),
}

impl fmt::Display for Proxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Proxy::Http(host) | Proxy::Socks5(host) => write!(f, "{host}"),
        }
    }
}

impl PartialOrd for Proxy {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Proxy {
    fn cmp(&self, other: &Self) -> Ordering {
        let (addr, port) = self
            .parse_host()
            .unwrap_or_else(|_| (IpAddr::from([0, 0, 0, 0]), 0));

        let (other_addr, other_port) = other
            .parse_host()
            .unwrap_or_else(|_| (IpAddr::from([0, 0, 0, 0]), 0));

        if addr < other_addr {
            Ordering::Less
        } else if addr > other_addr {
            Ordering::Greater
        } else if port < other_port {
            Ordering::Less
        } else if port > other_port {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }
}

impl Proxy {
    /// Returns the URL of the proxy.
    fn url(&self) -> String {
        match self {
            Proxy::Http(host) => format!("http://{host}"),
            Proxy::Socks5(host) => format!("socks5://{host}"),
        }
    }

    /// Returns a tuple containing the IP address and port of the proxy.
    ///
    /// ## Errors
    ///
    /// Returns an error if the proxy is invalid.
    fn parse_host(&self) -> Result<(IpAddr, u16)> {
        let host = self.to_string();
        let parts: Vec<&str> = host.split(':').collect();

        if parts.len() != 2 {
            bail!("Invalid proxy: {host}");
        }

        let addr = IpAddr::from_str(parts[0]).context("Invalid IP address")?;
        let port: u16 = parts[1].parse().context("Invalid port")?;
        Ok((addr, port))
    }
}

/// A collection of unique HTTP and SOCKS5 proxies.
///
/// ## Fields
///
/// * `http` - A [`HashSet`] of unique HTTP proxies.
/// * `socks5` - A [`HashSet`] of unique SOCKS5 proxies.
#[derive(Debug)]
pub struct Proxies {
    pub http: HashSet<Proxy>,
    pub socks5: HashSet<Proxy>,
}

impl Proxies {
    /// Returns a new [`Proxies`] instance.
    pub fn new() -> Self {
        Self {
            http: HashSet::new(),
            socks5: HashSet::new(),
        }
    }

    /// Adds a proxy to the respective proxy list.
    ///
    /// ## Parameters
    ///
    /// * `proxy` - The proxy to add.
    ///
    /// ## Returns
    ///
    /// `true` if the proxy was added, `false` if it was already present.
    pub fn add(&mut self, proxy: Proxy) -> bool {
        match proxy {
            Proxy::Http(_) => self.http.insert(proxy),
            Proxy::Socks5(_) => self.socks5.insert(proxy),
        }
    }
}

impl FromIterator<Proxy> for Proxies {
    fn from_iter<T: IntoIterator<Item = Proxy>>(iter: T) -> Self {
        let mut proxy_lists = Self::new();

        for proxy in iter {
            proxy_lists.add(proxy);
        }

        proxy_lists
    }
}

/// Used to scrape proxies from the checkerproxy.net proxies archive.
///
/// ## Fields
///
/// * `client` - The [`reqwest::Client`] to use for making requests.
#[derive(Clone, Debug)]
pub struct ProxyScraper {
    client: reqwest::Client,
}

impl ProxyScraper {
    /// Returns a new [`ProxyScraper`] instance.
    ///
    /// ## Parameters
    ///
    /// * `client` - The [`reqwest::Client`] to use for making requests.
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    /// Scrapes the archive URLs from the <https://checkerproxy.net/getAllProxy> page
    /// and converts them into the API URLs.
    ///
    /// ## Returns
    ///
    /// A [`Vec`] of the API URLs.
    ///
    /// ## Errors
    ///
    /// Returns an error if the request to the <https://checkerproxy.net/getAllProxy> page fails.
    pub async fn scrape_archive_urls(&self) -> Result<Vec<String>> {
        let response = self
            .client
            .get("https://checkerproxy.net/getAllProxy")
            .send()
            .await
            .context("Failed to get archive URLs")?;

        let html = response.text().await?;
        let parser = Html::parse_document(&html);
        let selector = Selector::parse("li > a").unwrap();

        let mut archive_urls = Vec::new();

        for element in parser.select(&selector) {
            let uri_path = match element.value().attr("href") {
                Some(path) => path,
                None => continue,
            };

            if !uri_path.contains("/archive/") {
                continue;
            }

            let url = format!("https://checkerproxy.net/api{uri_path}");
            archive_urls.push(url);
        }

        Ok(archive_urls)
    }

    /// Scrapes the proxies from the given checkerproxy.net archive API URL.
    ///
    /// ## Parameters
    ///
    /// * `archive_url` - The archive API URL to scrape the proxies from.
    /// * `anonymous_only` - Whether to only scrape anonymous proxies.
    ///
    /// ## Returns
    ///
    /// A [`Vec`] of the scraped proxies.
    ///
    /// ## Errors
    ///
    /// Returns an error if the request to the archive API URL fails or
    /// if the JSON received is invalid.
    pub async fn scrape_proxies(
        &self,
        archive_url: String,
        anonymous_only: bool,
    ) -> Result<Vec<Proxy>> {
        let response = self
            .client
            .get(archive_url)
            .send()
            .await
            .context("Request failed")?
            .error_for_status()
            .context("Request returned an error status code")?;

        let json: Value = serde_json::from_str(&response.text().await?)?;
        let mut proxies = Vec::new();

        for proxy_dict in json.as_array().context("Invalid JSON received")? {
            if anonymous_only {
                let kind = match proxy_dict.get("kind") {
                    Some(value) => match value.as_u64() {
                        Some(value) => value,
                        None => continue,
                    },
                    None => continue,
                };

                if kind != 2 {
                    continue;
                }
            }

            let host = match proxy_dict.get("addr") {
                Some(value) => match value.as_str() {
                    Some(str_value) => str_value.to_string(),
                    None => continue,
                },
                None => continue,
            };

            let proxy = match proxy_dict.get("type") {
                Some(value) => match value.as_u64() {
                    Some(1) => Proxy::Http(host),
                    Some(2) => Proxy::Http(host),
                    Some(4) => Proxy::Socks5(host),
                    _ => continue,
                },
                None => continue,
            };

            proxies.push(proxy);
        }

        Ok(proxies)
    }
}

impl Default for ProxyScraper {
    /// Returns a new [`ProxyScraper`] instance with a 30 second timeout.
    fn default() -> Self {
        Self::new(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap(),
        )
    }
}

/// Used to check a collection of proxies.
///
/// ## Fields
///
/// * `semaphore` - The semaphore used to limit the number of concurrent requests.
/// * `progress_bar` - The progress bar used to display the progress of the proxy checks.
#[derive(Debug)]
pub struct ProxyChecker {
    semaphore: Arc<Semaphore>,
    progress_bar: ProgressBar,
}

impl ProxyChecker {
    /// Returns a new [`ProxyChecker`] instance.
    ///
    /// ## Parameters
    ///
    /// * `semaphore` - The semaphore used to limit the number of concurrent requests.
    /// * `progress_bar` - The progress bar used to display the progress of the proxy checks.
    pub fn new(semaphore: Arc<Semaphore>, progress_bar: ProgressBar) -> Self {
        Self {
            semaphore,
            progress_bar,
        }
    }

    /// Checks if the given proxy is working by making an HTTP request to the given URL.
    ///
    /// ## Parameters
    ///
    /// * `proxy` - The proxy to check.
    /// * `url` - The URL that the proxy will be checked against.
    /// * `timeout` - The request timeout in seconds.
    ///
    /// ## Returns
    ///
    /// The proxy if the check succeeds.
    ///
    /// ## Errors
    ///
    /// An error is returned if the [`reqwest::Client`] cannot be built, if the request fails,
    /// or if the HTTP response is an error status code.
    async fn check_proxy(proxy: Proxy, url: String, timeout: u64) -> Result<Proxy> {
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(proxy.url())?)
            .timeout(Duration::from_secs(timeout))
            .build()?;

        client
            .get(url)
            .send()
            .await
            .context("Request failed")?
            .error_for_status()
            .context("Request returned an error status code")?;

        Ok(proxy)
    }

    /// Checks if the given proxies are working by making an HTTP request to the given URL.
    ///
    /// ## Parameters
    ///
    /// * `proxies` - A set of proxies to check.
    /// * `url` - The URL that the proxies will be checked against.
    /// * `timeout` - The request timeout in seconds.
    ///
    /// ## Returns
    ///
    /// A [`Vec`] of the working proxies.
    ///
    /// ## Errors
    ///
    /// An error is returned if the semaphore has been closed.
    pub async fn check_proxies(
        &self,
        proxies: HashSet<Proxy>,
        url: String,
        timeout: u64,
    ) -> Result<Vec<Proxy>> {
        let mut tasks = Vec::new();

        for proxy in proxies {
            let semaphore = self.semaphore.clone();
            let progress_bar = self.progress_bar.clone();
            let url = url.clone();

            let task = tokio::spawn(async move {
                let _permit = semaphore.acquire().await?;
                let result = Self::check_proxy(proxy, url, timeout).await;
                progress_bar.inc(1);
                result
            });

            tasks.push(task);
        }

        let results = future::try_join_all(tasks).await?;
        let mut working_proxies = Vec::new();

        for result in results {
            match result {
                Ok(proxy) => working_proxies.push(proxy),
                Err(err) => match err.downcast_ref::<AcquireError>() {
                    Some(_) => bail!("Semaphore has been closed"),
                    None => continue,
                },
            }
        }

        Ok(working_proxies)
    }
}

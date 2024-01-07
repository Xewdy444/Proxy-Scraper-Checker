mod proxy_utilities;

use anyhow::{bail, Context, Result};
use clap::{command, Parser};
use futures::future;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use proxy_utilities::{Proxies, Proxy, ProxyChecker, ProxyScraper, ProxyType};
#[cfg(not(windows))]
use rlimit::Resource;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tabled::builder::Builder;
use tabled::settings::{Alignment, Style};
use tokio::sync::Semaphore;

/// Represents the result of a check task.
#[derive(Debug)]
struct CheckTaskResult {
    proxy_type: ProxyType,
    working_proxies: Vec<Proxy>,
    proxies_checked: u64,
    check_duration: Duration,
}

impl CheckTaskResult {
    /// Returns a new [`CheckTaskResult`] instance.
    ///
    /// # Parameters
    ///
    /// * `proxy_type` - The type of proxies that were checked.
    /// * `working_proxies` - The working proxies.
    /// * `proxies_checked` - The number of proxies that were checked.
    /// * `check_duration` - The duration of the check task.
    fn new(
        proxy_type: ProxyType,
        working_proxies: Vec<Proxy>,
        proxies_checked: u64,
        check_duration: Duration,
    ) -> Self {
        Self {
            proxy_type,
            working_proxies,
            proxies_checked,
            check_duration,
        }
    }
}

/// Increases the open file limit if necessary on Windows.
///
/// # Parameters
///
/// * `limit` - The limit to set.
///
/// # Returns
///
/// A [`Result`] containing the result of the operation.
///
/// # Errors
///
/// Returns an error if the open file limit is greater than 8192
/// or if the limit could not be set.
#[cfg(windows)]
fn set_open_file_limit(limit: u64) -> Result<()> {
    let open_file_limit = rlimit::getmaxstdio() as u64;

    if limit <= open_file_limit {
        return Ok(());
    }

    if limit > 8192 {
        bail!("The open file limit cannot be greater than 8192 on Windows");
    }

    rlimit::setmaxstdio(limit as u32).context("Failed to set open file limit")?;
    Ok(())
}

/// Increases the open file limit if necessary on non-Windows platforms.
///
/// # Parameters
///
/// * `limit` - The limit to set.
///
/// # Returns
///
/// A [`Result`] containing the result of the operation.
///
/// # Errors
///
/// Returns an error if the limit could not be retrieved or set.
#[cfg(not(windows))]
fn set_open_file_limit(limit: u64) -> Result<()> {
    let open_file_limit =
        rlimit::getrlimit(Resource::NOFILE).context("Failed to get open file limit")?;

    if limit <= open_file_limit.0 {
        return Ok(());
    }

    rlimit::setrlimit(Resource::NOFILE, limit, open_file_limit.1)
        .context("Failed to set open file limit")?;

    Ok(())
}

/// Writes the proxies to a file.
///
/// # Parameters
///
/// * `proxy_type` - The type of proxies.
/// * `proxies` - The proxies to write to a file.
/// * `proxies_folder` - The folder to save the proxies to.
///
/// # Returns
///
/// A [`Result`] containing the result of the operation.
///
/// # Errors
///
/// Returns an error if the proxies folder could not be created,
/// if the file could not be created, or if the proxies could not be written to the file.
fn write_proxies_to_file(
    proxy_type: ProxyType,
    proxies: &[Proxy],
    proxies_folder: &Path,
) -> Result<()> {
    if !proxies_folder.exists() {
        fs::create_dir_all(proxies_folder).context("Failed to create proxies folder")?;
    }

    let file_name = match proxy_type {
        ProxyType::Http => "http.txt",
        ProxyType::Socks5 => "socks5.txt",
    };

    let file = File::create(proxies_folder.join(file_name)).context("Failed to create file")?;
    let mut writer = BufWriter::new(file);

    for proxy in proxies {
        writeln!(writer, "{proxy}").context("Failed to write proxy")?;
    }

    writer.flush().context("Failed to flush writer")?;
    Ok(())
}

#[derive(Debug, Parser)]
#[command(
    about = "A command-line tool for scraping and checking HTTP and SOCKS5 proxies from the checkerproxy.net proxies archive"
)]
struct Args {
    /// The URL to check the proxies against
    #[arg(short, long, default_value_t = String::from("https://httpbin.org/ip"))]
    url: String,

    /// The number of tasks to run concurrently for checking proxies
    #[arg(long, default_value_t = 512)]
    tasks: u64,

    /// The proxy request timeout in seconds
    #[arg(long, default_value_t = 30)]
    timeout: u64,

    /// The folder to save the working proxies to
    #[arg(short, long, default_value_t = String::from("proxies"))]
    folder: String,

    /// Only save anonymous proxies
    #[arg(short, long, default_value_t = false)]
    anonymous: bool,

    /// Only save HTTP proxies
    #[arg(long, default_value_t = false, conflicts_with = "socks5")]
    http: bool,

    /// Only save SOCKS5 proxies
    #[arg(long, default_value_t = false, conflicts_with = "http")]
    socks5: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let proxy_scraper = ProxyScraper::default();

    set_open_file_limit(args.tasks)
        .context("Decreasing the number of tasks should fix this issue")
        .unwrap();

    let archive_urls = proxy_scraper
        .scrape_archive_urls()
        .await
        .expect("Failed to scrape archive URLs");

    let scrape_progress_bar = ProgressBar::new(archive_urls.len() as u64).with_style(
        ProgressStyle::default_bar()
        .template(
            "[{msg}] {spinner:.red} {percent}% [{wide_bar:.red}] {pos}/{len} [{elapsed_precise}<{eta_precise}, {per_sec}]",
        )
        .unwrap(),
    );

    scrape_progress_bar.set_message("Scraping proxies archive");
    let mut scrape_tasks = Vec::new();

    for url in archive_urls {
        let proxy_scraper = proxy_scraper.clone();
        let progress_bar = scrape_progress_bar.clone();

        let task = tokio::spawn(async move {
            let result = proxy_scraper.scrape_proxies(url, args.anonymous).await;
            progress_bar.inc(1);
            result
        });

        scrape_tasks.push(task);
    }

    let proxies: Proxies = future::try_join_all(scrape_tasks)
        .await
        .expect("Failed to scrape proxies")
        .into_iter()
        .filter_map(|proxies| proxies.ok())
        .flatten()
        .collect();

    scrape_progress_bar.finish_with_message("Finished scraping proxies archive");

    let multiprogress_bar = MultiProgress::new();
    let semaphore = Arc::new(Semaphore::new(args.tasks as usize));
    let mut check_tasks = Vec::new();

    if !args.socks5 {
        let proxy_count = proxies.http.len() as u64;

        let http_progress_bar = multiprogress_bar.add(
            ProgressBar::new(proxy_count).with_style(
                ProgressStyle::default_bar()
                    .template(
                        "[{msg}] {spinner:.green} {percent}% [{wide_bar:.green}] {pos}/{len} [{elapsed_precise}<{eta_precise}, {per_sec}]",
                    )
                    .unwrap(),
            ),
        );

        let http_semaphore = semaphore.clone();
        let url = args.url.clone();

        let http_task = tokio::spawn(async move {
            let proxy_checker = ProxyChecker::new(http_semaphore, http_progress_bar.clone());
            http_progress_bar.set_message("Checking HTTP proxies");

            let mut working_proxies = proxy_checker
                .check_proxies(proxies.http, url, args.timeout)
                .await
                .expect("Failed to check HTTP proxies");

            http_progress_bar.finish_with_message("Finished checking HTTP proxies");
            let check_duration = Duration::from_secs(http_progress_bar.elapsed().as_secs());
            working_proxies.sort_unstable();

            CheckTaskResult::new(
                ProxyType::Http,
                working_proxies,
                proxy_count,
                check_duration,
            )
        });

        check_tasks.push(http_task);
    }

    if !args.http {
        let proxy_count = proxies.socks5.len() as u64;

        let socks5_progress_bar = multiprogress_bar.add(
            ProgressBar::new(proxy_count).with_style(
                ProgressStyle::default_bar()
                    .template(
                        "[{msg}] {spinner:.blue} {percent}% [{wide_bar:.blue}] {pos}/{len} [{elapsed_precise}<{eta_precise}, {per_sec}]",
                    )
                    .unwrap(),
            ),
        );

        let socks5_semaphore = semaphore.clone();
        let url = args.url.clone();

        let socks5_task = tokio::spawn(async move {
            let proxy_checker = ProxyChecker::new(socks5_semaphore, socks5_progress_bar.clone());
            socks5_progress_bar.set_message("Checking SOCKS5 proxies");

            let mut working_proxies = proxy_checker
                .check_proxies(proxies.socks5, url, args.timeout)
                .await
                .expect("Failed to check SOCKS5 proxies");

            socks5_progress_bar.finish_with_message("Finished checking SOCKS5 proxies");
            let check_duration = Duration::from_secs(socks5_progress_bar.elapsed().as_secs());
            working_proxies.sort_unstable();

            CheckTaskResult::new(
                ProxyType::Socks5,
                working_proxies,
                proxy_count,
                check_duration,
            )
        });

        check_tasks.push(socks5_task);
    }

    let check_task_results: Vec<CheckTaskResult> = future::try_join_all(check_tasks)
        .await
        .unwrap()
        .into_iter()
        .collect();

    let proxies_folder = Path::new(&args.folder);
    let mut table_builder = Builder::default();

    table_builder.push_record([
        "Proxy Type",
        "Working Proxies",
        "Proxies Checked",
        "Check Duration",
    ]);

    if !args.socks5 {
        let result = check_task_results
            .iter()
            .find(|result| result.proxy_type == ProxyType::Http)
            .unwrap();

        table_builder.push_record([
            "HTTP",
            &result.working_proxies.len().to_string(),
            &result.proxies_checked.to_string(),
            &humantime::format_duration(result.check_duration).to_string(),
        ]);

        if !result.working_proxies.is_empty() {
            write_proxies_to_file(ProxyType::Http, &result.working_proxies, proxies_folder)
                .unwrap();
        }
    }

    if !args.http {
        let result = check_task_results
            .iter()
            .find(|result| result.proxy_type == ProxyType::Socks5)
            .unwrap();

        table_builder.push_record([
            "SOCKS5",
            &result.working_proxies.len().to_string(),
            &result.proxies_checked.to_string(),
            &humantime::format_duration(result.check_duration).to_string(),
        ]);

        if !result.working_proxies.is_empty() {
            write_proxies_to_file(ProxyType::Socks5, &result.working_proxies, proxies_folder)
                .unwrap();
        }
    }

    let mut table = table_builder.build();
    println!("{}", table.with(Style::modern()).with(Alignment::center()));
}

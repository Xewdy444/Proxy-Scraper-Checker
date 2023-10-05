mod proxy_utilities;

use clap::{command, Parser};
use futures::future;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use proxy_utilities::{Proxies, Proxy, ProxyChecker, ProxyScraper, ProxyType};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::time;
use tabled::builder::Builder;
use tabled::settings::{Alignment, Style};
use tokio::sync::Semaphore;

/// Represents the result of a check task.
#[derive(Debug)]
struct CheckTaskResult {
    proxy_type: ProxyType,
    working_proxies: Vec<Proxy>,
    proxies_checked: usize,
    elapsed_time: time::Duration,
}

impl CheckTaskResult {
    /// Creates a new `CheckTaskResult` instance.
    ///
    /// # Arguments
    ///
    /// * `proxy_type` - The type of proxies that were checked.
    /// * `working_proxies` - The working proxies.
    /// * `proxies_checked` - The number of proxies that were checked.
    /// * `elapsed_time` - The elapsed time.
    ///
    /// # Returns
    ///
    /// A new `CheckTaskResult` instance.
    fn new(
        proxy_type: ProxyType,
        working_proxies: Vec<Proxy>,
        proxies_checked: usize,
        elapsed_time: time::Duration,
    ) -> Self {
        Self {
            proxy_type,
            working_proxies,
            proxies_checked,
            elapsed_time,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    about = "A command-line tool written in Rust for scraping and checking HTTP and SOCKS5 proxies from the checkerproxy.net proxies archive"
)]
struct Args {
    /// The URL to check the proxies against
    #[arg(short, long, default_value_t = String::from("https://httpbin.org/ip"))]
    url: String,

    /// The number of tasks to run concurrently for checking proxies
    #[arg(long, default_value_t = 512)]
    tasks: usize,

    /// The proxy request timeout in seconds
    #[arg(long, default_value_t = 30)]
    timeout: usize,

    /// The folder to save the working proxies to
    #[arg(short, long, default_value_t = String::from("proxies"))]
    folder: String,

    /// Only check anonymous proxies
    #[arg(short, long, default_value_t = false)]
    anonymous: bool,

    /// Only check HTTP proxies
    #[arg(long, default_value_t = false, conflicts_with = "socks5")]
    http: bool,

    /// Only check SOCKS5 proxies
    #[arg(long, default_value_t = false, conflicts_with = "http")]
    socks5: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let proxy_scraper = ProxyScraper::default();

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
    let semaphore = Arc::new(Semaphore::new(args.tasks));
    let mut check_tasks = Vec::new();

    if !args.socks5 {
        let proxy_count = proxies.http.len();

        let http_progress_bar = multiprogress_bar.add(
            ProgressBar::new(proxy_count as u64).with_style(
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

            let working_proxies = proxy_checker
                .check_proxies(proxies.http, url, args.timeout)
                .await
                .expect("Failed to check HTTP proxies");

            http_progress_bar.finish_with_message("Finished checking HTTP proxies");
            let elapsed_time = time::Duration::from_secs(http_progress_bar.elapsed().as_secs());
            CheckTaskResult::new(ProxyType::Http, working_proxies, proxy_count, elapsed_time)
        });

        check_tasks.push(http_task);
    }

    if !args.http {
        let proxy_count = proxies.socks5.len();

        let socks5_progress_bar = multiprogress_bar.add(
            ProgressBar::new(proxy_count as u64).with_style(
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

            let working_proxies = proxy_checker
                .check_proxies(proxies.socks5, url, args.timeout)
                .await
                .expect("Failed to check SOCKS5 proxies");

            socks5_progress_bar.finish_with_message("Finished checking SOCKS5 proxies");
            let elapsed_time = time::Duration::from_secs(socks5_progress_bar.elapsed().as_secs());

            CheckTaskResult::new(
                ProxyType::Socks5,
                working_proxies,
                proxy_count,
                elapsed_time,
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

    if !proxies_folder.exists() {
        fs::create_dir_all(proxies_folder).expect("Failed to create proxies folder");
    }

    let mut table_builder = Builder::default();

    table_builder.set_header([
        "Proxy Type",
        "Working Proxies",
        "Proxies Checked",
        "Elapsed Time",
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
            &humantime::format_duration(result.elapsed_time).to_string(),
        ]);

        let file = File::create(proxies_folder.join("http.txt"))
            .expect("Failed to create HTTP proxies file");

        let mut writer = BufWriter::new(file);

        for proxy in &result.working_proxies {
            writeln!(writer, "{}", proxy).expect("Failed to write HTTP proxy to file");
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
            &humantime::format_duration(result.elapsed_time).to_string(),
        ]);

        let file = File::create(proxies_folder.join("socks5.txt"))
            .expect("Failed to create SOCKS5 proxies file");

        let mut writer = BufWriter::new(file);

        for proxy in &result.working_proxies {
            writeln!(writer, "{}", proxy).expect("Failed to write SOCKS5 proxy to file");
        }
    }

    let mut table = table_builder.build();
    println!("{}", table.with(Style::modern()).with(Alignment::center()));
}

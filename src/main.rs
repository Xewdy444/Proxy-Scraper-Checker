mod proxy_utilities;

use clap::{command, Parser};
use futures::future;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use proxy_utilities::{Proxies, ProxyChecker};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Semaphore;

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
    tasks: u16,

    /// The proxy request timeout in seconds
    #[arg(long, default_value_t = 30)]
    timeout: u16,

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

    let archive_urls = proxy_utilities::scrape_archive_urls()
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
        let progress_bar = scrape_progress_bar.clone();

        let task = tokio::spawn(async move {
            let result = proxy_utilities::scrape_proxies(url).await;
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
        let http_progress_bar = multiprogress_bar.add(
            ProgressBar::new(proxies.http.len() as u64).with_style(
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

            let proxies = proxy_checker
                .check_proxies(proxies.http, url, args.timeout)
                .await
                .expect("Failed to check HTTP proxies");

            http_progress_bar.finish_with_message("Finished checking HTTP proxies");
            proxies
        });

        check_tasks.push(http_task);
    }

    if !args.http {
        let socks5_progress_bar = multiprogress_bar.add(
            ProgressBar::new(proxies.socks5.len() as u64).with_style(
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

            let proxies = proxy_checker
                .check_proxies(proxies.socks5, url, args.timeout)
                .await
                .expect("Failed to check SOCKS5 proxies");

            socks5_progress_bar.finish_with_message("Finished checking SOCKS5 proxies");
            proxies
        });

        check_tasks.push(socks5_task);
    }

    let working_proxies: Proxies = future::try_join_all(check_tasks)
        .await
        .unwrap()
        .into_iter()
        .flatten()
        .collect();

    let proxies_folder = Path::new("Proxies");

    if !proxies_folder.exists() {
        fs::create_dir(proxies_folder).expect("Failed to create proxies folder");
    }

    if !args.socks5 {
        let file = File::create(proxies_folder.join("http.txt"))
            .expect("Failed to create HTTP proxies file");

        let mut writer = BufWriter::new(file);

        for proxy in working_proxies.http {
            writeln!(writer, "{}", proxy).expect("Failed to write HTTP proxy to file");
        }
    }

    if !args.http {
        let file = File::create(proxies_folder.join("socks5.txt"))
            .expect("Failed to create SOCKS5 proxies file");

        let mut writer = BufWriter::new(file);

        for proxy in working_proxies.socks5 {
            writeln!(writer, "{}", proxy).expect("Failed to write SOCKS5 proxy to file");
        }
    }
}

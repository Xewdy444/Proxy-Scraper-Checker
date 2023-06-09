# Proxy-Scraper-Checker
A command-line tool written in Rust for scraping and checking HTTP and SOCKS5 proxies from the checkerproxy.net proxies archive. The working proxies are written to `Proxies/http.txt` and `Proxies/socks5.txt` according to the respective proxy type.

![image](https://user-images.githubusercontent.com/95155966/231937289-ddf0187f-e8c9-4878-b92d-96617695e6f0.png)

# Build
    $ cargo build --release

# Usage
> **Note**
> If the program finishes checking the proxies in just a few seconds, that probably means that the `tasks` value is set too high. You can also try increasing your system's open file limit.

```
A command-line tool written in Rust for scraping and checking HTTP and SOCKS5 proxies from the checkerproxy.net proxies archive

Usage: proxy_scraper_checker.exe [OPTIONS]

Options:
  -u, --url <URL>          The URL to check the proxies against [default: https://httpbin.org/ip]     
      --tasks <TASKS>      The number of tasks to run concurrently for checking proxies [default: 512]
      --timeout <TIMEOUT>  The proxy request timeout in seconds [default: 30]
      --http               Only check HTTP proxies
      --socks5             Only check SOCKS5 proxies
  -h, --help               Print help
```
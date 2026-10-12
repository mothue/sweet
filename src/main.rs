use bytes::Bytes;
use clap::Parser;
use futures_util::StreamExt;
use futures_util::stream;
use indicatif::{ProgressBar, ProgressStyle};
use miette::{IntoDiagnostic, Result, WrapErr};
use owo_colors::OwoColorize;
use reqwest::Client;
use serde::Deserialize;
use std::time::{Duration, Instant};

use tokio::signal;

const LOGO: &str = r#"                           _____
__________      _____________  /_
__  ___/_ | /| / /  _ \  _ \  __/
_(__  )__ |/ |/ //  __/  __/ /_
/____/ ____/|__/ \___/\___/\__/"#;

#[derive(Parser)]
#[command(
    name = "sweet",
    about = "CLI speed test",
    version,
    disable_version_flag = true,
    before_help = format!(
        "{}\n\n{}",
        LOGO.red().bold(),
        format!("sweet v{}", env!("CARGO_PKG_VERSION")).bright_red()
    )
)]
struct Args {
    #[arg(short = 'v', long = "version", action = clap::ArgAction::Version, help = "Print version")]
    version: Option<bool>,
    /// Fetch and display network information from ipinfo.io
    #[arg(short = 'i', long)]
    info: bool,
    #[arg(short = 'd', long)]
    download_only: bool,
    #[arg(short = 'u', long)]
    upload_only: bool,
    /// Ping only, with 1-100 requests (default: 10)
    #[arg(
        short = 'p',
        long,
        num_args = 0..=1,
        default_missing_value = "10",
        value_name = "COUNT",
        value_parser = clap::value_parser!(u32).range(1..=100)
    )]
    ping: Option<u32>,
}

const PING_COUNT: u32 = 10;
const DOWNLOAD_MB: u64 = 25;
const UPLOAD_MB: u64 = 25;

const PING_URL: &str = "https://speed.cloudflare.com/__down?bytes=0";
const DOWNLOAD_URL: &str = "https://speed.cloudflare.com/__down?bytes=";
const UPLOAD_URL: &str = "https://speed.cloudflare.com/__up";
const META_URL: &str = "https://ipinfo.io/json";
const UA: &str = concat!("sweettest/", env!("CARGO_PKG_VERSION"));

/// Network information data from ipinfo.io
#[derive(Deserialize, Debug)]
struct Meta {
    ip: String,
    hostname: Option<String>,
    city: Option<String>,
    region: Option<String>,
    country: Option<String>,
    org: Option<String>,
    timezone: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let client = Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .into_diagnostic()
        .wrap_err("Failed to build HTTP client")?;

    tokio::select! {
        result = run_inner(&args, &client) => result,
        _ = signal::ctrl_c() => {
            println!("{}", "Aborted the test".red().bold());
            Ok(())
        }
    }
}

/// 実行部本体
async fn run_inner(args: &Args, client: &Client) -> Result<()> {
    if args.info {
        fetch_meta(client).await?;
        println!();
    }

    let run_all = !args.download_only && !args.upload_only && args.ping.is_none();

    if run_all || args.ping.is_some() {
        measure_ping(client, args.ping.unwrap_or(PING_COUNT)).await?;
    }
    if run_all || args.download_only {
        measure_download(client, DOWNLOAD_MB).await?;
    }
    if run_all || args.upload_only {
        measure_upload(client, UPLOAD_MB).await?;
    }

    Ok(())
}

/// ネットワーク情報取得関数
/// ipinfo.ioからIPアドレスやロケーションなどの情報を取ってきて表示するだけ
async fn fetch_meta(client: &Client) -> Result<()> {
    let spinner = spinner("Fetching...");

    let resp = client
        .get(META_URL)
        .send()
        .await
        .into_diagnostic()
        .wrap_err("Failed to fetch network information")?;

    let meta: Meta = resp
        .json()
        .await
        .into_diagnostic()
        .wrap_err("Failed to parse network information")?;

    spinner.finish_and_clear();

    println!("{}", "Network Information".bold());
    println!(
        "  {}  {}",
        format!("{:<10}", "IP Address").dimmed(),
        meta.ip.yellow()
    );
    if let Some(host) = &meta.hostname {
        println!(
            "  {}  {}",
            format!("{:<10}", "Hostname").dimmed(),
            host.dimmed()
        );
    }
    if let (Some(city), Some(region), Some(country)) = (&meta.city, &meta.region, &meta.country) {
        println!(
            "  {}  {}",
            format!("{:<10}", "Locale").dimmed(),
            format!("{city}, {region}, {country}").bright_white()
        );
    }
    if let Some(org) = &meta.org {
        println!(
            "  {}  {}",
            format!("{:<10}", "AS").dimmed(),
            org.bright_white()
        );
    }
    if let Some(tz) = &meta.timezone {
        println!(
            "  {}  {}",
            format!("{:<10}", "TZ").dimmed(),
            tz.bright_white()
        );
    }
    Ok(())
}

/// Ping計測関数
async fn measure_ping(client: &Client, count: u32) -> Result<()> {
    print!("{}  ", "Ping".bold());

    let spinner = spinner(&format!("Ping... (0/{count})"));

    let mut total = Duration::ZERO;
    let mut rtts: Vec<u32> = Vec::with_capacity(count as usize);

    // CloudflareのpingはGETで0バイトのリクエストを送るだけでできる
    // 特に面白みはない
    for i in 0..count {
        spinner.set_message(format!("Ping... ({i}/{count})"));
        let start = Instant::now();
        if client.get(PING_URL).send().await.is_ok() {
            let elapsed = start.elapsed();
            total += elapsed;
            rtts.push(elapsed.as_millis() as u32);
        }
    }

    spinner.finish_and_clear();

    if rtts.is_empty() {
        println!("{}", "Failed".red());
        return Ok(());
    }

    let avg = total.as_millis() / rtts.len() as u128;
    let min = rtts.iter().min().unwrap();
    let max = rtts.iter().max().unwrap();

    let jitter: u32 = if rtts.len() > 1 {
        rtts.windows(2).map(|w| w[1].abs_diff(w[0])).sum::<u32>() / (rtts.len() - 1) as u32
    } else {
        0
    };

    let avg_colored = match avg {
        0..=9 => format!("{avg}").bright_cyan().bold().to_string(),
        10..=30 => format!("{avg}").green().bold().to_string(),
        31..=80 => format!("{avg}").yellow().bold().to_string(),
        _ => format!("{avg}").red().bold().to_string(),
    };

    let jitter_colored = match jitter {
        0..=9 => format!("{jitter}").bright_cyan().bold().to_string(),
        10..=30 => format!("{jitter}").green().bold().to_string(),
        31..=80 => format!("{jitter}").yellow().bold().to_string(),
        _ => format!("{jitter}").red().bold().to_string(),
    };

    println!(
        "avg {} ms  min {} ms  max {} ms  jitter {} ms  ({}/{} Success)\n",
        avg_colored,
        min.to_string().dimmed(),
        max.to_string().dimmed(),
        jitter_colored,
        rtts.len(),
        count,
    );
    Ok(())
}

/// ダウンロード計測関数
/// ストリームで受ける
async fn measure_download(client: &Client, mb: u64) -> Result<()> {
    let bytes = mb * 1024 * 1024;
    let url = format!("{DOWNLOAD_URL}{bytes}");
    let pb = progress_bar(bytes, "Download");

    let start = Instant::now();
    let resp = client
        .get(&url)
        .send()
        .await
        .into_diagnostic()
        .wrap_err("Failed to download")?;

    let mut stream = resp.bytes_stream();
    let mut received: u64 = 0;

    // ストリームからチャンクを受け取るループ
    // 受け取るたびに受信量を更新してプログレスバーを進める実装
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.into_diagnostic()?;
        received += chunk.len() as u64;
        pb.set_position(received);

        let elapsed = start.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            let mbps = (received as f64 * 8.0) / (elapsed * 1_000_000.0);
            pb.set_message(format!("{mbps:.1} Mbps"));
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    pb.finish_and_clear();

    let mbps = (received as f64 * 8.0) / (elapsed * 1_000_000.0);
    let mbps_colored = color_mbps(mbps);

    println!(
        "{}  {} {}  ({:.1}MiB/s)",
        "Download:".bold(),
        mbps_colored,
        "Mbps".dimmed(),
        received as f64 / (elapsed * 1_048_576.0),
    );
    Ok(())
}

/// アップロード計測関数
/// ストリームで送る
/// メモリ効率のため，チャンクを64KBにして，それを繰り返す形にして送る（メモリが一定！）
async fn measure_upload(client: &Client, mb: u64) -> Result<()> {
    let total_bytes = mb * 1024 * 1024;
    let chunk_size: u64 = 64 * 1024;
    let pb = progress_bar(total_bytes, "Upload  ");

    let chunk_size_usize = chunk_size as usize;
    let template: Bytes = Bytes::from(
        (0..chunk_size_usize)
            .map(|j| (j % 251) as u8)
            .collect::<Vec<u8>>(),
    );

    let start = Instant::now();
    let pb_clone = pb.clone();

    // ジェネリクスがややこしいが，stream::iterでチャンクのオフセットを生成して，それをmapでBytesに変換してる
    let stream = stream::iter(
        (0..)
            .map(move |i| i * chunk_size)
            .take_while(move |&offset| offset < total_bytes),
    )
    .map(move |offset| {
        // 最後のチャンクはサイズが小さくなる可能性があるので，minで調整
        let size = chunk_size.min(total_bytes - offset) as usize;
        // templateから必要なサイズだけ切り取る
        let chunk = template.slice(0..size);
        pb_clone.inc(size as u64);
        let elapsed = start.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            let mbps = (pb_clone.position() as f64 * 8.0) / (elapsed * 1_000_000.0);
            pb_clone.set_message(format!("{mbps:.1} Mbps"));
        }
        // chunk
        Ok::<Bytes, std::io::Error>(chunk)
    });

    client
        .post(UPLOAD_URL)
        .header("Content-Type", "application/octet-stream")
        .header("User-Agent", UA)
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await
        .into_diagnostic()
        .wrap_err("Failed to upload")?;

    let elapsed = start.elapsed().as_secs_f64();
    pb.finish_and_clear();

    let mbps = (total_bytes as f64 * 8.0) / (elapsed * 1_000_000.0);
    let mbps_colored = color_mbps(mbps);

    println!(
        "{}  {} {}  ({:.1}MiB/s)",
        "Upload:  ".bold(),
        mbps_colored,
        "Mbps".dimmed(),
        total_bytes as f64 / (elapsed * 1_048_576.0),
    );
    Ok(())
}

/// Mbpsの数値に応じて色を変える
fn color_mbps(mbps: f64) -> String {
    if mbps <= 10.0 {
        format!("{mbps:.2}").red().bold().to_string()
    } else if mbps <= 50.0 {
        format!("{mbps:.2}").yellow().bold().to_string()
    } else if mbps <= 100.0 {
        format!("{mbps:.2}").green().bold().to_string()
    } else {
        format!("{mbps:.2}").bright_cyan().bold().to_string()
    }
}

/// スピナーを作成するユーティリティ関数
/// 見りゃわかる
fn spinner(msg: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_information_is_opt_in() {
        assert!(!Args::parse_from(["sweet"]).info);
        assert!(Args::parse_from(["sweet", "--info"]).info);
        assert!(Args::parse_from(["sweet", "-i"]).info);
    }

    #[test]
    fn accepts_short_and_long_transfer_options() {
        for option in ["-d", "--download-only"] {
            assert!(Args::parse_from(["sweet", option]).download_only);
        }
        for option in ["-u", "--upload-only"] {
            assert!(Args::parse_from(["sweet", option]).upload_only);
        }
    }

    #[test]
    fn rejects_removed_options() {
        assert!(Args::try_parse_from(["sweet", "--i"]).is_err());
        assert!(Args::try_parse_from(["sweet", "--ping-only"]).is_err());
        for option in ["--ping-count", "--download-mb", "--upload-mb"] {
            assert!(Args::try_parse_from(["sweet", option, "10"]).is_err());
        }
    }

    #[test]
    fn ping_count_is_optional_and_bounded() {
        assert_eq!(Args::parse_from(["sweet"]).ping, None);
        for option in ["-p", "--ping"] {
            assert_eq!(Args::parse_from(["sweet", option]).ping, Some(PING_COUNT));
            for count in ["1", "10", "100"] {
                assert_eq!(
                    Args::parse_from(["sweet", option, count]).ping,
                    Some(count.parse().unwrap())
                );
            }
            for count in ["0", "101", "-1", "1.5", "abc"] {
                assert!(Args::try_parse_from(["sweet", option, count]).is_err());
            }
        }
        let args = Args::parse_from(["sweet", "--ping", "--info"]);
        assert_eq!(args.ping, Some(PING_COUNT));
        assert!(args.info);
    }

    #[tokio::test]
    async fn skips_network_information_without_info() {
        let args = Args::parse_from(["sweet", "--ping"]);
        let client = Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();

        // Failed pings are non-fatal, but a metadata request through this proxy would fail.
        assert!(run_inner(&args, &client).await.is_ok());
    }
}

/// プログレスバーを作成するユーティリティ関数
/// 見りゃわかる2
fn progress_bar(total: u64, prefix: &str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template(
            "{prefix:.bold} {bar:40.cyan/blue} {bytes}/{total_bytes}  {elapsed}  (ETA {eta})  {msg}",
        )
        .unwrap()
        .progress_chars("█▓░"),
    );
    pb.set_prefix(prefix.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

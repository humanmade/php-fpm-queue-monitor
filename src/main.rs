use anyhow::{Context, Result};
use aws_config::BehaviorVersion;
use aws_sdk_cloudwatch::{
    types::{Dimension, MetricDatum, StandardUnit},
    Client as CloudWatchClient,
};
use clap::Parser;
use serde_json::Value;
use std::process::Command;
use std::time::Duration;
use tokio::time;
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Interval in seconds to run the monitoring loop
    #[arg(short, long, default_value_t = 10)]
    interval: u64,

    /// AWS region (defaults to environment variable or AWS config)
    #[arg(short, long)]
    region: Option<String>,

    /// CloudWatch namespace for metrics
    #[arg(short, long, default_value = "PhpFpm")]
    namespace: String,

    /// Dimensions for metrics as key=value pairs (can be specified multiple times)
    #[arg(short, long)]
    dimension: Vec<String>,

    /// Dry run mode - don't send metrics to CloudWatch
    #[arg(long)]
    dry_run: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    info!("Starting PHP-FPM queue monitor");
    info!("Interval: {} seconds", args.interval);
    info!("Namespace: {}", args.namespace);
    info!("Dry run: {}", args.dry_run);

    // Initialize AWS config
    let config = if let Some(region) = &args.region {
        aws_config::defaults(BehaviorVersion::latest())
            .region(aws_config::Region::new(region.clone()))
            .load()
            .await
    } else {
        aws_config::load_defaults(BehaviorVersion::latest()).await
    };

    let cloudwatch = CloudWatchClient::new(&config);

    let mut interval = time::interval(Duration::from_secs(args.interval));

    loop {
        interval.tick().await;

        match collect_and_send_metrics(&cloudwatch, &args).await {
            Ok(()) => {}
            Err(e) => {
                error!("Error in monitoring loop: {}", e);
            }
        }
    }
}

async fn collect_and_send_metrics(
    cloudwatch: &CloudWatchClient,
    args: &Args,
) -> Result<()> {
    let total_queue_len = collect_php_fpm_queue_length().await?;

    info!("Total queue length: {}", total_queue_len);

    // Only send metrics if queue length is greater than 0
    if total_queue_len > 0 {
        if args.dry_run {
            info!("Would send metric: {} to namespace {}", total_queue_len, args.namespace);
        } else {
            send_cloudwatch_metric(cloudwatch, args, total_queue_len).await?;
            info!("Sent metric to CloudWatch: {}", total_queue_len);
        }
    }

    Ok(())
}

async fn collect_php_fpm_queue_length() -> Result<i32> {
    let container_ids = get_docker_container_ids().await?;
    let mut total_queue_len = 0;

    for container_id in container_ids {
        if is_php_fpm_container(&container_id).await? {
            let queue_len = get_container_queue_length(&container_id).await?;
            total_queue_len += queue_len;
        }
    }

    Ok(total_queue_len)
}

async fn get_docker_container_ids() -> Result<Vec<String>> {
    let output = Command::new("docker")
        .args(["ps", "-q"])
        .output()
        .context("Failed to execute docker ps -q")?;

    if !output.status.success() {
        anyhow::bail!("docker ps -q failed with status: {}", output.status);
    }

    let container_ids: Vec<String> = String::from_utf8(output.stdout)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
        .collect();

    Ok(container_ids)
}

async fn is_php_fpm_container(container_id: &str) -> Result<bool> {
    let output = Command::new("docker")
        .args(["inspect", container_id, "--format", "{{json .Config.Cmd}}"])
        .output()
        .context("Failed to execute docker inspect")?;

    if !output.status.success() {
        warn!("Failed to inspect container {}: {}", container_id, output.status);
        return Ok(false);
    }

    let cmd_json = String::from_utf8(output.stdout)?;
    let cmd: Value = serde_json::from_str(&cmd_json.trim())
        .context("Failed to parse docker inspect output as JSON")?;

    // Check if the command array contains "php-fpm"
    if let Some(cmd_array) = cmd.as_array() {
        Ok(cmd_array.iter().any(|v| {
            v.as_str().map_or(false, |s| s == "php-fpm")
        }))
    } else {
        Ok(false)
    }
}

async fn get_container_queue_length(container_id: &str) -> Result<i32> {
    // First get the container PID
    let pid_output = Command::new("docker")
        .args(["inspect", "-f", "{{.State.Pid}}", container_id])
        .output()
        .context("Failed to get container PID")?;

    if !pid_output.status.success() {
        anyhow::bail!("Failed to get PID for container {}", container_id);
    }

    let pid = String::from_utf8(pid_output.stdout)?
        .trim()
        .parse::<u32>()
        .context("Failed to parse PID")?;

    // Use nsenter and ss to get socket queue information
    let output = Command::new("sudo")
        .args([
            "nsenter",
            "-t",
            &pid.to_string(),
            "-n",
            "ss",
            "-lxnH",
        ])
        .output()
        .context("Failed to execute nsenter ss command")?;

    if !output.status.success() {
        warn!("nsenter ss failed for container {}: {}", container_id, output.status);
        return Ok(0);
    }

    let ss_output = String::from_utf8(output.stdout)?;

    // Parse the ss output to find the PHP-FPM socket and extract queue length
    for line in ss_output.lines() {
        if line.contains("/var/run/php-fpm/www.socket") {
            // Extract the third column (queue length) from ss output
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                if let Ok(queue_len) = parts[2].parse::<i32>() {
                    return Ok(queue_len);
                }
            }
        }
    }

    Ok(0)
}

async fn send_cloudwatch_metric(
    cloudwatch: &CloudWatchClient,
    args: &Args,
    value: i32,
) -> Result<()> {
    let mut dimensions = Vec::new();

    // Parse dimensions from CLI arguments (format: key=value)
    for dimension_str in &args.dimension {
        if let Some((key, value)) = dimension_str.split_once('=') {
            dimensions.push(
                Dimension::builder()
                    .name(key.trim())
                    .value(value.trim())
                    .build()
            );
        } else {
            warn!("Invalid dimension format '{}', expected key=value", dimension_str);
        }
    }

    let metric_datum = MetricDatum::builder()
        .metric_name("ListenQueue")
        .unit(StandardUnit::Count)
        .value(value as f64)
        .storage_resolution(1) // High resolution metric
        .set_dimensions(Some(dimensions))
        .build();

    info!("Prepared MetricDatum: {:?}", metric_datum);

    cloudwatch
        .put_metric_data()
        .namespace(&args.namespace)
        .metric_data(metric_datum)
        .send()
        .await
        .context("Failed to send metric to CloudWatch")?;

    Ok(())
}

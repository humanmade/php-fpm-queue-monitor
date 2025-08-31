# PHP-FPM Queue Monitor

A Rust application that monitors PHP-FPM socket queue lengths across Docker containers and sends metrics to AWS CloudWatch.

## Overview

This tool replaces the original bash script that monitors PHP-FPM containers. It:

1. **Discovers PHP-FPM containers**: Scans running Docker containers to find those running `php-fpm`
2. **Collects queue metrics**: Uses `nsenter` and `ss` commands to check socket queue lengths
3. **Sends to CloudWatch**: Uses the AWS SDK to send high-resolution metrics to CloudWatch


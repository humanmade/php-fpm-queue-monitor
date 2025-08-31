FROM amazonlinux:2

# Install development tools
RUN yum update -y && \
    yum groupinstall -y "Development Tools" && \
    yum install -y \
    curl \
    pkg-config \
    openssl-devel

# Install Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"

#!/bin/bash
export PATH=\ /root/.cargo/bin:\\
cd /root/growth-engine
set -a
source .env
set +a
# Resource guard
ulimit -u 512
ulimit -n 4096
echo \Starting growth-engine at \\
exec ./target/release/growth-engine

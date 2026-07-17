#!/usr/bin/env bash
set -e

anchor build -- --no-default-features --features pong
yarn generate ../crates/examples/pong

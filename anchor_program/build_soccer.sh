#!/usr/bin/env bash
set -e

anchor build -- --no-default-features --features soccer
yarn generate ../crates/examples/soccer

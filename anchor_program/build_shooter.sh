#!/usr/bin/env bash
set -e

anchor build -- --no-default-features --features shooter
yarn generate ../crates/examples/shooter

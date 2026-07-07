# Current MagicBlock ephemeral (ER) validator.
#
# The only Docker Hub image (magicblocklabs/validator:latest) is from Jun 2025 and
# uses an OLD config schema, so we instead wrap the up-to-date npm distribution,
# which ships a prebuilt native binary as a platform-specific optional dependency
# (@magicblock-labs/ephemeral-validator-linux-<arch>). Must be glibc (not Alpine/musl)
# AND new enough: the prebuilt binary needs GLIBC_2.39, so Debian trixie (glibc 2.41),
# not bookworm (2.36).
FROM node:22-trixie-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g @magicblock-labs/ephemeral-validator@0.13.3

WORKDIR /app

# The wrapper forwards all args straight to the native validator, which takes the
# config file path positionally. `--no-tui` is required under Docker: the validator
# has a terminal UI that can't init without a TTY (it would exit 1 with no logs).
# Config is bind-mounted at /app/config.toml by compose.
ENTRYPOINT ["ephemeral-validator"]
CMD ["--no-tui", "/app/config.toml"]

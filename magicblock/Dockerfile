# Current MagicBlock ephemeral (ER) validator.
#
# The only Docker Hub image (magicblocklabs/validator:latest) is from Jun 2025 and
# uses an OLD config schema, so we instead wrap the up-to-date npm distribution,
# which ships a prebuilt native binary as a platform-specific optional dependency
# (@magicblock-labs/ephemeral-validator-linux-<arch>). Must be glibc (not Alpine/musl)
# AND new enough: the prebuilt binary needs GLIBC_2.39, so Debian trixie (glibc 2.41),
# not bookworm (2.36).
FROM node:22-trixie-slim@sha256:c5849ff9c9ebcd66615412f0b548ca5b8ecaef84003dc9ac2e077ebe46aaa3f6

# Freeze the package repository alongside the base image.
RUN printf '%s\n' \
      'Types: deb' \
      'URIs: http://snapshot.debian.org/archive/debian/20260920T000000Z/' \
      'Suites: trixie trixie-updates' 'Components: main' \
      'Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg' 'Check-Valid-Until: no' \
      '' 'Types: deb' \
      'URIs: http://snapshot.debian.org/archive/debian-security/20260920T000000Z/' \
      'Suites: trixie-security' 'Components: main' \
      'Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg' 'Check-Valid-Until: no' \
      > /etc/apt/sources.list.d/debian.sources \
    && apt-get update \
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

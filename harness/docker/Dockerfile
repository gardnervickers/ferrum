# The millgui harness in a container: LinuxCNC 2.9.10 sim, Rust, Xvfb.
# Built and run by millgui/harness/docker-run (macOS or Linux with Docker).
# The repo is mounted at /src at run time; nothing of it is baked in except
# the setup script and the probe manager comp. Simulator only.
ARG BASE=ubuntu:24.04
FROM ${BASE}
SHELL ["/bin/bash", "-o", "pipefail", "-c"]

ENV DEBIAN_FRONTEND=noninteractive LANG=C.UTF-8
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends sudo curl ca-certificates git >/dev/null \
 && rm -rf /var/lib/apt/lists/*
# An extra CA for a TLS-intercepting proxy (HARNESS_EXTRA_CA); empty otherwise.
COPY extra-ca.crt /usr/local/share/ca-certificates/extra-ca.crt
RUN update-ca-certificates >/dev/null 2>&1 || true

# LinuxCNC refuses root: everything runs as cncsim.
RUN useradd -m -s /bin/bash cncsim && echo 'cncsim ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/cncsim

COPY setup-linuxcnc /tmp/setup/millgui/harness/setup-linuxcnc
COPY probe_manager.comp /tmp/setup/hal_components/probe_manager.comp
RUN mkdir -p /opt/lcnc && chown cncsim /opt/lcnc && chown -R cncsim /tmp/setup
USER cncsim
RUN /tmp/setup/millgui/harness/setup-linuxcnc /opt/lcnc && sudo rm -rf /var/lib/apt/lists/* /opt/lcnc/src/.git

# Rust (stable) for cncsim.
ENV RUSTUP_HOME=/home/cncsim/.rustup CARGO_HOME=/home/cncsim/.cargo PATH=/home/cncsim/.cargo/bin:$PATH
RUN curl -sSf -o /tmp/rustup-init "https://static.rust-lang.org/rustup/dist/$(uname -m)-unknown-linux-gnu/rustup-init" \
 && chmod +x /tmp/rustup-init && /tmp/rustup-init -y --profile minimal --default-toolchain stable >/dev/null \
 && rm /tmp/rustup-init && cargo --version

# Build output and the cargo registry live in named volumes (see docker-run),
# owned by cncsim from these directories.
RUN mkdir -p /home/cncsim/target /home/cncsim/.cargo/registry
ENV LCNC_DIR=/opt/lcnc CARGO_TARGET_DIR_NML=/home/cncsim/target
WORKDIR /src
ENTRYPOINT ["/src/millgui/harness/run"]

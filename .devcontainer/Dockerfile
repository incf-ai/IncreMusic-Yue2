# .devcontainer/Dockerfile
FROM mcr.microsoft.com/devcontainers/rust:1

RUN apt-get update && apt-get install -y \
    libx11-6 \
    libxcursor1 \
    libxrandr2 \
    libxi6 \
    libxkbcommon0 \
    libxkbcommon-x11-0 \
    libgl1 \
    libegl1 \
    && rm -rf /var/lib/apt/lists/*

# --- AMD ROCm userspace + ROCm-enabled onnxruntime -------------------------
#
# GPU: PCI 1002:7551, confirmed via `lspci` on the host to be Navi 48
# (RDNA4, gfx1201) -- an AMD Radeon AI PRO R9700. AMD's official support
# list added gfx1201 in ROCm 7.2 (Jan 2026); we install 7.2.4, the newest
# build actually published under https://repo.radeon.com/rocm/apt/ as of
# this writing.
#
# Base image is Debian 13 (trixie). AMD now ships native Debian 13
# instructions, but they still point at the Ubuntu 24.04 ("noble") binary
# repo rather than a Debian-native one -- that's intentional on AMD's part,
# not a workaround we invented.
#
# We only need ROCm's *userspace* runtime (HIP, rocBLAS, MIOpen, etc.) to
# build/link onnxruntime and run inference -- the amdgpu kernel driver
# itself lives on the host and is passed through via `/dev/dri`
# (see devcontainer.json), so we deliberately never install amdgpu-dkms
# here; that would try to build a kernel module for a kernel this
# container doesn't have.
ARG ROCM_VERSION=7.2.4
RUN mkdir --parents --mode=0755 /etc/apt/keyrings \
    && wget -qO- https://repo.radeon.com/rocm/rocm.gpg.key | gpg --dearmor > /etc/apt/keyrings/rocm.gpg \
    && printf 'deb [arch=amd64 signed-by=/etc/apt/keyrings/rocm.gpg] https://repo.radeon.com/rocm/apt/%s noble main\ndeb [arch=amd64 signed-by=/etc/apt/keyrings/rocm.gpg] https://repo.radeon.com/graphics/%s/ubuntu noble main\n' "$ROCM_VERSION" "$ROCM_VERSION" > /etc/apt/sources.list.d/rocm.list \
    && printf 'Package: *\nPin: release o=repo.radeon.com\nPin-Priority: 600\n' > /etc/apt/preferences.d/rocm-pin-600 \
    && apt-get update \
    && apt-get install -y rocm-dev rocm-libs \
    && rm -rf /var/lib/apt/lists/*

# `/dev/dri` render nodes are passed through from the host (runArgs
# `--device=/dev/dri`) but are owned by the host's `video`/`render` GIDs,
# which don't exist in this image by default. Force our `video`/`render`
# groups to the exact GIDs observed on the host (`getent group render
# video` on the host) and put `vscode` in both, so device access works as
# a non-root user without touching `--cap-drop=ALL`. If the host's GIDs
# ever change, override with --build-arg.
ARG HOST_VIDEO_GID=44
ARG HOST_RENDER_GID=992
RUN (getent group video >/dev/null || groupadd video) \
    && groupmod -g ${HOST_VIDEO_GID} video \
    && (getent group render >/dev/null || groupadd render) \
    && groupmod -g ${HOST_RENDER_GID} render \
    && usermod -aG video,render vscode

ENV LD_LIBRARY_PATH=/opt/rocm/lib${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}

RUN chown -R vscode:rustlang "$CARGO_HOME" "$RUSTUP_HOME"
USER vscode
RUN rustup toolchain install nightly --component llvm-tools-preview \
    && cargo install cargo-llvm-cov --locked

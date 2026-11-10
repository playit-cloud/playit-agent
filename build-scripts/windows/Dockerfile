# syntax=docker/dockerfile:1
# Packages already-built Windows programs into an MSI with WiX 3 under Wine.
FROM debian:trixie-20251229-slim

RUN dpkg --add-architecture i386 \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
       ca-certificates curl unzip xz-utils wine wine64 wine32:i386 xvfb xauth msitools python3 \
    && rm -rf /var/lib/apt/lists/*

# WiX 3.14.1, the WiX 3 release cargo-wix drives on the Windows CI runners.
RUN curl -fL --retry 3 -o /tmp/wix.zip \
      https://github.com/wixtoolset/wix3/releases/download/wix3141rtm/wix314-binaries.zip \
    && echo '6ac824e1642d6f7277d0ed7ea09411a508f6116ba6fae0aa5f2c7daa2ff43d31  /tmp/wix.zip' | sha256sum -c - \
    && mkdir -p /opt/wix \
    && unzip -q /tmp/wix.zip -d /opt/wix \
    && rm /tmp/wix.zip

RUN curl -fL --retry 3 -o /tmp/mono.tar.xz \
      https://github.com/wine-mono/wine-mono/releases/download/wine-mono-9.4.0/wine-mono-9.4.0-x86.tar.xz \
    && echo 'fd772219aacf46b825fa891a647af4a9ddf8439320101c231918b2037bf13858  /tmp/mono.tar.xz' | sha256sum -c - \
    && mkdir -p /usr/share/wine/mono \
    && tar -xf /tmp/mono.tar.xz -C /usr/share/wine/mono \
    && rm /tmp/mono.tar.xz

ENV WINEDEBUG=-all

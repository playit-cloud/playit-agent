###### DOWNLOAD RELEASE ARTIFACT ######
FROM alpine:3.18 AS artifact-downloader

ARG REPOSITORY
ARG VERSION
ARG TARGETARCH

WORKDIR /download

RUN apk add --no-cache dpkg wget
RUN case "${TARGETARCH}" in \
      amd64) deb_arch="amd64" ;; \
      arm64) deb_arch="arm64" ;; \
      *) echo "unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && wget -O playit.deb "https://github.com/${REPOSITORY}/releases/download/${VERSION}/playit_${deb_arch}.deb" \
    && dpkg-deb -x playit.deb /extract

########## RUNTIME CONTAINER ##########

FROM alpine:3.18
RUN apk add --no-cache ca-certificates

COPY --from=artifact-downloader /extract/opt/playit/playitd /usr/local/bin/playitd
RUN mkdir /playit
COPY docker/entrypoint.sh /playit/entrypoint.sh
RUN chmod +x /playit/entrypoint.sh

ENTRYPOINT ["/playit/entrypoint.sh"]

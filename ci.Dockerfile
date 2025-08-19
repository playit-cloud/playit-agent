###### DOWNLOAD RELEASE ARTIFACT ######
FROM alpine:3.18 AS artifact-downloader

ARG REPOSITORY
ARG VERSION

WORKDIR /download

RUN wget -O playit "https://github.com/${REPOSITORY}/releases/download/${VERSION}/playit-linux-$([[ "$(uname -m)" == "x86_64" ]] && echo "amd64" || echo "aarch64")" && chmod +x playit

########## RUNTIME CONTAINER ##########

FROM alpine:3.18
RUN apk add --no-cache ca-certificates

COPY --from=artifact-downloader /download/playit /usr/local/bin/playit
RUN mkdir /playit
COPY docker/entrypoint.sh /playit/entrypoint.sh
RUN chmod +x /playit/entrypoint.sh

ENTRYPOINT ["/playit/entrypoint.sh"]

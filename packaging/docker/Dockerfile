# The dvicode command in a container: `docker run --rm -v "$PWD:/src" ghcr.io/hexkdotcom/dvicode lint /src`.
# Built by the release workflow from a release's static Linux x64 engine (copied here as
# `dvicode`, with its license files). Git is there for `--changed-since`.
FROM alpine:3.20
RUN apk add --no-cache git \
 && git config --system --add safe.directory '*'
COPY dvicode /usr/local/bin/dvicode
COPY LICENSE NOTICE THIRD_PARTY_NOTICES.md /usr/share/doc/dvicode/
COPY licenses /usr/share/doc/dvicode/licenses
WORKDIR /src
ENTRYPOINT ["dvicode"]
CMD ["lint", "."]

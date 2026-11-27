# The service example, built in one image and run from a small one. Build
# it from the repository's root:
#
#   docker build -f examples/deploy/Dockerfile -t oxtel-service .
#   docker run --rm -p 8080:8080 oxtel-service
#
# For a program of your own, build your binary with `cargo build --release`
# and copy target/release/<your binary> instead.

FROM rust:1.99.1-slim-trixie AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p oxtel-examples --example service

FROM debian:trixie-20261104-slim
COPY --from=build /src/target/release/examples/service /usr/local/bin/service
# oxtel verifies HTTPS receivers with root certificates built into it, so
# the image needs no CA bundle. Run as a user without privileges.
USER 65532:65532
ENV PORT=8080
EXPOSE 8080
# The exec form, so the service is process 1 and receives SIGTERM itself.
ENTRYPOINT ["/usr/local/bin/service"]

#!/usr/bin/env bash
# Starts the OpenTelemetry Collector the telemetry crate's integration test
# sends to (crates/telemetry/tests/collector.rs): the image and the
# configuration shipped in deploy/k8s, with crates/telemetry/tests/collector/
# override.yaml laid over it. It shares the Docker machine's network with
# the runner, as the test PostgreSQL does, and listens on 127.0.0.1 only, at
# ports free when the job starts. Prints the two variables the test reads,
# for $GITHUB_ENV.
#
#   tools/ci/start-collector.sh <container name> <label> >> "$GITHUB_ENV"
set -euo pipefail

name=${1:?container name}
label=${2:?label}
root=$(cd "$(dirname "$0")/../.." && pwd)

free_port() { python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'; }
in_port=$(free_port)
grpc_port=$(free_port)
return_port=$(free_port)
health_port=$(free_port)

# The collector configuration, as the ConfigMap ic-otel-collector holds it.
shipped=$(awk '/^  config.yaml: \|/{f=1; next} f && /^---/{exit} f{sub(/^    /, ""); print}' "$root/deploy/k8s/telemetry.yaml")
# The Application Insights exporter is defined but not in the test's
# pipelines; its secret file does not exist here, so give it a dummy value.
shipped=${shipped//'${file:/mnt/secrets/appinsights-connection-string}'/'InstrumentationKey=00000000-0000-0000-0000-000000000000'}
# The same image the cluster runs (deploy/k8s/kustomization.yaml).
tag=$(awk '/name: otel-collector/{f=1} f && /newTag:/{print $2; exit}' "$root/deploy/k8s/kustomization.yaml")

docker run -d --name "$name" --label "$label" --network host \
  -e IC_COLLECTOR_SHIPPED="$shipped" \
  -e IC_COLLECTOR_OVERRIDE="$(cat "$root/crates/telemetry/tests/collector/override.yaml")" \
  -e IC_OTEL_IN_PORT="$in_port" -e IC_OTEL_GRPC_PORT="$grpc_port" \
  -e IC_OTEL_RETURN_PORT="$return_port" -e IC_OTEL_HEALTH_PORT="$health_port" \
  "otel/opentelemetry-collector-contrib:$tag" \
  --config=env:IC_COLLECTOR_SHIPPED --config=env:IC_COLLECTOR_OVERRIDE >/dev/null

for _ in $(seq 1 60); do
  if curl -fsS "http://127.0.0.1:$health_port/" >/dev/null 2>&1; then
    echo "IC_TEST_OTEL_COLLECTOR=127.0.0.1:$in_port"
    echo "IC_TEST_OTEL_RETURN=127.0.0.1:$return_port"
    exit 0
  fi
  sleep 1
done
docker logs "$name" >&2
exit 1

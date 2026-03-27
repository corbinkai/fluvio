set shell := ["bash", "-euo", "pipefail", "-c"]

root_dir     := justfile_directory()
cluster_name := "koshee-fluvio"
registry_host := "localhost:9050"
registry     := "koshee-dev-zot:5000"
namespace    := "fluvio-system"
kube_ctx     := "k3d-" + cluster_name
kubeconfig   := justfile_directory() + "/kubeconfig-k3d.yaml"

# Show available recipes
default:
    @echo "Koshee Fluvio Justfile"
    @echo ""
    @echo "Quick start:"
    @echo "  just shared-up         # shared-infra k3d bring-up"
    @echo "  just standalone-up     # standalone k3d bring-up"
    @echo "  just destroy           # delete local k3d cluster"
    @echo "  just build             # build CLI + cluster binary"
    @echo "  just test              # run unit tests"
    @echo "  just check             # fmt + clippy + docs"
    @echo ""
    @just --list --unsorted

# ============================================================
# BUILD
# ============================================================

# Build the Fluvio CLI
build-cli:
    cargo build --bin fluvio -p fluvio-cli

# Build the cluster binary (SC + SPU)
build-cluster:
    cargo build --bin fluvio-run -p fluvio-run

# Build both CLI and cluster binary
build: build-cli build-cluster

# Build in release mode
build-release:
    RELEASE=true cargo build --bin fluvio -p fluvio-cli --release
    RELEASE=true cargo build --bin fluvio-run -p fluvio-run --release

# Build the test harness
build-test:
    cargo build --bin fluvio-test -p fluvio-test

# Build SmartModule Development Kit
build-smdk:
    cargo build --bin smdk -p smartmodule-development-kit

# Build Connector Development Kit
build-cdk:
    cargo build --bin cdk -p cdk

# ============================================================
# TEST
# ============================================================

# Run all unit tests
test:
    cargo test --lib --all-features

# Run unit tests for a specific crate
test-crate crate:
    cargo test --lib -p {{ crate }}

# Run SPU + socket integration tests (requires SmartModules built)
test-integration: build-smartmodules
    cargo test --lib --all-features -p fluvio-spu -- --ignored --test-threads=1
    cargo test --lib --all-features -p fluvio-socket -- --ignored --test-threads=1
    cargo test --lib --all-features -p fluvio-service -- --ignored --test-threads=1
    cargo test -p fluvio-smartengine -- --ignored --test-threads=1

# Run k3d integration tests (requires cluster: just create-cluster)
test-k8-integration: create-cluster
    cargo test -p fluvio-sc --test k8_integration -- --ignored --test-threads=1

# Run E2E bats tests against k3d (requires cluster + built image)
test-e2e: create-cluster
    bats tests/e2e/kube_rs_e2e.bats

# Run ALL tests: unit + integration + k3d + e2e
test-all: build-smartmodules create-cluster
    #!/usr/bin/env bash
    set -euo pipefail
    echo "=== Unit tests ==="
    cargo test --lib --all-features
    echo ""
    echo "=== SPU integration tests (64 tests) ==="
    cargo test --lib --all-features -p fluvio-spu -- --ignored --test-threads=1
    echo ""
    echo "=== Socket integration tests (4 tests) ==="
    cargo test --lib --all-features -p fluvio-socket -- --ignored --test-threads=1
    echo ""
    echo "=== SmartEngine integration tests ==="
    cargo test -p fluvio-smartengine -- --ignored --test-threads=1
    echo ""
    echo "=== K3d controller integration tests (20 tests) ==="
    cargo test -p fluvio-sc --test k8_integration -- --ignored --test-threads=1
    echo ""
    echo "=== E2E bats tests (6 tests) ==="
    bats tests/e2e/kube_rs_e2e.bats
    echo ""
    echo "=== All tests passed ==="

# Run SmartModule engine tests
test-smartmodule: build-smartmodules
    cargo test -p fluvio-smartengine -- --ignored --nocapture

# Run doc tests
test-doc:
    cargo test --all-features --doc

# Run CLI smoke tests (bats)
test-cli-smoke:
    bats ./tests/cli/fluvio_smoke_tests/

# Run a single bats test file
test-bats file:
    bats {{ file }}

# Build SmartModule examples (needed by integration tests)
build-smartmodules: build-smdk
    #!/usr/bin/env bash
    set -euo pipefail
    export PATH="{{ root_dir }}/target/debug:$PATH"
    make -C smartmodule/examples build

# ============================================================
# CODE QUALITY
# ============================================================

# Run all checks (fmt + clippy + docs)
check: check-fmt check-clippy check-docs

# Check code formatting
check-fmt:
    cargo fmt -- --check

# Run clippy with all features
check-clippy:
    cargo check --all --all-features --tests
    cargo clippy --all --all-features --tests -- -D warnings -A clippy::upper_case_acronyms

# Check documentation builds
check-docs:
    cargo doc --no-deps

# Run cargo-deny security audit
check-audit:
    cargo deny check

# Format code
fmt:
    cargo fmt

# ============================================================
# K3D CLUSTER
# ============================================================

# Create k3d cluster and install Fluvio CRDs
create-cluster:
    #!/usr/bin/env bash
    set -euo pipefail
    if k3d cluster list 2>/dev/null | grep -qw {{ cluster_name }}; then
      echo "Cluster {{ cluster_name }} already exists"
    else
      k3d cluster create --config k3d/cluster.yaml
    fi
    k3d kubeconfig get {{ cluster_name }} > {{ kubeconfig }}
    echo "Waiting for cluster to be ready..."
    KUBECONFIG={{ kubeconfig }} kubectl wait --for=condition=Ready nodes --all --timeout=120s --context {{ kube_ctx }}
    KUBECONFIG={{ kubeconfig }} kubectl create namespace {{ namespace }} --context {{ kube_ctx }} 2>/dev/null || true
    KUBECONFIG={{ kubeconfig }} kubectl apply -f k8-util/helm/fluvio-sys/templates/ --context {{ kube_ctx }}
    echo "Fluvio CRDs installed in {{ namespace }}"

# Delete k3d cluster
delete-cluster:
    k3d cluster delete {{ cluster_name }}

shared-up: deploy

standalone-up: deploy

destroy: delete-cluster

# Stop k3d cluster (preserves state)
stop-cluster:
    k3d cluster stop {{ cluster_name }}

# Start a stopped k3d cluster
start-cluster:
    k3d cluster start {{ cluster_name }}

# Show cluster status
cluster-status:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "=== Cluster ==="
    k3d cluster list | grep {{ cluster_name }} || echo "Not found"
    echo ""
    echo "=== Nodes ==="
    KUBECONFIG={{ kubeconfig }} kubectl get nodes --context {{ kube_ctx }} 2>/dev/null || echo "Cluster not running"
    echo ""
    echo "=== Fluvio Resources ==="
    KUBECONFIG={{ kubeconfig }} kubectl get spugroups,spus,topics,statefulset,svc -n {{ namespace }} --context {{ kube_ctx }} 2>/dev/null || true

# ============================================================
# DEPLOY (K3D)
# ============================================================

# Build fluvio-run with musl for Alpine containers
build-musl:
    cargo zigbuild --bin fluvio-run -p fluvio-run --target x86_64-unknown-linux-musl

# Build and push fluvio-run image to k3d registry
push-image: build-musl
    #!/usr/bin/env bash
    set -euo pipefail
    HOST_IMAGE="{{ registry_host }}/fluvio:dev"

    echo "Building Docker image..."
    docker build \
      --build-arg BINARY=./target/x86_64-unknown-linux-musl/debug/fluvio-run \
      -f k8-util/docker/Dockerfile.fluvio-run \
      -t "${HOST_IMAGE}" \
      .

    echo "Pushing to k3d registry..."
    skopeo copy --tmpdir /tmp --dest-tls-verify=false "docker-daemon:${HOST_IMAGE}" "docker://${HOST_IMAGE}"
    echo "IMAGE={{ registry }}/fluvio:dev"

# Deploy SC to k3d cluster
deploy: create-cluster push-image
    #!/usr/bin/env bash
    set -euo pipefail
    IMAGE="{{ registry }}/fluvio:dev"

    echo "Deploying SC with image ${IMAGE}..."
    KUBECONFIG={{ kubeconfig }} kubectl delete configmap/spu-k8 \
      deployment/fluvio-sc \
      service/fluvio-sc-public \
      service/fluvio-sc-internal \
      service/fluvio-spg-main \
      service/fluvio-spu-main-0 \
      service/fluvio-spu-main-1 \
      serviceaccount/fluvio \
      role/fluvio \
      rolebinding/fluvio \
      statefulset/fluvio-spg-main \
      spugroup/main \
      -n {{ namespace }} \
      --context {{ kube_ctx }} \
      --ignore-not-found
    KUBECONFIG={{ kubeconfig }} kubectl delete spu --all -n {{ namespace }} --context {{ kube_ctx }} --ignore-not-found
    KUBECONFIG={{ kubeconfig }} kubectl delete pvc --all -n {{ namespace }} --context {{ kube_ctx }} --ignore-not-found

    helm upgrade --install fluvio-app ./k8-util/helm/fluvio-app \
      --namespace {{ namespace }} \
      --kubeconfig {{ kubeconfig }} \
      --kube-context {{ kube_ctx }} \
      --set image.registry={{ registry }} \
      --set image.tag=dev \
      --set image.pullPolicy=Always \
      --set service.type=ClusterIP \
      --set serviceAccount.name=fluvio
    KUBECONFIG={{ kubeconfig }} kubectl patch configmap/spu-k8 -n {{ namespace }} --context {{ kube_ctx }} --type=merge -p '{
      "data": {
        "lbServiceAnnotations": "{\"fluvio.io/ingress-address\":\"fluvio-spu-main-0\"}"
      }
    }'

    KUBECONFIG={{ kubeconfig }} kubectl apply --context {{ kube_ctx }} -n {{ namespace }} -f - <<EOF
    apiVersion: fluvio.infinyon.com/v1
    kind: SpuGroup
    metadata:
      name: main
      namespace: {{ namespace }}
    spec:
      replicas: 1
      minId: 0
    EOF

    echo "Waiting for SC deployment..."
    KUBECONFIG={{ kubeconfig }} kubectl rollout status deployment/fluvio-sc -n {{ namespace }} --context {{ kube_ctx }} --timeout=180s
    until KUBECONFIG={{ kubeconfig }} kubectl get statefulset/fluvio-spg-main -n {{ namespace }} --context {{ kube_ctx }} >/dev/null 2>&1; do
      sleep 2
    done
    SC_INTERNAL_IP=$(KUBECONFIG={{ kubeconfig }} kubectl get svc fluvio-sc-internal -n {{ namespace }} --context {{ kube_ctx }} -o jsonpath='{.spec.clusterIP}')
    KUBECONFIG={{ kubeconfig }} kubectl patch statefulset/fluvio-spg-main -n {{ namespace }} --context {{ kube_ctx }} --type=merge -p "{
      \"spec\": {
        \"template\": {
          \"spec\": {
            \"hostAliases\": [
              {
                \"ip\": \"${SC_INTERNAL_IP}\",
                \"hostnames\": [\"fluvio-sc-internal.fluvio-system.svc.cluster.local\"]
              }
            ]
          }
        }
      }
    }"
    KUBECONFIG={{ kubeconfig }} kubectl delete pod fluvio-spg-main-0 -n {{ namespace }} --context {{ kube_ctx }} --ignore-not-found
    echo "Waiting for SPU StatefulSet..."
    KUBECONFIG={{ kubeconfig }} kubectl rollout status statefulset/fluvio-spg-main -n {{ namespace }} --context {{ kube_ctx }} --timeout=300s
    KUBECONFIG={{ kubeconfig }} kubectl get deploy,sts,svc,spugroup,spu -n {{ namespace }} --context {{ kube_ctx }}

# Run SC locally against k3d cluster
run-sc:
    ./target/debug/fluvio-run sc --namespace {{ namespace }} --k8

# Create a test SpuGroup
create-spugroup replicas="2":
    #!/usr/bin/env bash
    set -euo pipefail
    KUBECONFIG={{ kubeconfig }} kubectl apply --context {{ kube_ctx }} -f - <<EOF
    apiVersion: fluvio.infinyon.com/v1
    kind: SpuGroup
    metadata:
      name: main
      namespace: {{ namespace }}
    spec:
      replicas: {{ replicas }}
      minId: 0
    EOF
    echo "SpuGroup 'main' created with {{ replicas }} replicas"

# ============================================================
# LOCAL DEVELOPMENT
# ============================================================

# Start a local cluster (no k3d, no k8s)
local-start: build
    ./target/debug/fluvio cluster start --local

# Create a topic on the local cluster
local-topic name:
    ./target/debug/fluvio topic create {{ name }}

# Produce a message to a topic
local-produce topic:
    echo "hello fluvio" | ./target/debug/fluvio produce {{ topic }}

# Consume from a topic
local-consume topic:
    ./target/debug/fluvio consume {{ topic }} -B -d

# Quick smoke test: create topic, produce, consume
local-smoke: local-start
    #!/usr/bin/env bash
    set -euo pipefail
    FLVD=./target/debug/fluvio
    TOPIC="smoke-test-$(date +%s)"
    ${FLVD} topic create ${TOPIC}
    echo "hello from justfile" | ${FLVD} produce ${TOPIC}
    ${FLVD} consume ${TOPIC} -B -d
    ${FLVD} topic delete ${TOPIC}
    echo "Smoke test passed"

# ============================================================
# E2E TESTING (K3D)
# ============================================================

# Full k3d e2e test: cluster + deploy + spugroup + verify
e2e: create-cluster
    #!/usr/bin/env bash
    set -euo pipefail

    echo "=== Building ==="
    just build-cluster

    echo "=== Starting SC locally ==="
    ./target/debug/fluvio-run sc --namespace {{ namespace }} --k8 &
    SC_PID=$!
    sleep 5

    echo "=== Creating SpuGroup ==="
    just create-spugroup 2

    sleep 5

    echo "=== Verifying resources ==="
    echo "--- SPU CRDs ---"
    KUBECONFIG={{ kubeconfig }} kubectl get spus -n {{ namespace }} --context {{ kube_ctx }}
    echo "--- Services ---"
    KUBECONFIG={{ kubeconfig }} kubectl get svc -n {{ namespace }} --context {{ kube_ctx }}
    echo "--- StatefulSets ---"
    KUBECONFIG={{ kubeconfig }} kubectl get statefulset -n {{ namespace }} --context {{ kube_ctx }}

    kill $SC_PID 2>/dev/null || true
    wait $SC_PID 2>/dev/null || true
    echo "E2E test complete"

# ============================================================
# CLEANUP
# ============================================================

# Clean build artifacts
clean:
    cargo clean

# Full clean: build artifacts + k3d cluster
clean-all: clean
    k3d cluster delete {{ cluster_name }} 2>/dev/null || true

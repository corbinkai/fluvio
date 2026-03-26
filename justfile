set shell := ["bash", "-euo", "pipefail", "-c"]

root_dir     := justfile_directory()
cluster_name := "koshee-fluvio"
registry     := "koshee-fluvio-registry.localhost:16050"
namespace    := "fluvio-system"
kube_ctx     := "k3d-" + cluster_name

# Show available recipes
default:
    @echo "Koshee Fluvio Justfile"
    @echo ""
    @echo "Quick start:"
    @echo "  just create-cluster    # create k3d cluster + install CRDs"
    @echo "  just build             # build CLI + cluster binary"
    @echo "  just test              # run unit tests"
    @echo "  just check             # fmt + clippy + docs"
    @echo "  just deploy            # build image + deploy SC to k3d"
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

# Run integration tests (requires SmartModules built first)
test-integration: build-smartmodules
    cargo test --lib --all-features -p fluvio-spu -- --ignored --test-threads=1
    cargo test --lib --all-features -p fluvio-socket -- --ignored --test-threads=1
    cargo test --lib --all-features -p fluvio-service -- --ignored --test-threads=1
    cargo test -p fluvio-smartengine -- --ignored --test-threads=1

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
build-smartmodules:
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
      echo "Waiting for cluster to be ready..."
      kubectl wait --for=condition=Ready nodes --all --timeout=120s --context {{ kube_ctx }}
    fi
    kubectl create namespace {{ namespace }} --context {{ kube_ctx }} 2>/dev/null || true
    kubectl apply -f k8-util/helm/fluvio-sys/templates/ --context {{ kube_ctx }}
    echo "Fluvio CRDs installed in {{ namespace }}"

# Delete k3d cluster
delete-cluster:
    k3d cluster delete {{ cluster_name }}

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
    kubectl get nodes --context {{ kube_ctx }} 2>/dev/null || echo "Cluster not running"
    echo ""
    echo "=== Fluvio Resources ==="
    kubectl get spugroups,spus,topics,statefulset,svc -n {{ namespace }} --context {{ kube_ctx }} 2>/dev/null || true

# ============================================================
# DEPLOY (K3D)
# ============================================================

# Build and push fluvio-run image to k3d registry
push-image: build-cluster
    #!/usr/bin/env bash
    set -euo pipefail
    TAG=$(date +%s)
    IMAGE="{{ registry }}/fluvio-run:${TAG}"

    echo "Building Docker image..."
    docker build \
      --build-arg BINARY=./target/debug/fluvio-run \
      -f k8-util/docker/Dockerfile.fluvio-run \
      -t "${IMAGE}" \
      .

    echo "Pushing to k3d registry..."
    docker push "${IMAGE}"
    echo "IMAGE=${IMAGE}"

# Deploy SC to k3d cluster
deploy: push-image
    #!/usr/bin/env bash
    set -euo pipefail
    TAG=$(docker images {{ registry }}/fluvio-run --format '{{ '{{' }}.Tag{{ '}}' }}' | head -1)
    IMAGE="{{ registry }}/fluvio-run:${TAG}"

    echo "Deploying SC with image ${IMAGE}..."
    # Create spu-k8 ConfigMap if not exists
    kubectl create configmap spu-k8 \
      --from-literal=image="${IMAGE}" \
      -n {{ namespace }} --context {{ kube_ctx }} \
      --dry-run=client -o yaml | kubectl apply --context {{ kube_ctx }} -f -

    # TODO: Deploy SC as a Deployment once Helm chart is ready
    echo "Run SC locally for now: ./target/debug/fluvio-run sc --namespace {{ namespace }} --k8"

# Run SC locally against k3d cluster
run-sc:
    ./target/debug/fluvio-run sc --namespace {{ namespace }} --k8

# Create a test SpuGroup
create-spugroup replicas="2":
    #!/usr/bin/env bash
    set -euo pipefail
    kubectl apply --context {{ kube_ctx }} -f - <<EOF
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
    kubectl get spus -n {{ namespace }} --context {{ kube_ctx }}
    echo "--- Services ---"
    kubectl get svc -n {{ namespace }} --context {{ kube_ctx }}
    echo "--- StatefulSets ---"
    kubectl get statefulset -n {{ namespace }} --context {{ kube_ctx }}

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

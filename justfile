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

    # Copy binary for the prebuilt Dockerfile stage
    cp target/x86_64-unknown-linux-musl/debug/fluvio-run fluvio-run
    trap 'rm -f fluvio-run' EXIT

    echo "Building Docker image..."
    docker build \
      -f k8-util/docker/Dockerfile \
      -t "${HOST_IMAGE}" \
      .

    echo "Pushing to k3d registry..."
    skopeo copy --tmpdir /tmp --dest-tls-verify=false "docker-daemon:${HOST_IMAGE}" "docker://${HOST_IMAGE}"
    echo "IMAGE={{ registry }}/fluvio:dev"

# Build and push namespace-gc image to k3d registry
push-namespace-gc: build-musl-gc
    #!/usr/bin/env bash
    set -euo pipefail
    HOST_IMAGE="{{ registry_host }}/fluvio-namespace-gc:dev"

    cp target/x86_64-unknown-linux-musl/debug/fluvio-namespace-gc fluvio-namespace-gc
    trap 'rm -f fluvio-namespace-gc' EXIT

    docker build \
      -f k8-util/docker/Dockerfile.namespace-gc \
      -t "${HOST_IMAGE}" \
      .

    skopeo copy --tmpdir /tmp --dest-tls-verify=false "docker-daemon:${HOST_IMAGE}" "docker://${HOST_IMAGE}"
    echo "IMAGE={{ registry }}/fluvio-namespace-gc:dev"

# Build namespace-gc with musl
build-musl-gc:
    cargo zigbuild --bin fluvio-namespace-gc -p fluvio-namespace-gc --target x86_64-unknown-linux-musl

# Deploy Fluvio to k3d cluster via Helm
deploy: create-cluster push-image
    #!/usr/bin/env bash
    set -euo pipefail

    echo "Installing Fluvio CRDs..."
    KUBECONFIG={{ kubeconfig }} kubectl apply -f k8-util/helm/fluvio-sys/templates/ --context {{ kube_ctx }}

    echo "Deploying Fluvio via Helm..."
    helm upgrade --install fluvio-app ./k8-util/helm/fluvio-app \
      --namespace {{ namespace }} \
      --kubeconfig {{ kubeconfig }} \
      --kube-context {{ kube_ctx }} \
      --set image.registry={{ registry }} \
      --set image.repository=fluvio \
      --set image.tag=dev \
      --set image.pullPolicy=Always \
      --set service.type=ClusterIP \
      --set spuGroup.enabled=true \
      --set spuGroup.replicas=1 \
      --wait --timeout 180s

    echo "Waiting for StatefulSet..."
    until KUBECONFIG={{ kubeconfig }} kubectl get statefulset/fluvio-spg-main -n {{ namespace }} --context {{ kube_ctx }} >/dev/null 2>&1; do
      sleep 2
    done
    KUBECONFIG={{ kubeconfig }} kubectl rollout status statefulset/fluvio-spg-main -n {{ namespace }} --context {{ kube_ctx }} --timeout=300s
    KUBECONFIG={{ kubeconfig }} kubectl get deploy,sts,svc,spugroup,spu -n {{ namespace }} --context {{ kube_ctx }}

# Redeploy (clean slate — deletes PVCs and all resources, then reinstalls)
redeploy: create-cluster push-image
    #!/usr/bin/env bash
    set -euo pipefail

    echo "Cleaning up existing resources..."
    helm uninstall fluvio-app \
      --namespace {{ namespace }} \
      --kubeconfig {{ kubeconfig }} \
      --kube-context {{ kube_ctx }} \
      --ignore-not-found 2>/dev/null || true
    KUBECONFIG={{ kubeconfig }} kubectl delete spu --all -n {{ namespace }} --context {{ kube_ctx }} --ignore-not-found
    KUBECONFIG={{ kubeconfig }} kubectl delete pvc --all -n {{ namespace }} --context {{ kube_ctx }} --ignore-not-found
    sleep 2

    just deploy

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
# DEVSPACE + SKUPPER (shared infra integration)
# ============================================================

# Set up Skupper cross-cluster service mesh to shared infra
skupper-setup:
    #!/usr/bin/env bash
    set -euo pipefail
    KUBECONFIG={{ kubeconfig }} kubectl create namespace {{ namespace }} --context {{ kube_ctx }} --dry-run=client -o yaml | \
      KUBECONFIG={{ kubeconfig }} kubectl apply --context {{ kube_ctx }} -f -
    # Copy fresh token from infra repo if available
    if [ -f ../infrastructure/k3d-infra/skupper/token.yaml ]; then
      cp ../infrastructure/k3d-infra/skupper/token.yaml skupper/token.yaml
    fi
    # Install Skupper controller
    helm upgrade --install skupper oci://quay.io/skupper/helm/skupper \
      --version 2.1.3 \
      --namespace {{ namespace }} \
      --kubeconfig {{ kubeconfig }} \
      --kube-context {{ kube_ctx }} \
      --wait
    # Apply site, token, and listeners
    KUBECONFIG={{ kubeconfig }} kubectl apply -f skupper/site.yaml -n {{ namespace }} --context {{ kube_ctx }}
    if [ -f skupper/token.yaml ]; then
      KUBECONFIG={{ kubeconfig }} kubectl apply -f skupper/token.yaml -n {{ namespace }} --context {{ kube_ctx }}
    fi
    KUBECONFIG={{ kubeconfig }} kubectl apply -f skupper/listeners.yaml -n {{ namespace }} --context {{ kube_ctx }}
    echo "Skupper setup complete — shared services available as:"
    echo "  uptrace:14317      (OTLP gRPC)"
    echo "  uptrace-http:14319 (Uptrace UI)"
    echo "  mailcrab:1025      (SMTP)"
    echo "  mailcrab-web:1080  (Mail inbox)"

# Start development with DevSpace (builds, deploys, sets up Skupper)
dev: create-cluster
    devspace dev --kubeconfig {{ kubeconfig }} --kube-context {{ kube_ctx }} --namespace {{ namespace }}

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

# ============================================================
# GHCR IMAGE PUSH (emergency / manual)
# ============================================================

ghcr_registry := "ghcr.io"
ghcr_org      := "koshee-ai"

# Cross-compile and push fluvio-run to GHCR (arm64 via cargo-zigbuild)
emergency-push-fluvio tag="latest":
    docker buildx build \
      --platform linux/arm64 \
      --build-arg BUILD_MODE=zigbuild \
      -f k8-util/docker/Dockerfile \
      --push \
      -t {{ ghcr_registry }}/{{ ghcr_org }}/fluvio:{{ tag }} \
      .

# Cross-compile and push namespace-gc to GHCR (arm64 via cargo-zigbuild)
emergency-push-gc tag="latest":
    docker buildx build \
      --platform linux/arm64 \
      --build-arg BUILD_MODE=zigbuild \
      -f k8-util/docker/Dockerfile.namespace-gc \
      --push \
      -t {{ ghcr_registry }}/{{ ghcr_org }}/fluvio-namespace-gc:{{ tag }} \
      .

# Push all images to GHCR
emergency-push tag="latest": (emergency-push-fluvio tag) (emergency-push-gc tag)

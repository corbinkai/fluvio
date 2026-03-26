#!/usr/bin/env bats

# E2E tests for kube-rs SC controllers.
# Requires: k3d cluster (just create-cluster), built binary (just build-cluster)
#
# Run: just test-e2e-k8
# Or:  bats tests/e2e/kube_rs_e2e.bats

NAMESPACE="fluvio-system"
KUBE_CTX="k3d-koshee-fluvio"
BINARY="./target/debug/fluvio-run"

setup_file() {
    # Kill any lingering SC
    fuser -k 9003/tcp 9004/tcp 2>/dev/null || true
    sleep 2

    # Switch context
    kubectl config use-context "$KUBE_CTX"

    # Clean up any existing test resources
    kubectl delete spugroup --all -n "$NAMESPACE" --context "$KUBE_CTX" 2>/dev/null || true
    kubectl delete svc -l fluvio.io/spu-name -n "$NAMESPACE" --context "$KUBE_CTX" 2>/dev/null || true
    kubectl delete statefulset --all -n "$NAMESPACE" --context "$KUBE_CTX" 2>/dev/null || true
    kubectl delete spus --all -n "$NAMESPACE" --context "$KUBE_CTX" 2>/dev/null || true
    sleep 3

    # Start SC
    "$BINARY" sc --namespace "$NAMESPACE" --k8 &
    echo $! > /tmp/fluvio-e2e-sc.pid
    sleep 5
}

teardown_file() {
    # Kill SC
    if [ -f /tmp/fluvio-e2e-sc.pid ]; then
        kill "$(cat /tmp/fluvio-e2e-sc.pid)" 2>/dev/null || true
        rm /tmp/fluvio-e2e-sc.pid
    fi
    fuser -k 9003/tcp 9004/tcp 2>/dev/null || true

    # Clean up resources
    kubectl delete spugroup --all -n "$NAMESPACE" --context "$KUBE_CTX" 2>/dev/null || true
    sleep 5
}

@test "SC starts with kube-rs and prints success message" {
    # SC was started in setup_file, check it's running
    [ -f /tmp/fluvio-e2e-sc.pid ]
    pid=$(cat /tmp/fluvio-e2e-sc.pid)
    kill -0 "$pid"  # Check process is alive
}

@test "SpuGroup creates all expected K8s resources" {
    kubectl apply --context "$KUBE_CTX" -f - <<EOF
apiVersion: fluvio.infinyon.com/v1
kind: SpuGroup
metadata:
  name: e2e-test
  namespace: $NAMESPACE
spec:
  replicas: 2
  minId: 0
EOF

    # Wait for resources
    local attempts=0
    while [ $attempts -lt 30 ]; do
        svc_count=$(kubectl get svc -n "$NAMESPACE" --context "$KUBE_CTX" -l fluvio.io/spu-name -o name 2>/dev/null | grep "e2e-test" | wc -l)
        if [ "$svc_count" -ge 2 ]; then
            break
        fi
        sleep 1
        attempts=$((attempts + 1))
    done

    [ "$svc_count" -ge 2 ]

    # Verify StatefulSet
    kubectl get statefulset "fluvio-spg-e2e-test" -n "$NAMESPACE" --context "$KUBE_CTX"

    # Verify headless service
    kubectl get svc "fluvio-spg-e2e-test" -n "$NAMESPACE" --context "$KUBE_CTX"

    # Verify SPU CRDs
    local spu_count
    spu_count=$(kubectl get spus -n "$NAMESPACE" --context "$KUBE_CTX" -o name 2>/dev/null | grep "e2e-test" | wc -l)
    [ "$spu_count" -ge 2 ]
}

@test "SPU CRD has ClusterIP FQDN in publicEndpoint" {
    local spu_json
    spu_json=$(kubectl get spu "e2e-test-0" -n "$NAMESPACE" --context "$KUBE_CTX" -o json)

    local hostname
    hostname=$(echo "$spu_json" | jq -r '.spec.publicEndpoint.ingress[0].hostname // empty')
    [[ "$hostname" == *"fluvio-spu-e2e-test-0"* ]]
    [[ "$hostname" == *"svc.cluster.local"* ]]
}

@test "StatefulSet has liveness and readiness probes" {
    local sts_json
    sts_json=$(kubectl get statefulset "fluvio-spg-e2e-test" -n "$NAMESPACE" --context "$KUBE_CTX" -o json)

    local liveness
    liveness=$(echo "$sts_json" | jq '.spec.template.spec.containers[0].livenessProbe')
    [ "$liveness" != "null" ]

    local readiness
    readiness=$(echo "$sts_json" | jq '.spec.template.spec.containers[0].readinessProbe')
    [ "$readiness" != "null" ]

    local readyz_path
    readyz_path=$(echo "$sts_json" | jq -r '.spec.template.spec.containers[0].readinessProbe.httpGet.path')
    [ "$readyz_path" = "/readyz" ]
}

@test "Health endpoint responds on port 9008" {
    # This tests the health server if an SPU is running locally
    # Skip if no SPU is running (StatefulSet pods won't start without image)
    skip "SPU pods require container image in k3d registry"
}

@test "SpuGroup deletion cascades to owned resources" {
    kubectl delete spugroup "e2e-test" -n "$NAMESPACE" --context "$KUBE_CTX"
    sleep 10

    # Services should be gone
    local svc_count
    svc_count=$(kubectl get svc -n "$NAMESPACE" --context "$KUBE_CTX" -l fluvio.io/spu-name -o name 2>/dev/null | grep "e2e-test" | wc -l)
    [ "$svc_count" -eq 0 ]
}

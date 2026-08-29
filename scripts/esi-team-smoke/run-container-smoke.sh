#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
short_id=$(printf '%08x' "$$")
image="forgeloop-ai-esi-team-smoke-$short_id"
container="forgeloop-ai-esi-team-smoke-$short_id"
run_id="task-m12-006-$short_id"
workspace_id="clean-team-$short_id"

cleanup() {
    docker rm -f "$container" >/dev/null 2>&1 || true
    docker image rm "$image" >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM

docker build \
    --file "$repository_root/scripts/esi-team-smoke/Dockerfile" \
    --tag "$image" \
    --label forgeloop.managed=true \
    --label forgeloop.project=forgeloop-ai \
    --label forgeloop.resource=esi-team-smoke-image \
    --label forgeloop.task_id=TASK-M12-006 \
    --label "forgeloop.run_id=$run_id" \
    --label "forgeloop.workspace_id=$workspace_id" \
    "$repository_root"

docker run --rm \
    --name "$container" \
    --network none \
    --label forgeloop.managed=true \
    --label forgeloop.project=forgeloop-ai \
    --label forgeloop.resource=esi-team-smoke \
    --label forgeloop.task_id=TASK-M12-006 \
    --label "forgeloop.run_id=$run_id" \
    --label "forgeloop.workspace_id=$workspace_id" \
    "$image"
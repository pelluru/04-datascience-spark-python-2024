#!/usr/bin/env bash

# Exits as soon as any line fails.
set -euo pipefail

# Create multi-arch docker images from ${BUILDKITE_COMMIT}-x86_64 and ${BUILDKITE_COMMIT}-aarch64
# They are created by ci/scripts/docker.sh
#
# Also add addtional tags to the images:
# nightly-yyyyMMdd: nightly build in main-cron
# latest: only push to ghcr. dockerhub latest is latest release

date="$(date +%Y%m%d)"
ghcraddr="ghcr.io/risingwavelabs/risingwave"
dockerhubaddr="risingwavelabs/risingwave"


arches=()

if [ "${SKIP_TARGET_AMD64:-false}" != "true" ]; then
  arches+=("x86_64")
fi

if [ "${SKIP_TARGET_AARCH64:-false}" != "true" ]; then
  arches+=("aarch64")
fi

echo "--- arches: ${arches[*]}"

# push images to gchr
function pushGchr() {
  GHCRTAG="${ghcraddr}:$1"
  echo "push to gchr, image tag: ${GHCRTAG}"
  args=()
  for arch in "${arches[@]}"
  do
    args+=( --amend "${ghcraddr}:${BUILDKITE_COMMIT}-${arch}" )
  done
  docker manifest create --insecure "$GHCRTAG" "${args[@]}"
  docker manifest push --insecure "$GHCRTAG"
}

# push images to dockerhub
function pushDockerhub() {
  DOCKERTAG="${dockerhubaddr}:$1"
  echo "push to dockerhub, image tag: ${DOCKERTAG}"
  args=()
  for arch in "${arches[@]}"
  do
    args+=( --amend "${dockerhubaddr}:${BUILDKITE_COMMIT}-${arch}" )
  done
  docker manifest create --insecure "$DOCKERTAG" "${args[@]}"
  docker manifest push --insecure "$DOCKERTAG"
}

echo "--- ghcr login"
echo "$GHCR_TOKEN" | docker login ghcr.io -u "$GHCR_USERNAME" --password-stdin

echo "--- dockerhub login"
echo "$DOCKER_TOKEN" | docker login -u "risingwavelabs" --password-stdin

if [[ -n "${ORIGINAL_IMAGE_TAG+x}" ]] && [[ -n "${NEW_IMAGE_TAG+x}" ]]; then
  echo "--- retag docker image"
  echo "push to gchr, image tag: ${ghcraddr}:${NEW_IMAGE_TAG}"
  args=()
  for arch in "${arches[@]}"
  do
    args+=( --amend "${ghcraddr}:${NEW_IMAGE_TAG}-${arch}" )
  done
  docker manifest create --insecure "${ghcraddr}:${NEW_IMAGE_TAG}" "${args[@]}"
  docker manifest push --insecure "${ghcraddr}:${NEW_IMAGE_TAG}"
  exit 0
fi

echo "--- multi arch image create"
if [[ "${#BUILDKITE_COMMIT}" = 40 ]]; then
  # If the commit is 40 characters long, it's probably a SHA.
  TAG="git-${BUILDKITE_COMMIT}"
  pushGchr "${TAG}"
fi

if [ "${BUILDKITE_SOURCE}" == "schedule" ]; then
  # If this is a schedule build, tag the image with the date.
  TAG="nightly-${date}"
  pushGchr "${TAG}"
  pushDockerhub "${TAG}"
  TAG="latest"
  pushGchr ${TAG}
fi

if [[ -n "${IMAGE_TAG+x}" ]]; then
  # Tag the image with the $IMAGE_TAG.
  TAG="${IMAGE_TAG}"
  pushGchr "${TAG}"
fi

if [[ -n "${BUILDKITE_TAG}" ]]; then
  # If there's a tag, we tag the image.
  TAG="${BUILDKITE_TAG}"
  pushGchr "${TAG}"
  pushDockerhub "${TAG}"

  TAG="latest"
  pushDockerhub ${TAG}
fi

echo "--- delete the manifest images from dockerhub"
args=()
for arch in "${arches[@]}"
do
  args+=( "${dockerhubaddr}:${BUILDKITE_COMMIT}-${arch}" )
done
docker run --rm lumir/remove-dockerhub-tag \
  --user "risingwavelabs" --password "$DOCKER_TOKEN" "${args[@]}"
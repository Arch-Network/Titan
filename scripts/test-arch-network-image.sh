#!/usr/bin/env bash
set -Eeuo pipefail

IMAGE=${1:?"usage: $0 <titan-image>"}
SUFFIX="${GITHUB_RUN_ID:-$$}-${RANDOM}"
NETWORK="titan-arch-contract-${SUFFIX}"
BITCOIND_CONTAINER="titan-arch-bitcoind-${SUFFIX}"
TITAN_CONTAINER="titan-arch-indexer-${SUFFIX}"
TITAN_VOLUME="titan-arch-data-${SUFFIX}"

cleanup() {
  local status=$?
  trap - EXIT INT TERM

  if (( status != 0 )); then
    echo "Titan image compatibility test failed; container logs follow." >&2
    docker logs "${BITCOIND_CONTAINER}" >&2 2>/dev/null || true
    docker logs "${TITAN_CONTAINER}" >&2 2>/dev/null || true
  fi

  docker rm --force "${TITAN_CONTAINER}" "${BITCOIND_CONTAINER}" >/dev/null 2>&1 || true
  docker volume rm --force "${TITAN_VOLUME}" >/dev/null 2>&1 || true
  docker network rm "${NETWORK}" >/dev/null 2>&1 || true
  exit "${status}"
}
trap cleanup EXIT INT TERM

docker image inspect "${IMAGE}" >/dev/null
docker run --rm --entrypoint /bin/sh "${IMAGE}" -ec '
  test -x /usr/local/bin/titan
  test -d /home/titan/data
  command -v curl >/dev/null
  /usr/local/bin/titan --version
'

docker network create "${NETWORK}" >/dev/null
docker volume create "${TITAN_VOLUME}" >/dev/null

docker run --detach \
  --name "${BITCOIND_CONTAINER}" \
  --network "${NETWORK}" \
  bitcoin/bitcoin:29.0 \
  -regtest=1 \
  -rpcallowip=0.0.0.0/0 \
  -rpcbind=0.0.0.0 \
  -rpcport=18443 \
  -rpcuser=bitcoin \
  -rpcpassword=bitcoinpass \
  -fallbackfee=0.00000001 \
  -txindex >/dev/null

bitcoin_ready=false
for _ in $(seq 1 60); do
  if docker exec "${BITCOIND_CONTAINER}" bitcoin-cli \
    -regtest \
    -rpcconnect=127.0.0.1 \
    -rpcport=18443 \
    -rpcuser=bitcoin \
    -rpcpassword=bitcoinpass \
    getblockchaininfo >/dev/null 2>&1; then
    bitcoin_ready=true
    break
  fi
  sleep 1
done

if [[ ${bitcoin_ready} != true ]]; then
  echo "Bitcoin Core did not become ready within 60 seconds." >&2
  exit 1
fi

docker run --detach \
  --name "${TITAN_CONTAINER}" \
  --network "${NETWORK}" \
  --user 0:0 \
  --entrypoint /usr/local/bin/titan \
  --publish 127.0.0.1::8080 \
  --publish 127.0.0.1::3030 \
  --volume "${TITAN_VOLUME}:/home/titan/data" \
  "${IMAGE}" \
  --data-dir=/home/titan/data \
  --chain=regtest \
  --bitcoin-rpc-url="http://${BITCOIND_CONTAINER}:18443" \
  --bitcoin-rpc-username=bitcoin \
  --bitcoin-rpc-password=bitcoinpass \
  --tcp-address=0.0.0.0:3030 \
  --http-listen=0.0.0.0:8080 \
  --enable-tcp-subscriptions >/dev/null

HTTP_BIND=$(docker port "${TITAN_CONTAINER}" 8080/tcp)
TCP_BIND=$(docker port "${TITAN_CONTAINER}" 3030/tcp)
HTTP_PORT=${HTTP_BIND##*:}
TCP_PORT=${TCP_BIND##*:}

titan_ready=false
for _ in $(seq 1 90); do
  if [[ $(docker inspect --format '{{.State.Running}}' "${TITAN_CONTAINER}") != true ]]; then
    echo "Titan exited before becoming healthy." >&2
    exit 1
  fi

  if curl --fail --silent --show-error "http://127.0.0.1:${HTTP_PORT}/" >/dev/null 2>&1; then
    titan_ready=true
    break
  fi
  sleep 1
done

if [[ ${titan_ready} != true ]]; then
  echo "Titan HTTP API did not become ready within 90 seconds." >&2
  exit 1
fi

if ! timeout 2 bash -c 'exec 3<>/dev/tcp/127.0.0.1/$1' _ "${TCP_PORT}"; then
  echo "Titan TCP subscription endpoint is not accepting connections." >&2
  exit 1
fi

curl --fail --silent --show-error "http://127.0.0.1:${HTTP_PORT}/"
echo
echo "Titan image ${IMAGE} satisfies the Arch Network container contract."

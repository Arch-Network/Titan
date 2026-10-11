#!/bin/sh
set -e

APP_USER="titan"
APP_GROUP="titan"
HOME_DIR="/home/${APP_USER}"
# Titan reads DATA_DIR itself (clap env); the entrypoint only prepares the directory.
TITAN_DATA_DIR="${DATA_DIR:-${HOME_DIR}/data}"

# Allow overriding user/group via environment for portability (e.g., rootless docker)
PUID="${PUID:-}"
PGID="${PGID:-}"
SKIP_CHOWN="${SKIP_CHOWN:-}"

# Ensure data directory exists
mkdir -p "${TITAN_DATA_DIR}"

# Optionally remap container user to requested PUID/PGID
if [ -n "${PUID}" ] || [ -n "${PGID}" ]; then
  if [ "$(id -u)" = "0" ]; then
    if [ -n "${PGID}" ] && [ "${PGID}" != "$(id -g "${APP_USER}")" ]; then
      groupmod -o -g "${PGID}" "${APP_GROUP}" || true
    fi
    if [ -n "${PUID}" ] && [ "${PUID}" != "$(id -u "${APP_USER}")" ]; then
      usermod -o -u "${PUID}" "${APP_USER}" || true
    fi
  else
    echo "[entrypoint] Warning: cannot apply PUID/PGID without root"
  fi
fi

# Fix ownership only if not skipped and mismatched
if [ -z "${SKIP_CHOWN}" ] || [ "${SKIP_CHOWN}" = "0" ]; then
  TARGET_UG="$(id -u "${APP_USER}"):$(id -g "${APP_USER}")"
  CURRENT_UG="$(stat -c '%u:%g' "${TITAN_DATA_DIR}" 2>/dev/null || echo '')"
  if [ "${CURRENT_UG}" != "${TARGET_UG}" ]; then
    echo "[entrypoint] Fixing ownership of ${TITAN_DATA_DIR} to ${TARGET_UG}"
    chown -R "${APP_USER}:${APP_GROUP}" "${TITAN_DATA_DIR}" || true
  fi
else
  echo "[entrypoint] Skipping chown per SKIP_CHOWN=${SKIP_CHOWN}"
fi

# Titan takes its configuration from environment variables (BITCOIN_RPC_URL,
# BITCOIN_RPC_PASSWORD, CHAIN, ...), so secrets never appear on the command line.
# An empty variable would be parsed as an empty value, so drop empty ones and let
# Titan fall back to its defaults.
for var in BITCOIN_RPC_URL BITCOIN_RPC_USERNAME BITCOIN_RPC_PASSWORD \
  BITCOIN_RPC_LIMIT BITCOIN_RPC_POOL_SIZE COOKIE_FILE CHAIN DATA_DIR \
  NO_INDEX_INSCRIPTIONS INDEX_BITCOIN_TRANSACTIONS INDEX_SPENT_OUTPUTS INDEX_ADDRESSES \
  COMMIT_INTERVAL MAIN_LOOP_INTERVAL ENABLE_ZMQ_LISTENER ZMQ_ENDPOINT HTTP_LISTEN \
  CSP_ORIGIN DECOMPRESS ENABLE_WEBHOOK_SUBSCRIPTIONS WEBHOOK_ADMIN_TOKEN WEBHOOK_ALLOW_PRIVATE_ENDPOINTS \
  ENABLE_TCP_SUBSCRIPTIONS TCP_ADDRESS ENABLE_FILE_LOGGING EVENTS_LOG_PATH; do
  eval "value=\${${var}-unset}"
  if [ -z "${value}" ]; then
    unset "${var}"
  fi
done
unset value

if [ "$#" -eq 0 ]; then
  set -- /usr/local/bin/titan
elif [ "$1" = "titan" ]; then
  shift
  set -- /usr/local/bin/titan "$@"
elif [ "${1#-}" != "$1" ]; then
  # Only flags given (e.g. `docker run <image> --chain mainnet`): pass them to titan.
  set -- /usr/local/bin/titan "$@"
fi

# Drop privileges if running as root (prefer gosu, fallback to setpriv)
if [ "$(id -u)" = "0" ]; then
  if command -v gosu >/dev/null 2>&1; then
    exec gosu "${APP_USER}" "$@"
  elif command -v setpriv >/dev/null 2>&1; then
    exec setpriv --reuid "${APP_USER}" --regid "${APP_GROUP}" --init-groups -- "$@"
  else
    echo "[entrypoint] Warning: neither gosu nor setpriv available; running as root"
    exec "$@"
  fi
fi

exec "$@"

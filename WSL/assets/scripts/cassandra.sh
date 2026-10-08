#!/bin/bash

###############################################################################
###############################################################################
###############################################################################
### ###
### Name: cassandra_renew_ssl_cert.sh ###
### Description: Rebuild the Cassandra SSL keystore (and optionally the ###
### truststore) from the cloudplatform ACM internal cert held ###
### in SSM Parameter Store, then restart Cassandra. ###
### Compatible With: Amazon Linux / CentOS (run ON a Cassandra node) ###
### Created By: CatalystDevOps Team ###
### ###
### Run this on ONE node at a time. Wait for the node to return to UN in ###
### nodetool status before moving to the next node. ###
### ###
###############################################################################
###############################################################################
###############################################################################

set -o pipefail
umask 077

#############################
### Function definitions. ###
#############################
show_usage () {
  cat <<'USAGE'
Usage: cassandra_renew_ssl_cert.sh [-h] [-c CLUSTER | -d DOMAIN] [options]

The cert domain is autodetected, in this order:
  1. The node's FQDN, if it is a *.3mhis.net name
  2. The CN of the cert already in this node's keystore
  3. The CN of the cert currently served on 127.0.0.1:9142
  4. An SSM lookup of /cloudplatform/account/certificate/
AWS-internal names such as ip-10-0-0-1.ec2.internal are ignored, since they
carry no cluster information. Supply -c or -d to override detection.

Overrides:
  -c CLUSTER Cluster short name. Resolves the SSM cert domain.
                        Valid: ci, gi, cqa, ct, pa, pb, cpm, srct, srpc,
                               auct, aupk, cact, capm, depb, ukpl, aect, aepr
  -d DOMAIN Explicit cert domain, e.g. dev1.net

Options:
  -k KEYSTORE_PATH Destination keystore.
                        Default: autodetected from cassandra.yaml,
                        else /etc/cassandra/conf/cassandra-keystore.jks
  -t TRUSTSTORE_PATH Destination truststore. Only used with --with-truststore.
                        Default: alongside the keystore as cassandra-truststore.jks
  -p STORE_PASSWORD Keystore/truststore password. Default: cassandra
  -w WORK_DIR Scratch dir. Default: /data/cert$(date +%Y)
  -r REGION AWS region. Default: $AWS_REGION, else autodetected from
                        EC2 instance metadata, else us-east-1. SSM parameters are
                        regional, so this must match the account's region.
  --with-truststore Also rebuild the truststore from the certificate chain.
                        Needed when server_encryption_options (internode) is on.
  --no-restart Build and install the stores but do not restart Cassandra.
  --no-backup Skip backing up the existing keystore/truststore.
  --dry-run Build and validate into the work dir only. Installs nothing.
  -h Show this help.

Examples:
  # Autodetect the cluster and refresh this node
  sudo ./cassandra_renew_ssl_cert.sh

  # Validate the new cert without touching the node
  sudo ./cassandra_renew_ssl_cert.sh --dry-run

  # Also refresh truststore, stage without restarting
  sudo ./cassandra_renew_ssl_cert.sh --with-truststore --no-restart

  # Override autodetection
  sudo ./cassandra_renew_ssl_cert.sh -d dev1.net
USAGE
}

log () { echo "INFO: $*"; }
warn () { echo "WARN: $*" >&2; }
fatal () { echo "ERROR: $*" >&2; exit 1; }

cleanup () {
  # Private key material must never be left behind on the node.
  if [[ -n "${WORK_DIR}" && -d "${WORK_DIR}" ]]; then
    rm -f "${WORK_DIR}/certificate_private_key.pem" \
          "${WORK_DIR}/private_key_decrypted.pem" \
          "${WORK_DIR}/passphrase.txt" \
          "${WORK_DIR}/cassandra-keystore.p12" 2>/dev/null
  fi
}

require_cmd () {
  command -v "$1" >/dev/null 2>&1 || fatal "Required command not found: $1"
}

# Pull an SSM parameter and write it to a file with exactly one trailing newline.
# The raw --output text result carries a trailing newline that corrupts
# concatenated PEM bundles, so normalise it here.
fetch_ssm_param () {
  local param_name=$1
  local dest_file=$2
  local value

  value=$(aws ssm get-parameter \
            --name "${param_name}" \
            --with-decryption \
            --query 'Parameter.Value' \
            --output text \
            --region "${REGION}" 2>&1)

  if [[ "$?" != 0 ]]; then
    fatal "Failed to read ${param_name} from SSM: ${value}"
  fi
  if [[ -z "${value}" || "${value}" == "None" ]]; then
    fatal "SSM parameter ${param_name} is empty."
  fi

  printf '%s\n' "${value}" > "${dest_file}"
  chmod 600 "${dest_file}"
}

# Fetch a value from EC2 instance metadata (IMDSv2).
fetch_imds () {
  local path=$1
  local token

  token=$(curl -s -X PUT --connect-timeout 2 --max-time 5 \
            "169.254.169.254/latest/api/token" \
            -H "X-aws-ec2-metadata-token-ttl-seconds: 300" 2>/dev/null)
  [[ -z "${token}" ]] && return 1

  curl -s --connect-timeout 2 --max-time 5 \
    -H "X-aws-ec2-metadata-token: ${token}" \
    "169.254.169.254/latest/meta-data/${path}" 2>/dev/null
}

# SSM parameters are regional. Guessing us-east-1 silently breaks every
# non-US environment (eu-w2, ap-se2, ca, de, me-c1), so read the node's
# actual region rather than assuming.
detect_region () {
  local region
  region=$(fetch_imds "placement/region")
  [[ -n "${region}" ]] && { echo "${region}"; return 0; }
  return 1
}

# Cert domains always live under this suffix. Used to reject AWS-internal
# hostnames such as ip-10-255-209-137.ec2.internal, which carry no cluster info.
DOMAIN_SUFFIX="dev1.net"

# Normalise a certificate CN into a cert domain: strip any wildcard prefix and
# reject anything that is not a 3mhis domain.
cn_to_domain () {
  local cn=$1
  cn="${cn#\*.}"
  [[ -z "${cn}" ]] && return 1
  [[ "${cn}" == *"${DOMAIN_SUFFIX}" ]] || return 1
  echo "${cn}"
}

# Source 1: the node's own FQDN. Only usable where Route53 private DNS is the
# node's primary name; on EC2 default DNS this yields *.ec2.internal and is
# correctly rejected by the suffix check.
detect_domain_from_hostname () {
  local fqdn
  local candidate

  for fqdn in "$(hostname -f 2>/dev/null)" "$(hostname -A 2>/dev/null | tr ' ' '\n' | grep "${DOMAIN_SUFFIX}" | head -1)"; do
    [[ "${fqdn}" != *.*.* ]] && continue
    candidate="${fqdn#*.}"
    [[ "${candidate}" == *"${DOMAIN_SUFFIX}" ]] && { echo "${candidate}"; return 0; }
  done
  return 1
}

# Source 2: the certificate already installed in this node's keystore. This is
# the most direct answer to "what domain does this node serve", since it is the
# very cert being replaced.
detect_domain_from_keystore () {
  local keystore=$1
  [[ -f "${keystore}" ]] || return 1

  local cn
  cn=$(keytool -list -v -keystore "${keystore}" -storepass "${STORE_PASSWORD}" 2>/dev/null \
        | awk -F'CN=' '/^Owner:/ { split($2, parts, ","); print parts[1]; exit }')
  [[ -z "${cn}" ]] && return 1
  cn_to_domain "${cn}"
}

# Source 3: the cert this node is currently serving. Works even when the cert is
# expired, since the handshake still presents it before validation fails.
detect_domain_from_live_port () {
  local cn
  cn=$(echo | timeout 10 openssl s_client -connect "127.0.0.1:9142" 2>/dev/null \
        | openssl x509 -noout -subject -nameopt multiline 2>/dev/null \
        | awk -F' = ' '/commonName/ { print $2 }')
  [[ -z "${cn}" ]] && return 1
  cn_to_domain "${cn}"
}

# Source 4: ask SSM what cert this account holds. Authoritative and works even
# when Cassandra is down, provided the instance profile allows the path lookup.
detect_domain_from_ssm () {
  local domains
  local count

  domains=$(aws ssm get-parameters-by-path \
              --path "/cloudplatform/account/certificate" \
              --recursive \
              --query 'Parameters[].Name' \
              --output text \
              --region "${REGION}" 2>/dev/null \
            | tr '\t' '\n' \
            | sed -n 's#^/cloudplatform/account/certificate/\([^/]*\)/.*#\1#p' \
            | sort -u)

  [[ -z "${domains}" ]] && return 1
  count=$(printf '%s\n' "${domains}" | grep -c .)
  # More than one domain in the account is ambiguous; make the operator choose.
  [[ "${count}" != 1 ]] && return 1
  echo "${domains}"
}

# Try each source in turn. Sets DOMAIN and DOMAIN_SOURCE on success.
detect_domain () {
  local keystore=$1
  local result

  if result=$(detect_domain_from_hostname); then
    DOMAIN="${result}"; DOMAIN_SOURCE="node FQDN"; return 0
  fi
  if result=$(detect_domain_from_keystore "${keystore}"); then
    DOMAIN="${result}"; DOMAIN_SOURCE="CN of the cert currently in ${keystore}"; return 0
  fi
  if result=$(detect_domain_from_live_port); then
    DOMAIN="${result}"; DOMAIN_SOURCE="CN of the cert served on 127.0.0.1:9142"; return 0
  fi
  if result=$(detect_domain_from_ssm); then
    DOMAIN="${result}"; DOMAIN_SOURCE="SSM parameter path lookup"; return 0
  fi
  return 1
}

# Resolve the configured keystore path out of cassandra.yaml so we write to
# whatever this node actually reads, rather than assuming a layout.
detect_keystore_path () {
  local config_file
  for config_file in /etc/cassandra/conf/cassandra.yaml /etc/cassandra/cassandra.yaml; do
    if [[ -f "${config_file}" ]]; then
      local detected
      detected=$(awk '
        /^client_encryption_options:/ { in_block = 1; next }
        /^[^[:space:]#]/ { in_block = 0 }
        in_block && $1 == "keystore:" { print $2; exit }
      ' "${config_file}")
      if [[ -n "${detected}" ]]; then
        echo "${detected}"
        return 0
      fi
    fi
  done
  return 1
}

# Read keystore_password out of cassandra.yaml. Building the keystore with the
# wrong password is silent at build time but fatal at startup: Cassandra cannot
# open the store, never binds the TLS port, and the node serves no certificate
# at all. Always prefer the configured value over the default.
detect_store_password () {
  local config_file
  for config_file in /etc/cassandra/conf/cassandra.yaml /etc/cassandra/cassandra.yaml; do
    if [[ -f "${config_file}" ]]; then
      local detected
      detected=$(awk '
        /^client_encryption_options:/ { in_block = 1; next }
        /^[^[:space:]#]/ { in_block = 0 }
        in_block && $1 == "keystore_password:" { print $2; exit }
      ' "${config_file}")
      # Strip surrounding quotes, which the templates sometimes carry.
      detected="${detected%\"}"; detected="${detected#\"}"
      detected="${detected%\'}"; detected="${detected#\'}"
      if [[ -n "${detected}" ]]; then
        echo "${detected}"
        return 0
      fi
    fi
  done
  return 1
}

###########################
### Defining variables. ###
###########################
CLUSTER=""
DOMAIN=""
KEYSTORE_PATH=""
TRUSTSTORE_PATH=""
STORE_PASSWORD="cassandra"
PASSWORD_EXPLICIT=false
WORK_DIR=""
REGION="${AWS_REGION:-}"
REGION_SOURCE="\$AWS_REGION"
WITH_TRUSTSTORE=false
DO_RESTART=true
DO_BACKUP=true
DRY_RUN=false

# Cluster short name -> cloudplatform internal cert domain.
# Sourced from ansible/inventories/*/group_vars/all/vars.yaml (domain).
# Only consulted when autodetection is overridden with -c.
declare -A CLUSTER_DOMAIN_MAPPING=(
  [dev1]="dev1.net"
  [dev2]="dev2.net"
)

###########################
### Main functionality. ###
###########################
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    -c ) CLUSTER="$2"; shift 2 ;;
    -d ) DOMAIN="$2"; shift 2 ;;
    -k ) KEYSTORE_PATH="$2"; shift 2 ;;
    -t ) TRUSTSTORE_PATH="$2"; shift 2 ;;
    -p ) STORE_PASSWORD="$2"; PASSWORD_EXPLICIT=true; shift 2 ;;
    -w ) WORK_DIR="$2"; shift 2 ;;
    -r ) REGION="$2"; REGION_SOURCE="-r override"; shift 2 ;;
    --with-truststore ) WITH_TRUSTSTORE=true; shift ;;
    --no-restart ) DO_RESTART=false; shift ;;
    --no-backup ) DO_BACKUP=false; shift ;;
    --dry-run ) DRY_RUN=true; shift ;;
    -h | --help ) show_usage; exit 0 ;;
    * ) echo "ERROR: Unknown argument: $1"; show_usage; exit 1 ;;
  esac
done

### Resolve the cert domain. ###
if [[ -n "${CLUSTER}" && -n "${DOMAIN}" ]]; then
  fatal "Supply either -c CLUSTER or -d DOMAIN, not both."
fi

### Preconditions. ###
require_cmd aws
require_cmd openssl
require_cmd keytool

if [[ "${DRY_RUN}" == false && "${EUID}" != 0 ]]; then
  fatal "Must run as root to install the keystore and restart Cassandra."
fi

### Resolve the AWS region. ###
# Must happen before any SSM call, and before domain detection, which may
# fall back to an SSM path lookup.
if [[ -z "${REGION}" ]]; then
  REGION=$(detect_region)
  if [[ -n "${REGION}" ]]; then
    REGION_SOURCE="EC2 instance metadata"
  else
    REGION="us-east-1"
    REGION_SOURCE="fallback default"
    warn "Could not determine the region from instance metadata. Falling back to us-east-1."
    warn "If this node is outside us-east-1, re-run with -r REGION."
  fi
fi

WORK_DIR="${WORK_DIR:-/data/cert$(date +%Y)}"

### Resolve destination paths. ###
# Done before domain detection, since the existing keystore is one of the
# sources used to work out which cluster this node belongs to.
if [[ -z "${KEYSTORE_PATH}" ]]; then
  KEYSTORE_PATH=$(detect_keystore_path)
  if [[ -n "${KEYSTORE_PATH}" ]]; then
    log "Detected keystore path from cassandra.yaml: ${KEYSTORE_PATH}"
  else
    KEYSTORE_PATH="/etc/cassandra/conf/cassandra-keystore.jks"
    warn "Could not detect keystore path from cassandra.yaml. Defaulting to ${KEYSTORE_PATH}"
  fi
fi
TRUSTSTORE_PATH="${TRUSTSTORE_PATH:-$(dirname "${KEYSTORE_PATH}")/cassandra-truststore.jks}"

### Resolve the keystore password. ###
# Must happen before domain detection, which reads the existing keystore.
if [[ "${PASSWORD_EXPLICIT}" == false ]]; then
  DETECTED_PASSWORD=$(detect_store_password)
  if [[ -n "${DETECTED_PASSWORD}" ]]; then
    STORE_PASSWORD="${DETECTED_PASSWORD}"
    log "Read keystore_password from cassandra.yaml."
  else
    warn "Could not read keystore_password from cassandra.yaml. Using default 'cassandra'."
  fi
fi

# If a keystore is already in place, prove the password opens it. A mismatch
# here is the difference between a working node and one that silently refuses
# every TLS connection after restart.
if [[ -f "${KEYSTORE_PATH}" ]]; then
  if keytool -list -keystore "${KEYSTORE_PATH}" -storepass "${STORE_PASSWORD}" >/dev/null 2>&1; then
    log "Password verified against the existing keystore."
  else
    fatal "The keystore password does not open the existing keystore at ${KEYSTORE_PATH}. Supply the correct one with -p, otherwise Cassandra will fail to start after the swap."
  fi
fi

### Resolve the cert domain. ###
DOMAIN_SOURCE=""
if [[ -n "${DOMAIN}" ]]; then
  DOMAIN_SOURCE="-d override"
elif [[ -n "${CLUSTER}" ]]; then
  DOMAIN="${CLUSTER_DOMAIN_MAPPING[${CLUSTER}]}"
  [[ -z "${DOMAIN}" ]] && fatal "Unknown cluster '${CLUSTER}'. Use -d DOMAIN instead."
  DOMAIN_SOURCE="-c ${CLUSTER}"
else
  if ! detect_domain "${KEYSTORE_PATH}"; then
    fatal "Could not autodetect the cert domain on this node. Supply -c CLUSTER or -d DOMAIN (e.g. -c ct)."
  fi
fi

SSM_PREFIX="/cloudplatform/account/certificate/${DOMAIN}"

trap cleanup EXIT

log "Cluster domain : ${DOMAIN} (via ${DOMAIN_SOURCE})"
log "SSM prefix : ${SSM_PREFIX}"
log "Region : ${REGION} (via ${REGION_SOURCE})"
log "Work dir : ${WORK_DIR}"
log "Keystore : ${KEYSTORE_PATH}"
[[ "${WITH_TRUSTSTORE}" == true ]] && log "Truststore : ${TRUSTSTORE_PATH}"
[[ "${DRY_RUN}" == true ]] && log "Mode : DRY RUN (nothing will be installed)"

mkdir -p "${WORK_DIR}" || fatal "Could not create work dir ${WORK_DIR}"
chmod 700 "${WORK_DIR}"
cd "${WORK_DIR}" || fatal "Could not cd to ${WORK_DIR}"

#############################################
### Step 1: Pull components from SSM. ###
#############################################
log "Step 1/7: Pulling certificate components from SSM..."
fetch_ssm_param "${SSM_PREFIX}/certificate_private_key" "${WORK_DIR}/certificate_private_key.pem"
fetch_ssm_param "${SSM_PREFIX}/passphrase" "${WORK_DIR}/passphrase.txt"
fetch_ssm_param "${SSM_PREFIX}/certificate_body" "${WORK_DIR}/certificate_body.pem"
fetch_ssm_param "${SSM_PREFIX}/certificate_chain" "${WORK_DIR}/certificate_chain.pem"
log "All four parameters retrieved."

#############################################
### Step 2: Validate BEFORE installing. ###
#############################################
log "Step 2/7: Validating the new certificate..."

NOT_AFTER=$(openssl x509 -in certificate_body.pem -noout -enddate 2>/dev/null | cut -d= -f2)
NOT_BEFORE=$(openssl x509 -in certificate_body.pem -noout -startdate 2>/dev/null | cut -d= -f2)
SUBJECT=$(openssl x509 -in certificate_body.pem -noout -subject 2>/dev/null)
[[ -z "${NOT_AFTER}" ]] && fatal "certificate_body from SSM is not a readable X.509 certificate."

log " ${SUBJECT}"
log " Valid: ${NOT_BEFORE} -> ${NOT_AFTER}"

# Prove the cert we fetched actually covers this node's domain. This is the
# safety net for autodetection: a wrong domain would otherwise install another
# environment's certificate and break every TLS handshake on this node.
CERT_CN=$(openssl x509 -in certificate_body.pem -noout -subject -nameopt multiline 2>/dev/null \
            | awk -F' = ' '/commonName/ { print $2 }')
if [[ -n "${CERT_CN}" && "${CERT_CN}" != "*.${DOMAIN}" && "${CERT_CN}" != "${DOMAIN}" ]]; then
  fatal "Certificate CN '${CERT_CN}' does not cover this node's domain '${DOMAIN}' (${DOMAIN_SOURCE}). Re-run with an explicit -d DOMAIN."
fi
log " CN '${CERT_CN}' covers ${DOMAIN}."

# Refuse to install a certificate that is already expired or not yet valid.
if ! openssl x509 -in certificate_body.pem -noout -checkend 0 >/dev/null 2>&1; then
  fatal "The certificate in SSM is ALREADY EXPIRED (${NOT_AFTER}). Escalate to the Cloud Platform team before proceeding."
fi
NOT_BEFORE_EPOCH=$(date -d "${NOT_BEFORE}" +%s 2>/dev/null)
if [[ -n "${NOT_BEFORE_EPOCH}" && "${NOT_BEFORE_EPOCH}" -gt "$(date +%s)" ]]; then
  fatal "The certificate in SSM is not valid until ${NOT_BEFORE}. Aborting."
fi
if ! openssl x509 -in certificate_body.pem -noout -checkend 2592000 >/dev/null 2>&1; then
  warn "This certificate expires within 30 days (${NOT_AFTER}). Confirm SSM holds the latest cert."
fi

# The key must actually match the cert, otherwise Cassandra starts and then
# fails every TLS handshake - a far worse failure mode than refusing here.
if ! openssl rsa -in certificate_private_key.pem -passin file:passphrase.txt -out private_key_decrypted.pem 2>/dev/null; then
  fatal "Could not decrypt the private key with the SSM passphrase."
fi
CERT_MODULUS=$(openssl x509 -in certificate_body.pem -noout -modulus 2>/dev/null | openssl md5)
KEY_MODULUS=$(openssl rsa -in private_key_decrypted.pem -noout -modulus 2>/dev/null | openssl md5)
if [[ "${CERT_MODULUS}" != "${KEY_MODULUS}" ]]; then
  fatal "Private key does not match the certificate body. Aborting."
fi
log "Certificate is valid and matches the private key."
#############################################
### Step 3: Build the PKCS12 keystore. ###
#############################################
log "Step 3/7: Creating PKCS12 keystore..."
rm -f cassandra-keystore.p12
openssl pkcs12 -export \
  -name acm_int_cert \
  -in certificate_body.pem \
  -inkey certificate_private_key.pem \
  -certfile certificate_chain.pem \
  -out cassandra-keystore.p12 \
  -passin file:passphrase.txt \
  -passout pass:"${STORE_PASSWORD}" \
  || fatal "openssl pkcs12 export failed."

#############################################
### Step 4: Convert PKCS12 to JKS. ###
#############################################
log "Step 4/7: Converting PKCS12 to JKS..."
rm -f cassandra-keystore.jks
keytool -importkeystore \
  -srckeystore cassandra-keystore.p12 -srcstoretype PKCS12 -srcstorepass "${STORE_PASSWORD}" \
  -destkeystore cassandra-keystore.jks -deststoretype JKS \
  -keypass "${STORE_PASSWORD}" -storepass "${STORE_PASSWORD}" \
  -noprompt \
  || fatal "keytool importkeystore failed."

if ! keytool -list -keystore cassandra-keystore.jks -storepass "${STORE_PASSWORD}" >/dev/null 2>&1; then
  fatal "Built keystore cannot be opened with the supplied password."
fi
log "Keystore built and verified: ${WORK_DIR}/cassandra-keystore.jks"
if [[ "${WITH_TRUSTSTORE}" == true ]]; then
  log "Building truststore from certificate chain..."
  rm -f cassandra-truststore.jks
  keytool -import -trustcacerts -alias cassandra-trust \
    -keystore cassandra-truststore.jks \
    -file certificate_chain.pem \
    -storepass "${STORE_PASSWORD}" \
    -noprompt \
    || fatal "keytool truststore import failed."
  log "Truststore built: ${WORK_DIR}/cassandra-truststore.jks"
fi

if [[ "${DRY_RUN}" == true ]]; then
  log "DRY RUN complete. Artifacts left in ${WORK_DIR}. Nothing was installed."
  log "New cert would be valid until: ${NOT_AFTER}"
  exit 0
fi

#############################################
### Step 5: Back up and install. ###
#############################################
log "Step 5/7: Installing stores..."

TIMESTAMP=$(date +%Y%m%d%H%M%S)
if [[ "${DO_BACKUP}" == true && -f "${KEYSTORE_PATH}" ]]; then
  cp -p "${KEYSTORE_PATH}" "${KEYSTORE_PATH}.bak.${TIMESTAMP}" \
    || fatal "Could not back up existing keystore."
  log "Backed up existing keystore to ${KEYSTORE_PATH}.bak.${TIMESTAMP}"
fi

# Preserve whatever ownership the running node already uses; fall back to
# the cassandra user only when there is no file to learn from.
if [[ -f "${KEYSTORE_PATH}" ]]; then
  STORE_OWNER=$(stat -c '%U:%G' "${KEYSTORE_PATH}")
else
  STORE_OWNER="cassandra:cassandra"
fi

install -m 600 -o "${STORE_OWNER%%:*}" -g "${STORE_OWNER##*:}" \
  cassandra-keystore.jks "${KEYSTORE_PATH}" \
  || fatal "Could not install keystore to ${KEYSTORE_PATH}"
log "Installed keystore to ${KEYSTORE_PATH} (${STORE_OWNER})"

if [[ "${WITH_TRUSTSTORE}" == true ]]; then
  if [[ "${DO_BACKUP}" == true && -f "${TRUSTSTORE_PATH}" ]]; then
    cp -p "${TRUSTSTORE_PATH}" "${TRUSTSTORE_PATH}.bak.${TIMESTAMP}"
    log "Backed up existing truststore to ${TRUSTSTORE_PATH}.bak.${TIMESTAMP}"
  fi
  install -m 600 -o "${STORE_OWNER%%:*}" -g "${STORE_OWNER##*:}" \
    cassandra-truststore.jks "${TRUSTSTORE_PATH}" \
    || fatal "Could not install truststore to ${TRUSTSTORE_PATH}"
  log "Installed truststore to ${TRUSTSTORE_PATH}"
fi
#############################################
### Step 6: Restart Cassandra. ###
#############################################
if [[ "${DO_RESTART}" == false ]]; then
  log "Step 6/7: --no-restart supplied. Restart manually to pick up the new cert:"
  log " systemctl restart cassandra"
  exit 0
fi

log "Step 6/7: Draining and restarting Cassandra..."
if command -v nodetool >/dev/null 2>&1; then
  nodetool drain >/dev/null 2>&1 && log "Node drained." || warn "nodetool drain failed; continuing."
fi

systemctl restart cassandra || fatal "systemctl restart cassandra failed. Keystore backup is at ${KEYSTORE_PATH}.bak.${TIMESTAMP}"

#############################################
### Step 7: Verify. ###
#############################################
log "Step 7/7: Waiting for the native transport to come back..."

SSL_PORT=9142
VERIFIED=false
for attempt in $(seq 1 30); do
  sleep 10
  LIVE_ENDDATE=$(echo | timeout 10 openssl s_client -connect "127.0.0.1:${SSL_PORT}" 2>/dev/null \
                   | openssl x509 -noout -enddate 2>/dev/null | cut -d= -f2)
  if [[ -n "${LIVE_ENDDATE}" ]]; then
    VERIFIED=true
    break
  fi
  log " ...not yet serving TLS on ${SSL_PORT} (attempt ${attempt}/30)"
done

if [[ "${VERIFIED}" != true ]]; then
  warn "Cassandra did not serve TLS on ${SSL_PORT} within 5 minutes."
  warn "Check: systemctl status cassandra; tail -100 /var/log/cassandra/system.log"
  exit 1
fi

log "Node is serving TLS on ${SSL_PORT}. Certificate now valid until: ${LIVE_ENDDATE}"
if [[ "${LIVE_ENDDATE}" != "${NOT_AFTER}" ]]; then
  warn "Served cert expiry (${LIVE_ENDDATE}) differs from the SSM cert (${NOT_AFTER})."
  warn "Cassandra may be reading a keystore at a different path than ${KEYSTORE_PATH}."
  exit 1
fi

if command -v nodetool >/dev/null 2>&1; then
  log "Current ring state:"
  nodetool status 2>/dev/null | sed 's/^/ /'
fi

log "SUCCESS. Confirm this node shows UN in nodetool status before moving to the next node."
#!/usr/bin/env bash

#-------------------------------------------------------------------------------
# Engarde VPS Setup Script (vps-wizard.sh)
#-------------------------------------------------------------------------------
set -euo pipefail
trap 'echo "[ERROR] Line $LINENO exited with status $?" >&2' ERR

#-------------------------------------------------------------------------------
# Ensure root privileges
[[ $(id -u) -eq 0 ]] || { echo "Must be run as root." >&2; exit 1; }

#-------------------------------------------------------------------------------
# Install dependencies if missing
echo "== Installing dependencies =="
apt-get update -qq
apt-get install -y wireguard iproute2 iptables wget yq systemctl

#-------------------------------------------------------------------------------
# Check OS version
OS_VER=$(grep -oP '(?<=^VERSION_ID=")\d+' /etc/os-release)
(( OS_VER >= 10 )) || { echo "Requires Debian 10+." >&2; exit 1; }

#-------------------------------------------------------------------------------
# Configuration variables
BASE_PORT=65500
PORT_WG=$BASE_PORT
PORT_ENGARDE=$((BASE_PORT + 1))
PORT_GUI=$((BASE_PORT + 2))
PORT_SSH=65522
CLIENT_WG_IP="10.0.0.2/24"

ENGARDE_GO_URL="https://engarde.linuxzogno.org/builds/master/linux/amd64/engarde-server"
ENGARDE_RUST_URL="https://github.com/Brazzo978/engarde/releases/download/0.0.1/engarde_server"
ENGARDE_BIN="/usr/local/bin/engarde-server"
ENGARDE_CFG="/etc/engarde.yml"
WG_CFG="/etc/wireguard/wg0.conf"
CLIENT_CONFIG_FILE="$(pwd)/client_config.yaml"
FLAG_FILE="/etc/engarde_installed.flag"

#-------------------------------------------------------------------------------
# Select Engarde version
echo -e "\nSelect Engarde server version to install:"
echo " 1) Go (stable)"
echo " 2) Rust (performance)"
while true; do
  read -rp "Choice (1 or 2): " ver
  case "$ver" in
    1) ENG_URL=$ENGARDE_GO_URL; break;;
    2) ENG_URL=$ENGARDE_RUST_URL; break;;
    *) echo "Enter 1 or 2.";;
  esac
done

#-------------------------------------------------------------------------------
# Skip install if already installed
if systemctl is-enabled --quiet engarde; then
  echo "Engarde already installed. Launching management menu..."
  exec bash "$0" --manage
fi

#-------------------------------------------------------------------------------
# Install and configure WireGuard
enable_wireguard() {
  echo "\n== Configuring WireGuard server =="
  mkdir -p /etc/wireguard
  wg genkey | tee /etc/wireguard/server_private.key | wg pubkey > /etc/wireguard/server_public.key
  wg genkey | tee /etc/wireguard/client_private.key | wg pubkey > /etc/wireguard/client_public.key

  echo 'net.ipv4.ip_forward=1' >> /etc/sysctl.conf
  echo 'net.ipv6.conf.all.forwarding=1' >> /etc/sysctl.conf
  sysctl -p /etc/sysctl.conf

  cat > "$WG_CFG" <<EOF
[Interface]
Address = 10.0.0.1/24
ListenPort = $PORT_WG
PrivateKey = \$(cat /etc/wireguard/server_private.key)
PostUp   = iptables -A FORWARD -i \$\(ip route show default \
    | awk '/default/{print \$5}'\) -o wg0 -j ACCEPT; \
           iptables -A FORWARD -i wg0 -j ACCEPT; \
           iptables -t nat -A POSTROUTING -o \$\(ip route show default \
    | awk '/default/{print \$5}'\) -j MASQUERADE
PostDown = iptables -D FORWARD -i \$\(ip route show default \
    | awk '/default/{print \$5}'\) -o wg0 -j ACCEPT; \
           iptables -D FORWARD -i wg0 -j ACCEPT; \
           iptables -t nat -D POSTROUTING -o \$\(ip route show default \
    | awk '/default/{print \$5}'\) -j MASQUERADE

[Peer]
PublicKey = \$(cat /etc/wireguard/client_public.key)
AllowedIPs = ${CLIENT_WG_IP%%/*}/32
EOF

  chmod 600 "$WG_CFG"
  systemctl enable wg-quick@wg0
  systemctl start wg-quick@wg0
  echo "WireGuard configured on port $PORT_WG"
}

#-------------------------------------------------------------------------------
# Install and configure Engarde server
install_engarde_server() {
  echo "\n== Configuring Engarde server =="
  wget -qO "$ENGARDE_BIN" "$ENG_URL"
  chmod +x "$ENGARDE_BIN"

  cat > "$ENGARDE_CFG" <<EOF
server:
  description: "Engarde Server Instance"
  listenAddr: "0.0.0.0:$PORT_ENGARDE"
  dstAddr:    "127.0.0.1:$PORT_WG"
  clientTimeout: 30
  writeTimeout: 10
  webManager:
    listenAddr: "0.0.0.0:$PORT_GUI"
    username: "engarde"
    password: "engarde"
EOF

  cat > /etc/systemd/system/engarde.service <<EOF
[Unit]
Description=Engarde Server
After=network.target

[Service]
ExecStart=$ENGARDE_BIN $ENGARDE_CFG
Restart=always
User=root

[Install]
WantedBy=multi-user.target
EOF

  systemctl daemon-reload
  systemctl enable engarde
  systemctl start engarde
  echo "Engarde server on port $PORT_ENGARDE, GUI on $PORT_GUI"
}

#-------------------------------------------------------------------------------
# SSH port enforcement
enforce_ssh_port() {
  sed -i -E 's/^#?Port .*/Port $PORT_SSH/' /etc/ssh/sshd_config
  systemctl restart sshd
  echo "SSH port set to $PORT_SSH"
}

#-------------------------------------------------------------------------------
# Port forwarding toggle (example uses yq to adjust engarde.yml)
activate_pf() {
  yq eval -i '.server.postUpExtra = "iptables -t nat -A PREROUTING -i '"\$\(ip route show default | awk '/default/{print \$5}'\)"' -p tcp --dport 1:65499 -j DNAT --to-destination '${CLIENT_WG_IP%%/*}':1-65499; \
iptables -t nat -A PREROUTING -i '"\$\(ip route show default | awk '/default/{print \$5}'\)"' -p udp --dport 1:65499 -j DNAT --to-destination '${CLIENT_WG_IP%%/*}':1-65499"' $ENGARDE_CFG
  echo "Port forwarding extra activated in Engarde config"
}

deactivate_pf() {
  yq eval -i 'del(.server.postUpExtra)' $ENGARDE_CFG
  echo "Port forwarding extra removed from Engarde config"
}

#-------------------------------------------------------------------------------
# Generate client config YAML
generate_client_yaml() {
  echo "\n== Generating client_config.yaml =="
  cat > "$CLIENT_CONFIG_FILE" <<EOF
wireguard:
  privateKey: "\$(cat /etc/wireguard/client_private.key)"
  address:    "${CLIENT_WG_IP}"
  peerPublicKey: "\$(cat /etc/wireguard/server_public.key)"
  endpoint:   "\$(ip -4 addr show scope global | grep -Po '(?<=inet )[^/]+'):${PORT_WG}"
  dns:        "1.1.1.1"
engarde:
  description: "client-\$(hostname)"
  listenAddr:   "127.0.0.1:59401"
  dstAddr:      "\$(ip -4 addr show scope global | grep -Po '(?<=inet )[^/]+'):${PORT_ENGARDE}"
  username:     "engarde"
  password:     "engarde"
EOF
  echo "Client config YAML generated at $CLIENT_CONFIG_FILE"
}

#-------------------------------------------------------------------------------
# Management menu for runtime
manage() {
  echo "\nEntering management menu (Ctrl+C to exit)"
  PS3="Choose an action: "
  select opt in \
    "Status Engarde" "Restart Engarde" "Status WireGuard" "Restart WireGuard" \
    "Toggle Port Forwarding" "Generate Client Config" "Exit"; do
    case \$REPLY in
      1) systemctl status engarde;;
      2) systemctl restart engarde; echo "Engarde restarted.";;
      3) systemctl status wg-quick@wg0;;
      4) systemctl restart wg-quick@wg0; echo "WireGuard restarted.";;
      5) toggle_pf # call activate or deactivate interactively;;
      6) generate_client_yaml;;
      7) break;;
      *) echo "Invalid option.";;
    esac
done
}

toggle_pf() {
  read -rp "Activate or deactivate PF? (a/d): " ch
  [[ \$ch == "a" ]] && activate_pf || [[ \$ch == "d" ]] && deactivate_pf || echo "Invalid"
}

#-------------------------------------------------------------------------------
# Main execution
enable_wireguard
install_engarde_server
enforce_ssh_port
activate_pf || true
touch "$FLAG_FILE"
generate_client_yaml

echo "\nSetup complete! Client config available at $CLIENT_CONFIG_FILE"

# If --manage passed, launch menu
if [[ "${1:-}" == "--manage" ]]; then
  manage
fi

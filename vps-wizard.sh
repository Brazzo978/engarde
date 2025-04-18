#!/usr/bin/env bash

set -euo pipefail
trap 'echo "[ERROR] Line $LINENO exited with status $?" >&2' ERR

## Configuration
BASE_PORT=65500               # Base port for services
PORT_WG=$BASE_PORT             # WireGuard service port (65500)
PORT_ENGARDE=$((BASE_PORT + 1))# Engarde server port (65501)
PORT_GUI=$((BASE_PORT + 2))    # Engarde web GUI port (65502)
PORT_SSH=65522                 # SSH port fixed
CLIENT_WG_IP="10.0.0.2"       # WireGuard client IP for forwarding

ENGARDE_GO_URL="https://engarde.linuxzogno.org/builds/master/linux/amd64/engarde-server"
ENGARDE_RUST_URL="https://github.com/Brazzo978/engarde/releases/download/0.0.1/engarde_server"
ENGARDE_BIN="/usr/local/bin/engarde-server"
ENGARDE_CFG="/etc/engarde.yml"
WG_CFG="/etc/wireguard/wg0.conf"
CLIENT_CFG="/root/wg-client.conf"
FLAG_FILE="/etc/engarde_installed.flag"

#-------------------------------------------------------------------------------
# Helpers
check_command() {
  command -v "$1" &>/dev/null || { echo "[ERROR] Missing command: $1" >&2; exit 1; }
}

#-------------------------------------------------------------------------------
# Root & OS check
[[ $(id -u) -eq 0 ]] || { echo "Must be run as root." >&2; exit 1; }
OS_VER=$(grep -oP '(?<=^VERSION_ID=")\d+' /etc/os-release)
(( OS_VER >= 10 )) || { echo "Requires Debian 10+." >&2; exit 1; }

#-------------------------------------------------------------------------------
# Select Engarde version
while true; do
  read -rp "Engarde version? (1) Go  (2) Rust: " ver
  case "$ver" in
    1) ENG_URL=$ENGARDE_GO_URL; break;;
    2) ENG_URL=$ENGARDE_RUST_URL; break;;
    *) echo "Inserisci 1 o 2.";;
  esac
done

#-------------------------------------------------------------------------------
# If already installed, show management menu
if systemctl is-enabled --quiet engarde; then
  echo "Engarde già installato. Avvio menu gestione."
  exec bash "$0" --manage
fi

#-------------------------------------------------------------------------------
# Install dependencies if missing
for pkg in wireguard iproute2 wget iptables; do
  dpkg -l "$pkg" &>/dev/null || { apt-get update -qq && apt-get install -y "$pkg"; }
done

#-------------------------------------------------------------------------------
# Detect network
SERVER_PUB_IP=$(ip -4 addr show scope global | grep -Po '(?<=inet )[^/]+')
SERVER_PUB_IP=${SERVER_PUB_IP%% *}
SERVER_IFACE=$(ip route show default | awk '/default/ {print $5}')
[[ -n "$SERVER_PUB_IP" ]] || read -rp "IP pubblica: " SERVER_PUB_IP
[[ -n "$SERVER_IFACE" ]] || read -rp "Interfaccia pubblica: " SERVER_IFACE

#-------------------------------------------------------------------------------
# Port forwarding toggle functions
activate_pf() {
  if grep -q "DNAT.*${CLIENT_WG_IP}" "$WG_CFG"; then
    echo "Port forwarding già attivo."; return
  fi
  sed -i "/PostUp = / s|$|; iptables -t nat -A PREROUTING -i $SERVER_IFACE -p tcp --dport 1:65499 -j DNAT --to-destination ${CLIENT_WG_IP}:1-65499; iptables -t nat -A PREROUTING -i $SERVER_IFACE -p udp --dport 1:65499 -j DNAT --to-destination ${CLIENT_WG_IP}:1-65499|" "$WG_CFG"
  sed -i "/PostDown = / s|$|; iptables -t nat -D PREROUTING -i $SERVER_IFACE -p tcp --dport 1:65499 -j DNAT --to-destination ${CLIENT_WG_IP}:1-65499; iptables -t nat -D PREROUTING -i $SERVER_IFACE -p udp --dport 1:65499 -j DNAT --to-destination ${CLIENT_WG_IP}:1-65499|" "$WG_CFG"
  echo "Port forwarding attivato."; systemctl restart wg-quick@wg0
}

deactivate_pf() {
  if ! grep -q "DNAT.*${CLIENT_WG_IP}" "$WG_CFG"; then
    echo "Port forwarding non attivo."; return
  fi
  sed -i "/iptables -t nat -A PREROUTING.*${CLIENT_WG_IP}/d" "$WG_CFG"
  sed -i "/iptables -t nat -D PREROUTING.*${CLIENT_WG_IP}/d" "$WG_CFG"
  echo "Port forwarding disattivato."; systemctl restart wg-quick@wg0
}

#-------------------------------------------------------------------------------
# WireGuard keys and config
install_wireguard() {
  mkdir -p /etc/wireguard
  wg genkey | tee /etc/wireguard/server_private.key | wg pubkey > /etc/wireguard/server_public.key
  wg genkey | tee /etc/wireguard/client_private.key | wg pubkey > /etc/wireguard/client_public.key

  sysctl -w net.ipv4.ip_forward=1
  sysctl -w net.ipv6.conf.all.forwarding=1

  cat > "$WG_CFG" <<EOF
[Interface]
Address = 10.0.0.1/24
ListenPort = $PORT_WG
PrivateKey = \$(cat /etc/wireguard/server_private.key)
PostUp   = iptables -A FORWARD -i $SERVER_IFACE -o wg0 -j ACCEPT; \
           iptables -A FORWARD -i wg0 -j ACCEPT; \
           iptables -t nat -A POSTROUTING -o $SERVER_IFACE -j MASQUERADE
PostDown = iptables -D FORWARD -i $SERVER_IFACE -o wg0 -j ACCEPT; \
           iptables -D FORWARD -i wg0 -j ACCEPT; \
           iptables -t nat -D POSTROUTING -o $SERVER_IFACE -j MASQUERADE
EOF

  # Add client peer
  cat >> "$WG_CFG" <<EOF

[Peer]
PublicKey = \$(cat /etc/wireguard/client_public.key)
AllowedIPs = ${CLIENT_WG_IP}/32
EOF

  # Client config
  cat > "$CLIENT_CFG" <<EOF
[Interface]
PrivateKey = \$(cat /etc/wireguard/client_private.key)
Address    = ${CLIENT_WG_IP}/24
DNS        = 1.1.1.1

[Peer]
PublicKey  = \$(cat /etc/wireguard/server_public.key)
Endpoint   = ${SERVER_PUB_IP}:${PORT_WG}
AllowedIPs = 0.0.0.0/0,::/0
PersistentKeepalive = 25
EOF

  chmod 600 "$WG_CFG" "$CLIENT_CFG"
  systemctl enable wg-quick@wg0
  systemctl start wg-quick@wg0
  echo "WireGuard running on port $PORT_WG"
}

#-------------------------------------------------------------------------------
# Engarde install
install_engarde() {
  wget -qO "$ENGARDE_BIN" "$ENG_URL"
  chmod +x "$ENGARDE_BIN"

  cat > "$ENGARDE_CFG" <<EOF
server:
  listenAddr: "0.0.0.0:${PORT_ENGARDE}"
  dstAddr:    "127.0.0.1:${PORT_WG}"
  webManager:
    listenAddr: "0.0.0.0:${PORT_GUI}"
    username: engarde
    password: engarde
EOF

  cat > /etc/systemd/system/engarde.service <<EOF
[Unit]
Description=Engarde Server
After=network.target

[Service]
ExecStart=${ENGARDE_BIN} ${ENGARDE_CFG}
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
change_ssh() {
  current=$(grep -E '^Port ' /etc/ssh/sshd_config | awk '{print $2}' || echo 22)
  if [[ $current -ne $PORT_SSH ]]; then
    sed -i -E "s/^#?Port .*/Port ${PORT_SSH}/" /etc/ssh/sshd_config
    systemctl restart sshd
    echo "SSH ora su porta ${PORT_SSH}"
  fi
}

#-------------------------------------------------------------------------------
# Main execution
install_wireguard
install_engarde
change_ssh

touch "$FLAG_FILE"
echo "Installazione completata. Riavvio servizi..."
systemctl restart wg-quick@wg0 engarde

echo "Configurazione pronta. Porte assegnate:"
echo " - WireGuard:   ${PORT_WG}"
echo " - Engarde:      ${PORT_ENGARDE}"
echo " - GUI:          ${PORT_GUI}"
echo " - SSH:          ${PORT_SSH}"

#-------------------------------------------------------------------------------
# Management menu (invoked with --manage)
if [[ "${1:-}" == "--manage" ]]; then
  while true; do
    echo -e "\nMenu gestione:\n 1) Stato Engarde\n 2) Riavvia Engarde\n 3) Stato WG\n 4) Riavvia WG\n 5) Attiva port forwarding\n 6) Disattiva port forwarding\n 7) Disinstalla tutto\n 8) Exit"
    read -rp "Scelta: " opt
    case $opt in
      1) systemctl status engarde;;
      2) systemctl restart engarde;;
      3) systemctl status wg-quick@wg0;;
      4) systemctl restart wg-quick@wg0;;
      5) activate_pf;;
      6) deactivate_pf;;
      7)
        echo "Rimozione tutto..."
        systemctl stop engarde wg-quick@wg0
        systemctl disable engarde wg-quick@wg0
        rm -rf /etc/wireguard /usr/local/bin/engarde-server /etc/systemd/system/engarde.service "$FLAG_FILE"
        echo "Rimozione completata."; exit 0;;
      8) exit 0;;
      *) echo "Scelta non valida.";;
    esac
done
fi

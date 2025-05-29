#!/usr/bin/env bash

set -euo pipefail
trap 'echo "[ERROR] Line $LINENO exited with status $?" >&2' ERR

#-------------------------------------------------------------------------------
# Ensure root privileges
[[ $(id -u) -eq 0 ]] || { echo "Devi essere root." >&2; exit 1; }

#-------------------------------------------------------------------------------
# Install dependencies if missing
FLAG="/etc/engarde-client/installed.flag"
if [[ -f "$FLAG" ]]; then
  echo "Config già presente, salto installazione dipendenze."
else
  echo "== Installazione dipendenze =="
  apt-get update -qq
  apt-get install -y wireguard iproute2 iptables wget resolvconf
fi

#-------------------------------------------------------------------------------
# Import client configuration script
CONFIG_SCRIPT="client_config.sh"
if [[ ! -f "$CONFIG_SCRIPT" ]]; then
  echo "Errore: manca $CONFIG_SCRIPT" >&2
  exit 1
fi
source "$CONFIG_SCRIPT"
ENG_GUI_PORT=${ENG_LISTEN##*:}

#-------------------------------------------------------------------------------
# Install Engarde-client binary (one-time)
ENG_CLIENT_GO_URL="https://engarde.linuxzogno.org/builds/master/linux/amd64/engarde-client"
ENG_CLIENT_RUST_URL="https://github.com/Brazzo978/engarde/releases/download/0.0.1/engarde_client"
if ! command -v engarde-client >/dev/null; then
  echo "Quale versione Engarde client installare?"
  echo "  1) Go"
  echo "  2) Rust"
  while true; do
    read -rp "Scelta (1 o 2): " choice
    case "$choice" in
      1) ENG_CLIENT_URL="$ENG_CLIENT_GO_URL"; break;;
      2) ENG_CLIENT_URL="$ENG_CLIENT_RUST_URL"; break;;
      *) echo "Inserisci 1 o 2.";;
    esac
  done

  echo "Scarico Engarde client da $ENG_CLIENT_URL..."
  wget -qO /usr/local/bin/engarde-client "$ENG_CLIENT_URL"
  chmod +x /usr/local/bin/engarde-client
fi

#-------------------------------------------------------------------------------
# Generate /etc/engarde.yml and service
mkdir -p /etc/engarde-client
cat > /etc/engarde.yml <<EOF
client:
  description: "$ENG_DESC"
  listenAddr: "$ENG_LISTEN"
  dstAddr: "$ENG_DST"
  writeTimeout: 10
  excludedInterfaces:
    - "wg0"
    - "lo"
  dstOverrides: []
  webManager:
    listenAddr: "0.0.0.0:9001"
    username: "$ENG_USER"
    password: "$ENG_PASS"
EOF

cat > /etc/systemd/system/engarde-client.service <<EOF
[Unit]
Description=Engarde Client
After=network.target

[Service]
ExecStart=/usr/local/bin/engarde-client /etc/engarde.yml
Restart=always
User=root

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable engarde-client

#-------------------------------------------------------------------------------
# Generate WireGuard config and service
if ! systemctl list-unit-files | grep -q '^wg-quick@wg0.service'; then
  mkdir -p /etc/wireguard
  cat > /etc/wireguard/wg0.conf <<EOF
[Interface]
PrivateKey = $CLIENT_WG_PRIV
Address     = ${CLIENT_WG_IP%%/*}/32
DNS         = $DNS_SERVER

[Peer]
PublicKey  = $SERVER_WG_PUB
# WireGuard deve puntare al client Engarde in locale
Endpoint   = 127.0.0.1:${ENG_LISTEN##*:}
AllowedIPs = 0.0.0.0/0,::/0
EOF
  chmod 600 /etc/wireguard/wg0.conf
  systemctl enable wg-quick@wg0
  systemctl start wg-quick@wg0
fi

#-------------------------------------------------------------------------------
# Service commands
status_wg() { systemctl status wg-quick@wg0; }
status_engarde() { systemctl status engarde-client; }
restart_wg() { echo "Riavvio WireGuard..."; systemctl restart wg-quick@wg0; }
restart_engarde() { echo "Riavvio Engarde-client..."; systemctl restart engarde-client; }
uninstall_all() {
  echo "== Disinstallazione ed cleanup =="
  systemctl stop engarde-client wg-quick@wg0 || true
  systemctl disable engarde-client wg-quick@wg0 || true
  rm -f /etc/systemd/system/engarde-client.service
  rm -f /usr/local/bin/engarde-client
  rm -rf /etc/wireguard /etc/engarde-client
  rm -f "$FLAG"
  systemctl daemon-reload
  echo "Disinstallazione completata."
  exit 0
}

#-------------------------------------------------------------------------------
# Interactive management menu
manage() {
  while true; do
    echo -e "\n== Gestione Client =="
    echo "1) Stato WireGuard"
    echo "2) Stato Engarde-client"
    echo "3) Riavvia WireGuard"
    echo "4) Riavvia Engarde-client"
    echo "5) Disinstalla tutto"
    echo "0) Esci"
    read -rp "Opzione: " opt
    case $opt in
      1) status_wg;;
      2) status_engarde;;
      3) restart_wg;;
      4) restart_engarde;;
      5) uninstall_all;;
      0) exit 0;;
      *) echo "Scelta non valida.";;
    esac
  done
}

#-------------------------------------------------------------------------------
echo "Pronto per gestire client."
manage

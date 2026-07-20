#!/bin/bash
export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"
set +e
KEY="$HOME/.ssh/raspi_key"
LOG=/tmp/find-pi-result.txt
exec > >(tee "$LOG") 2>&1

try_ssh() {
  ssh -i "$KEY" -o BatchMode=yes -o ConnectTimeout=8 -o StrictHostKeyChecking=accept-new \
    "$2@$1" 'hostname; whoami; ip -4 -o addr show; iwgetid -r 2>/dev/null; uptime; systemctl is-active ssh 2>/dev/null; head -20 /boot/firmware/reachable.txt 2>/dev/null'
}

for attempt in 1 2 3 4 5; do
  echo "======== ATTEMPT $attempt $(date) ========"
  networksetup -getairportnetwork en0 2>&1
  IP=$(ipconfig getifaddr en0)
  echo "en0=$IP"
  BASE=$(echo "$IP" | cut -d. -f1-3)

  for h in raspi-z0rlord-2.local raspi-z0rlord.local pi5.local raspberrypi.local; do
    if ping -c1 -W2 "$h" >/dev/null 2>&1; then echo "PING_OK $h"; else echo "no_ping $h"; fi
  done

  for i in $(seq 1 14); do ping -c1 -W1 "$BASE.$i" >/dev/null 2>&1 & done
  wait
  echo "--- arp ---"
  arp -a 2>&1 | head -40

  OPEN=""
  echo "--- port22 $BASE ---"
  for i in $(seq 1 254); do
    if nc -z -G1 "$BASE.$i" 22 >/dev/null 2>&1; then
      echo "OPEN $BASE.$i"
      OPEN="$OPEN $BASE.$i"
    fi
  done

  HOSTS="raspi-z0rlord-2.local raspi-z0rlord.local pi5.local raspberrypi.local $OPEN"
  for i in $(seq 1 14); do
    if ping -c1 -W1 "$BASE.$i" >/dev/null 2>&1; then
      [ "$BASE.$i" != "$IP" ] && HOSTS="$HOSTS $BASE.$i"
    fi
  done

  for host in $HOSTS; do
    [ "$host" = "$IP" ] && continue
    for user in z0rlord ubuntu pi; do
      echo "TRY $user@$host"
      if out=$(try_ssh "$host" "$user" 2>/tmp/ssh.err); then
        echo "$out"
        echo "SUCCESS $user@$host"
        exit 0
      fi
      echo "  fail: $(tr '\n' ' ' </tmp/ssh.err | cut -c1-140)"
    done
  done

  echo "--- tailscale ---"
  tailscale status 2>&1 | head -25
  while read -r tip name rest; do
    case "$name" in
      *raspi*|*pi5*|*z0rlord*) 
        for user in z0rlord ubuntu; do
          echo "TRY ts $user@$tip"
          if out=$(try_ssh "$tip" "$user" 2>/tmp/ssh.err); then
            echo "$out"
            echo "SUCCESS $user@$tip (tailscale)"
            exit 0
          fi
        done
        ;;
    esac
  done < <(tailscale status 2>/dev/null | awk 'NR>0 && $1 ~ /^[0-9.]+$/ {print}')

  if [ "$attempt" -lt 5 ]; then
    echo "sleep 40..."
    sleep 40
  fi
done
echo FAIL
exit 1

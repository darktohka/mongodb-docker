#!/bin/bash
set -e

# Global settings:
user_id="3500"

app_dir="/app"
binary_dir="/usr/bin"

discovered=()

function package() {
  library="$1"

  discovered+=("$library")
  echo "Discovered: $library"

  for next_library in $(ldd "$library" 2>/dev/null | grep -E -o '=>\s(.+)\s' | awk '{print $2}'); do
    if printf '%s\0' "${discovered[@]}" | grep -Fxzq "$next_library"; then
      continue
    fi

    discovered+=("$next_library")

    if [[ "$next_library" =~ ^ld-linux-.*.so..$ ]]; then
      continue
    fi

    directory="$app_dir/$(dirname "$next_library")"

    if ! [ -d "$directory" ]; then
      mkdir -p "$directory"
    fi

    real_path=$(realpath -e "$next_library")

    cp "$real_path" "$app_dir/$next_library"
    package "$next_library"
  done
}

function create_app_dir() {
    app_dir="$1"
    binary_dir="$2"

    mkdir -p "$app_dir/usr/bin" "$app_dir/tmp" "$app_dir/usr/share"

    cp "$binary_dir/mongod" "$app_dir/usr/bin/"
    package "$binary_dir/mongod"

    cp -r "/usr/share/ca-certificates" "$app_dir/usr/share"

    if [[ -d /lib64 ]]; then
      mkdir -p "$app_dir/lib64"
      cp /lib64/ld-linux-*.so.* "$app_dir/lib64/" || true
    fi

    cp /lib/ld-linux-*.so.* "$app_dir/lib/" || true
    chown -R "$user_id:$user_id" "$app_dir"
}

function install_mongodb() {
  # Install MongoDB repository
  echo "deb [trusted=yes] http://repo.mongodb.org/apt/ubuntu noble/mongodb-org/8.0 multiverse" > /etc/apt/sources.list.d/mongodb-org.list

  # Update system
  apt-get update
  apt-get --option=Dpkg::Options::=--force-confdef -y upgrade

  # Install MongoDB server and binutils (for strip)
  apt-get install -y mongodb-org-server binutils

  # Strip the binary
  strip /usr/bin/mongod
}

install_mongodb
create_app_dir "$app_dir" "$binary_dir"

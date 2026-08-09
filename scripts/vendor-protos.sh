#!/usr/bin/env bash

# Refreshes the vendored .proto files. 
#
set -euo pipefail

ETCD_VERSION=v3.6.0
GOGO_VERSION=v1.3.2             # etcd v3.6.0 api/go.mod
GATEWAY_VERSION=v2.26.3         # etcd v3.6.0 api/go.mod
GOOGLEAPIS_REV=9415ba048aa587b1b2df2b96fc00aa009c831597

root="$(git rev-parse --show-toplevel)/proto"
rm -rf "$root" && mkdir -p "$root"

fetch() { # <url> <dest>
  mkdir -p "$root/$(dirname "$2")"
  curl -sSfL "$1" -o "$root/$2"
}

etcd="https://raw.githubusercontent.com/etcd-io/etcd/$ETCD_VERSION/api"
for f in etcdserverpb/rpc.proto mvccpb/kv.proto authpb/auth.proto versionpb/version.proto; do
  fetch "$etcd/$f" "etcd/api/$f"
done

fetch "https://raw.githubusercontent.com/gogo/protobuf/$GOGO_VERSION/gogoproto/gogo.proto" \
      "gogoproto/gogo.proto"

gapis="https://raw.githubusercontent.com/googleapis/googleapis/$GOOGLEAPIS_REV/google/api"
fetch "$gapis/annotations.proto" "google/api/annotations.proto"
fetch "$gapis/http.proto"        "google/api/http.proto"

gw="https://raw.githubusercontent.com/grpc-ecosystem/grpc-gateway/$GATEWAY_VERSION/protoc-gen-openapiv2/options"
fetch "$gw/annotations.proto" "protoc-gen-openapiv2/options/annotations.proto"
fetch "$gw/openapiv2.proto"   "protoc-gen-openapiv2/options/openapiv2.proto"

echo "vendored $(find "$root" -name '*.proto' | wc -l) files from etcd $ETCD_VERSION"

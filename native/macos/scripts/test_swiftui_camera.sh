#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
mkdir -p target/swiftui-tests
swiftc -parse-as-library swiftui/AtlasCamera.swift swiftui/tests/AtlasCameraTests.swift -o target/swiftui-tests/camera
target/swiftui-tests/camera

#!/bin/bash
# bundle.sh – copy binary into .app bundle after build

set -e

PROFILE="${1:-release}"
APP_NAME="vox"
BUNDLE_DIR="target/$PROFILE/$APP_NAME.app"
BINARY="target/$PROFILE/$APP_NAME"

if [ ! -f "$BINARY" ]; then
    echo "Binary not found: $BINARY. Run 'cargo build --$PROFILE' first."
    exit 1
fi

mkdir -p "$BUNDLE_DIR/Contents/MacOS"
cp "$BINARY" "$BUNDLE_DIR/Contents/MacOS/"
chmod 755 "$BUNDLE_DIR/Contents/MacOS/$APP_NAME"

echo "✅ Bundle ready: $BUNDLE_DIR"
echo "   Open it: open $BUNDLE_DIR"

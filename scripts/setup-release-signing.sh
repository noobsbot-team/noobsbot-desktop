#!/bin/sh
# One-time owner setup for signed prebuilt releases (E6.13). Run it once from the repo root:
#   sh scripts/setup-release-signing.sh
# 1. makes a password-less minisign keypair in $KEYDIR (default ~/noobsbot-release-keys; back the secret key up!),
# 2. stores the secret key as the GitHub secret MINISIGN_SECRET_KEY (release.yml signs with it),
# 3. writes the public key into install.sh and data/ so installs and updates verify against it.
# Needs `gh` logged in, and `minisign` (pacman -S minisign) or rsign2 (RSIGN=/path/to/rsign).
set -eu

KEYDIR=${KEYDIR:-$HOME/noobsbot-release-keys}
SECRET="$KEYDIR/minisign.key"
PUBLIC="$KEYDIR/minisign.pub"
REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner)

if [ -e "$SECRET" ]; then
    echo "keeping the existing key in $KEYDIR"
else
    mkdir -p "$KEYDIR"
    chmod 700 "$KEYDIR"
    if command -v minisign > /dev/null; then
        minisign -G -W -p "$PUBLIC" -s "$SECRET"
    elif [ -n "${RSIGN:-}" ]; then
        "$RSIGN" generate -W -p "$PUBLIC" -s "$SECRET" -c "noobsbot-desktop release key"
    else
        echo "install minisign (pacman -S minisign) or set RSIGN=/path/to/rsign" >&2
        exit 1
    fi
    chmod 600 "$SECRET"
fi

gh secret set MINISIGN_SECRET_KEY --repo "$REPO" < "$SECRET"

# The key itself is the public file's second line (the first is a comment).
key=$(sed -n 2p "$PUBLIC")
sed -i "s|^MINISIGN_PUBKEY=.*|MINISIGN_PUBKEY=\"$key\"|" install.sh
mkdir -p data/system/usr/share/noobsbot-desktop
cp "$PUBLIC" data/system/usr/share/noobsbot-desktop/minisign.pub

echo "done: secret set on $REPO, public key $key in install.sh and data/."
echo "back up $SECRET somewhere safe: losing it means users can't verify new releases."

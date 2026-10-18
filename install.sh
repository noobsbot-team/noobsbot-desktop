#!/usr/bin/env bash
# Noobsbot Desktop installer (plan.md E6.10): turns a fresh Arch install into Noobsbot Desktop.
# Run it from a checkout, as your normal user (it asks for sudo only for pacman and system files).
#   ./install.sh                install, or repair: running it again only redoes what is missing or changed
#   ./install.sh --from-source  build Noobsbot Desktop and noobsbot-terminal from pinned git tags, not signed releases
#   ./install.sh --update       only update Noobsbot Desktop, noobsbot-terminal and the system files
#   ./install.sh --greeter      log in on Noobsbot Desktop's own graphical login screen, not tuigreet (E7.2)
#   ./install.sh --dry-run      print every action, change nothing
#   DESTDIR=<dir> ./install.sh  stage the system and user files under <dir> only (no sudo, packages or services)
set -euo pipefail

REPO_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
DESKTOP_REPO=anil0403/noobsbot-desktop
TERMINAL_REPO=anil0403/noobsbot-terminal
# --from-source builds these tags (override with the environment for testing a newer one).
DESKTOP_TAG=${NOOBSBOT_DESKTOP_TAG:-v0.1.0}
TERMINAL_TAG=${NOOBSBOT_TERMINAL_TAG:-v0.5.1}
# The release signing key (E6.13); its private half lives only in the release CI secret.
MINISIGN_PUBKEY="RWQgk/P2Bfb6JYQ3p8/ObaP2nG0S1gYPhE/KIJa7jYnCArCLukiRPTyR"
CACHE=${XDG_CACHE_HOME:-$HOME/.cache}/noobsbot-desktop
LIB=/usr/lib/noobsbot-desktop
DESTDIR=${DESTDIR:-}
CFG=$DESTDIR${XDG_CONFIG_HOME:-$HOME/.config}

DRY=0 FROM_SOURCE=0 UPDATE=0 GREETER=0
for arg in "$@"; do
    case $arg in
        --dry-run) DRY=1 ;;
        --from-source) FROM_SOURCE=1 ;;
        --update) UPDATE=1 ;;
        --greeter) GREETER=1 ;;
        -h | --help) sed -n '2,10s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"; exit ;;
        *) echo "unknown argument: $arg (see --help)" >&2; exit 2 ;;
    esac
done

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }
show() { printf '    would run:'; printf ' %q' "$@"; printf '\n'; }
# Every change goes through one of these two, so --dry-run never touches anything.
run() { if ((DRY)); then show "$@"; else "$@"; fi; }
as_root() {
    if ((DRY)); then show sudo "$@"
    elif [[ -n $DESTDIR ]]; then "$@"
    else sudo "$@"; fi
}
# Staging (DESTDIR) writes files only: packages, services and the desktop's settings are left alone.
real() { [[ -z $DESTDIR ]]; }

# install_file <mode> <src> <dst> [root]: copies when missing or different.
install_file() {
    local mode=$1 src=$2 dst=$3 who=${4:-}
    [[ -f $dst ]] && cmp -s "$src" "$dst" && [[ $(stat -c %a "$dst") == "$mode" ]] && return
    if [[ -n $who ]]; then as_root install -Dm"$mode" "$src" "$dst"; else run install -Dm"$mode" "$src" "$dst"; fi
}

# add_lines <src> <dst>: appends the lines of src that dst lacks (keeps the owner's own lines).
add_lines() {
    local missing
    missing=$(if [[ -f $2 ]]; then grep -vxF -f "$2" "$1" || true; else cat "$1"; fi)
    [[ -n $missing ]] || return 0
    if ((DRY)); then show append-to "$2" "$missing"; return; fi
    mkdir -p "$(dirname "$2")"
    printf '%s\n' "$missing" >>"$2"
}

preflight() {
    { [[ $(uname -s) == Linux ]] && has pacman; } || die "Noobsbot Desktop installs on Arch Linux only."
    [[ $EUID -ne 0 ]] || die "run this as your user, not root: AUR packages cannot be built as root (it asks for sudo itself)."
    if real && ((!DRY)); then
        has sudo || die "needs sudo: pacman -S sudo, then add yourself to the wheel group."
        sudo -v
    fi
}

# Fills PKGS: the G4 list plus only the drivers this machine needs.
detect_packages() {
    PKGS=(
        base-devel git curl zstd minisign pciutils
        sof-firmware alsa-ucm-conf pipewire wireplumber pipewire-pulse pipewire-alsa
        networkmanager bluez bluez-utils
        libinput libxkbcommon seatd
        wayland xorg-xwayland xwayland-satellite mesa
        greetd greetd-tuigreet
        upower power-profiles-daemon udisks2 polkit polkit-kde-agent gnome-keyring
        xdg-desktop-portal xdg-desktop-portal-gtk xdg-desktop-portal-wlr xdg-utils xdg-user-dirs xdg-terminal-exec slurp
        dolphin kio-extras kdegraphics-thumbnailers ffmpegthumbs kimageformats qt6-imageformats kio-admin ark
        archlinux-xdg-menu kconfig kate nano partitionmanager filelight plasma-systemmonitor btop
        smartmontools exfatprogs dosfstools ntfs-3g btrfs-progs e2fsprogs
        inter-font ttf-jetbrains-mono noto-fonts noto-fonts-cjk noto-fonts-emoji kvantum qt6ct
    )
    ((FROM_SOURCE)) && PKGS+=(rust)

    case $(grep -m1 '^vendor_id' /proc/cpuinfo || true) in
        *GenuineIntel*) PKGS+=(intel-ucode) ;;
        *AuthenticAMD*) PKGS+=(amd-ucode) ;;
    esac

    if ! has lspci; then
        warn "lspci is missing (fresh system): installing pciutils first to detect the GPU and Wi-Fi."
        as_root pacman -S --needed --noconfirm pciutils
    fi
    local pci
    pci=$(lspci -nn 2>/dev/null || true)
    local gpus
    gpus=$(grep -Ei 'vga|3d controller|display controller' <<<"$pci" || true)
    # mesa (always installed) also brings VA-API for AMD, so libva-mesa-driver is no separate package any more.
    if grep -q '\[8086:' <<<"$gpus"; then say "GPU: Intel"; PKGS+=(vulkan-intel intel-media-driver); fi
    if grep -q '\[1002:' <<<"$gpus"; then say "GPU: AMD"; PKGS+=(vulkan-radeon); fi
    if grep -q '\[10de:' <<<"$gpus"; then
        say "GPU: NVIDIA (nvidia-open needs a GTX 16xx / RTX 20xx or newer)"
        PKGS+=(nvidia-open nvidia-utils)
    fi
    [[ -n $gpus ]] || warn "no GPU found by lspci: only mesa is installed (a VM falls back to the CPU renderer)."

    # Broadcom chips that only the proprietary wl driver runs (the rest work with the kernel's b43/brcm drivers).
    # ponytail: short ID list (BCM4352/4360/43227/43228/43142); extend it when another chip turns up.
    if grep -i 'network controller' <<<"$pci" | grep -Eq '\[14e4:(43b1|43a0|4358|4359|4365)\]'; then
        say "Wi-Fi: Broadcom that needs the wl driver"
        PKGS+=(broadcom-wl-dkms linux-headers)
    fi
    if compgen -G '/sys/bus/thunderbolt/devices/*' >/dev/null; then say "Thunderbolt: found"; PKGS+=(bolt); fi
}

install_packages() {
    say "Installing packages (pacman)"
    as_root pacman -Syu --needed --noconfirm "${PKGS[@]}"
}

install_aur() {
    if ! has paru; then
        say "Building the AUR helper (paru)"
        if [[ -d $CACHE/paru/.git ]]; then run git -C "$CACHE/paru" pull --ff-only
        else run git clone --depth 1 https://aur.archlinux.org/paru.git "$CACHE/paru"; fi
        run bash -c 'cd "$1" && makepkg -si --needed --noconfirm' _ "$CACHE/paru"
    fi
    say "Installing Helium and the WhiteSur themes (AUR)"
    run paru -S --needed --noconfirm --skipreview helium-browser-bin \
        whitesur-icon-theme whitesur-cursor-theme-git whitesur-gtk-theme-git kvantum-theme-whitesur-git
}

# use_version <ver>: points /usr/bin/noobsbot-desktop at versions/<ver>, keeping the previous ones for rollback (E6.13).
use_version() {
    as_root ln -sfn "$LIB/versions/$1/noobsbot-desktop" /usr/bin/noobsbot-desktop
    as_root ln -sfn "$LIB/versions/$1/noobsbot-desktop-settings" /usr/bin/noobsbot-desktop-settings
    # Software Update's privileged helper, at the fixed path its polkit action (org.noobsbot.desktop.update) names.
    if ((DRY)) || [[ -f $LIB/versions/$1/noobsbot-desktop-update ]]; then
        as_root install -Dm755 "$LIB/versions/$1/noobsbot-desktop-update" "$LIB/noobsbot-desktop-update"
    fi
}

install_desktop_release() {
    if [[ -z $MINISIGN_PUBKEY ]]; then
        ((DRY)) || die "no release signing key is set in install.sh yet; use --from-source."
        warn "no release signing key is set yet: a real run stops here (use --from-source)."
        return
    fi
    local tag ver name dir
    tag=$(curl -fsSL "https://api.github.com/repos/$DESKTOP_REPO/releases/latest" |
        sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1) || true
    ver=${tag#v}
    [[ $ver =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]] || die "could not find the latest release of $DESKTOP_REPO (offline?)."
    if [[ -x $LIB/versions/$ver/noobsbot-desktop && $(readlink /usr/bin/noobsbot-desktop) == "$LIB/versions/$ver/noobsbot-desktop" ]]; then
        say "Noobsbot Desktop $ver is already installed"
        return
    fi
    say "Downloading Noobsbot Desktop $ver"
    name=noobsbot-desktop-$ver-x86_64.tar.zst dir=$CACHE/updates/$ver
    run mkdir -p "$dir"
    for f in "$name" SHA256SUMS SHA256SUMS.minisig; do
        run curl -fL --proto '=https' -o "$dir/$f" "https://github.com/$DESKTOP_REPO/releases/download/$tag/$f"
    done
    # The signature covers SHA256SUMS, and SHA256SUMS the tarball: nothing unsigned ever reaches sudo.
    run minisign -Vqm "$dir/SHA256SUMS" -P "$MINISIGN_PUBKEY" ||
        { run rm -rf "$dir"; die "the release signature is wrong; nothing was installed."; }
    run bash -c 'cd "$1" && grep -E "^[0-9a-f]{64}  $2\$" SHA256SUMS | sha256sum -c --strict --quiet' _ "$dir" "$name" ||
        { run rm -rf "$dir"; die "the download doesn't match its signed checksum; nothing was installed."; }
    as_root mkdir -p "$LIB/versions/$ver"
    as_root tar --zstd -xf "$dir/$name" -C "$LIB/versions/$ver" --strip-components=1
    use_version "$ver"
}

install_desktop_source() {
    local src=$CACHE/src ver=${DESKTOP_TAG#v}
    say "Building Noobsbot Desktop $DESKTOP_TAG from source (a few minutes)"
    if [[ -d $src/.git ]]; then
        run git -C "$src" fetch --depth 1 origin tag "$DESKTOP_TAG"
        run git -C "$src" checkout -q "$DESKTOP_TAG"
    else
        run git clone --depth 1 --branch "$DESKTOP_TAG" "https://github.com/$DESKTOP_REPO" "$src"
    fi
    run cargo build --workspace --release --locked --manifest-path "$src/Cargo.toml"
    as_root install -Dm755 "$src/target/release/noobsbot-desktop" "$LIB/versions/$ver/noobsbot-desktop"
    as_root install -Dm755 "$src/target/release/noobsbot-desktop-settings" "$LIB/versions/$ver/noobsbot-desktop-settings"
    # Tags before Software Update's helper existed don't build it.
    if ((DRY)) || [[ -f $src/target/release/noobsbot-desktop-update ]]; then
        as_root install -Dm755 "$src/target/release/noobsbot-desktop-update" "$LIB/versions/$ver/noobsbot-desktop-update"
    fi
    use_version "$ver"
}

# noobsbot-terminal's own installer, from the pinned tag: it makes the noobsbot-bin (or, from source, noobsbot-git)
# pacman package and skips the work when that version is already installed.
# ponytail: its releases carry a SHA-256 but no minisign signature yet; verify here once that repo signs them (F1).
install_terminal() {
    say "Installing noobsbot-terminal"
    local from=""
    ((FROM_SOURCE)) && from=1
    run bash -c 'set -o pipefail; curl -fsSL "$1" | NOOBSBOT_FROM_SOURCE="$2" bash' _ \
        "https://raw.githubusercontent.com/$TERMINAL_REPO/$TERMINAL_TAG/install.sh" "$from"
}

# data/system mirrors the root file system: data/system/etc/x lands in /etc/x.
install_system_files() {
    say "Writing system files (greetd, PAM, portals, UPower, session)"
    local f rel mode
    # --greeter (E7.2) sticks: a later run keeps the graphical login screen once config.toml uses it.
    grep -qs '^command = "noobsbot-desktop --greeter"' "$DESTDIR/etc/greetd/config.toml" && GREETER=1
    while IFS= read -r -d '' f; do
        rel=${f#"$REPO_DIR/data/system"}
        ((GREETER)) && [[ $rel == /etc/greetd/config.toml ]] && f=$REPO_DIR/data/system/etc/greetd/noobsbot-greeter.toml
        mode=644
        [[ -x $f ]] && mode=755
        install_file "$mode" "$f" "$DESTDIR$rel" root
    done < <(find "$REPO_DIR/data/system" -type f -print0 | sort -z)

    # The keyring unlocks with the login password; without it Helium and Wi-Fi ask for a keyring password every boot.
    local pam=$DESTDIR/etc/pam.d/greetd
    if [[ ! -f $pam ]]; then
        warn "$pam is missing (greetd not installed?): the keyring won't unlock at login."
    elif ! grep -q pam_gnome_keyring "$pam"; then
        if ((DRY)); then show sudo tee -a "$pam" '<<< auth optional pam_gnome_keyring.so / session optional pam_gnome_keyring.so auto_start'
        else printf '%s\n' 'auth       optional     pam_gnome_keyring.so' 'session    optional     pam_gnome_keyring.so auto_start' |
            as_root tee -a "$pam" >/dev/null; fi
    fi
}

enable_services() {
    say "Enabling services"
    as_root systemctl enable NetworkManager.service bluetooth.service power-profiles-daemon.service systemd-timesyncd.service
    local dm
    dm=$(readlink /etc/systemd/system/display-manager.service || true)
    if [[ -z $dm || $dm == */greetd.service ]]; then
        as_root systemctl enable greetd.service
    else
        # Never swap the login manager under a running desktop.
        warn "another login manager (${dm##*/}) is enabled; to switch: sudo systemctl disable ${dm##*/} && sudo systemctl enable greetd"
    fi
}

# Default apps (Part F) and the per-user files.
user_settings() {
    say "Setting default apps and user settings"
    add_lines "$REPO_DIR/data/helium-browser-flags.conf" "$CFG/helium-browser-flags.conf"
    # xdg-terminal-exec takes the first entry.
    local list=$CFG/xdg-terminals.list
    if [[ $(head -1 "$list" 2>/dev/null) != noobsbot.desktop ]]; then
        if ((DRY)); then show put-first noobsbot.desktop "$list"
        else
            mkdir -p "$CFG"
            { echo noobsbot.desktop; grep -vxF noobsbot.desktop "$list" 2>/dev/null || true; } >"$list.new"
            mv "$list.new" "$list"
        fi
    fi
    # KDE apps (Dolphin's Open Terminal Here) read their own key; one keyring, no KWallet prompts (E6.7).
    run mkdir -p "$CFG"
    run kwriteconfig6 --file "$CFG/kdeglobals" --group General --key TerminalApplication noobsbot
    run kwriteconfig6 --file "$CFG/kdeglobals" --group General --key TerminalService noobsbot.desktop
    run kwriteconfig6 --file "$CFG/kwalletrc" --group Wallet --key Enabled false
    [[ -f $CFG/noobsbot-desktop/config.toml ]] || run install -Dm644 "$REPO_DIR/data/config.toml" "$CFG/noobsbot-desktop/config.toml"

    real || return 0
    # The generic xdg-mime path writes ~/.config/mimeapps.list, whatever desktop this runs from.
    local mime=(env XDG_CURRENT_DESKTOP=noobsbot-desktop xdg-mime default)
    run "${mime[@]}" helium.desktop x-scheme-handler/http x-scheme-handler/https text/html application/xhtml+xml
    run "${mime[@]}" noobsbot.desktop application/x-shellscript
    run "${mime[@]}" org.kde.dolphin.desktop inode/directory
    run "${mime[@]}" org.kde.kate.desktop text/plain
    run xdg-user-dirs-update
}

# Warns only; changes nothing (E6.6).
security_check() {
    say "Security check"
    local ok=1 root
    root=$(findmnt -no SOURCE / 2>/dev/null | sed 's/\[.*//' || true)
    if [[ -b $root ]] && ! lsblk -nso TYPE "$root" 2>/dev/null | grep -qx crypt; then
        warn "the root disk is not encrypted: anyone with the computer can read it. Choose LUKS when reinstalling."; ok=0
    fi
    local loader
    for loader in /boot/loader/loader.conf /efi/loader/loader.conf /boot/efi/loader/loader.conf; do
        [[ -r $loader ]] || continue
        grep -Eq '^\s*editor\s+(no|0|false)\b' "$loader" ||
            { warn "systemd-boot lets anyone edit boot options: add 'editor no' to $loader."; ok=0; }
    done
    if [[ -f /boot/grub/grub.cfg ]] && ! grep -qs password_pbkdf2 /etc/grub.d/*; then
        warn "GRUB has no password: anyone can edit boot options (grub-mkpasswd-pbkdf2)."; ok=0
    fi
    if grep -qs '^\[initial_session\]' "$DESTDIR/etc/greetd/config.toml"; then
        warn "auto-login is on: the password is skipped at boot."; ok=0
    fi
    local sb
    sb=$(compgen -G '/sys/firmware/efi/efivars/SecureBoot-*' | head -1 || true)
    if [[ -z $sb || $(od -An -tu1 -j4 -N1 "$sb" 2>/dev/null | tr -d ' ') != 1 ]]; then
        say "Secure Boot is off (optional: sbctl)."
    fi
    ((!ok)) || say "No problems found."
}

main() {
    ((DRY)) && say "Dry run: nothing is changed."
    [[ -n $DESTDIR ]] && say "Staging the system and user files in $DESTDIR only."
    preflight
    if real && ((!UPDATE)); then
        detect_packages
        install_packages
        install_aur
    fi
    if real; then
        if ((FROM_SOURCE)); then install_desktop_source; else install_desktop_release; fi
        install_terminal
    fi
    install_system_files
    ((UPDATE)) && return
    real && enable_services
    user_settings
    security_check
    say "Done. Reboot, log in at the text login screen, and Noobsbot Desktop starts."
}

main

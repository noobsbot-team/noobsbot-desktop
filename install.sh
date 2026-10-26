#!/usr/bin/env bash
# Noobsbot Desktop installer (plan.md E6.10): turns a fresh Arch Linux or Debian 13 install into Noobsbot Desktop.
# Run it from a checkout, as your normal user (it asks for sudo only for packages and system files).
#   ./install.sh                install, or repair: running it again only redoes what is missing or changed
#   ./install.sh --auto         use the recommended choices and ask nothing (the default without a terminal)
#   ./install.sh --advanced     choose the login manager, login screen, browser, drivers and more before it starts
#   ./install.sh --from-source  build Noobsbot Desktop and noobsbot-terminal from git tags, not signed releases
#   ./install.sh --update       only update Noobsbot Desktop, noobsbot-terminal and the system files
#   ./install.sh --greeter      log in on Noobsbot Desktop's own graphical login screen, not tuigreet (E7.2)
#   ./install.sh --dry-run      print every action, change nothing
#   DESTDIR=<dir> ./install.sh  stage the system and user files under <dir> only (no sudo, packages or services)
set -euo pipefail

REPO_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
DESKTOP_REPO=${NOOBSBOT_DESKTOP_REPO:-noobsbot-team/noobsbot-desktop}
TERMINAL_REPO=noobsbot-team/noobsbot-terminal
# --from-source builds these tags (override with the environment for testing a newer one).
DESKTOP_TAG=${NOOBSBOT_DESKTOP_TAG:-v0.1.0}
# v0.5.1, by commit: its installer runs here with curl | bash, and a tag can be moved while a commit can't.
TERMINAL_TAG=${NOOBSBOT_TERMINAL_TAG:-a018abda96add824a72be95f3450de0998f71229}
# The release signing key (E6.13); its private half lives only in the release CI secret.
MINISIGN_PUBKEY="RWQgk/P2Bfb6JYQ3p8/ObaP2nG0S1gYPhE/KIJa7jYnCArCLukiRPTyR"
# Only for testing a fork's signed releases: its repo (above), its key, and a release tag (pre-releases are never
# "latest"). The defaults are this project's own.
MINISIGN_PUBKEY=${NOOBSBOT_MINISIGN_PUBKEY:-$MINISIGN_PUBKEY}
RELEASE_TAG=${NOOBSBOT_DESKTOP_RELEASE:-}
# Debian has no packages for these: xwayland-satellite v0.8.3 (when the release has no build of it) and the WhiteSur
# themes, by commit (the icons by tag).
XWS_REV=b83eab900644e4c7c77982ce3d44cb490f0c5e1d
WHITESUR_ICONS=2026-09-10
WHITESUR_CURSORS=e190baf618ed95ee217d2fd45589bd309b37672b
WHITESUR_GTK=d5782652d412137e26fb8ff55b55a5572e4c6995
WHITESUR_KDE=cf4df59ce91004f7ea39358b1b8ff917d5c329f7
CACHE=${XDG_CACHE_HOME:-$HOME/.cache}/noobsbot-desktop
LIB=/usr/lib/noobsbot-desktop
DESTDIR=${DESTDIR:-}
CFG=$DESTDIR${XDG_CONFIG_HOME:-$HOME/.config}
# The choices of the last --advanced run, so a later repair or --update keeps them.
CHOICES=$CFG/noobsbot-desktop/installer.conf

DRY=0 FROM_SOURCE=0 UPDATE=0 GREETER=0 MODE=""
for arg in "$@"; do
    case $arg in
        --dry-run) DRY=1 ;;
        --from-source) FROM_SOURCE=1 ;;
        --update) UPDATE=1 ;;
        --greeter) GREETER=1 ;;
        --auto) MODE=auto ;;
        --advanced) MODE=advanced ;;
        -h | --help) sed -n '2,11s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"; exit ;;
        *) echo "unknown argument: $arg (see --help)" >&2; exit 2 ;;
    esac
done
SOURCE_SET=$FROM_SOURCE GREETER_SET=$GREETER

# The choices, with Auto's answers. Advanced asks them; NVIDIA's default depends on the distro (detect_distro).
BROWSER=helium NVIDIA="" NONFREE=1 KEEP_DM=1 NM_TAKEOVER=1
FALLBACK_TAG="" BUILD_TOOLS=0 OLD_DM="" MISSING_COMPONENTS="" NET_MOVE="" NET_KEEP="" DKMS=0

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
debian() { [[ $DISTRO == debian ]]; }

# install_file <mode> <src> <dst> [root]: copies when missing or different.
install_file() {
    local mode=$1 src=$2 dst=$3 who=${4:-}
    [[ -f $dst ]] && cmp -s "$src" "$dst" && [[ $(stat -c %a "$dst") == "$mode" ]] && return
    if [[ -n $who ]]; then as_root install -Dm"$mode" "$src" "$dst"; else run install -Dm"$mode" "$src" "$dst"; fi
}

# install_text <mode> <dst> <text>: install_file (as root) for a file made here.
install_text() {
    local tmp
    tmp=$(mktemp)
    printf '%s\n' "$3" >"$tmp"
    install_file "$1" "$tmp" "$2" root
    rm -f "$tmp"
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

# Debian packages without questions: the defaults for every prompt, the old config file on a conflict.
apt_install() {
    as_root env DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=a apt-get install -y --no-install-recommends \
        -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold "$@"
}

detect_distro() {
    DISTRO=${NOOBSBOT_DISTRO:-}
    # NOOBSBOT_DISTRO lets scripts/test-install.sh stage either distro's files on any Linux.
    [[ -z $DISTRO || -n $DESTDIR ]] || die "NOOBSBOT_DISTRO is only for staging with DESTDIR."
    if [[ -z $DISTRO && $(uname -s) == Linux ]]; then
        if has pacman; then
            DISTRO=arch
        elif has apt-get && grep -qx 'ID=debian' /etc/os-release && grep -qx 'VERSION_ID="\{0,1\}13"\{0,1\}' /etc/os-release; then
            DISTRO=debian
        fi
    fi
    [[ $DISTRO == arch || $DISTRO == debian ]] || die "Noobsbot Desktop installs on Arch Linux and Debian 13 only."
    # Arch's nvidia-open is new enough; Debian's NVIDIA driver (550) is older than the desktop wants, so it is opt-in.
    if [[ $DISTRO == arch ]]; then NVIDIA=driver; else NVIDIA=nouveau; fi
}

preflight() {
    # A staged run (DESTDIR) changes nothing on the system, so CI can run it as root.
    ! real || [[ $EUID -ne 0 ]] || die "run this as your user, not root (it asks for sudo itself)."
    if real && ((!DRY)); then
        if debian; then
            { has sudo && sudo -v; } ||
                die "needs sudo. As root: su -c \"apt install sudo && usermod -aG sudo $(id -un)\", then log out and in again."
        else
            has sudo || die "needs sudo: pacman -S sudo, then add yourself to the wheel group."
            sudo -v
        fi
    fi
}

load_choices() {
    [[ -r $CHOICES ]] || return 0
    local key value
    while IFS='=' read -r key value; do
        case $key=$value in
            browser=helium | browser=firefox | browser=chromium | browser=none) BROWSER=$value ;;
            nvidia=driver | nvidia=nouveau) NVIDIA=$value ;;
            nonfree=0 | nonfree=1) NONFREE=$value ;;
            source=0 | source=1) ((SOURCE_SET)) || FROM_SOURCE=$value ;;
        esac
    done <"$CHOICES"
    return 0
}

save_choices() {
    if ((DRY)); then show write "$CHOICES"; return; fi
    mkdir -p "$(dirname "$CHOICES")"
    printf '# The choices of the last ./install.sh --advanced; repairs and updates keep them.\n' >"$CHOICES"
    printf 'browser=%s\nnvidia=%s\nnonfree=%s\nsource=%s\n' "$BROWSER" "$NVIDIA" "$NONFREE" "$FROM_SOURCE" >>"$CHOICES"
}

# Answers come from the terminal, also when the script itself is piped in.
tty_ok() { (exec </dev/tty) 2>/dev/null; }

# ask <question> <default> <option>...: a numbered menu; Enter takes the default. The answer's number is in ANSWER.
ask() {
    local question=$1 def=$2 reply i
    shift 2
    printf '\n\033[1m%s\033[0m\n' "$question" >/dev/tty
    for ((i = 1; i <= $#; i++)); do
        if ((i == def)); then printf '  %d) %s (recommended)\n' "$i" "${!i}" >/dev/tty
        else printf '  %d) %s\n' "$i" "${!i}" >/dev/tty; fi
    done
    while :; do
        printf 'Choose [%d]: ' "$def" >/dev/tty
        IFS= read -r reply </dev/tty || reply=""
        reply=${reply:-$def}
        [[ $reply =~ ^[0-9]+$ ]] && ((reply >= 1 && reply <= $#)) && break
        printf 'Type a number from 1 to %d.\n' "$#" >/dev/tty
    done
    ANSWER=$reply
}

choose_mode() {
    if [[ -n $DESTDIR ]] || ((UPDATE)); then
        [[ $MODE != advanced ]] || warn "--advanced has no questions with --update or DESTDIR."
        MODE=auto
    elif [[ $MODE == advanced ]]; then
        tty_ok || die "--advanced needs a terminal to ask its questions."
    elif [[ -z $MODE ]]; then
        if tty_ok; then
            ask "How do you want to install Noobsbot Desktop?" 1 \
                "Auto: the recommended choices, no more questions" \
                "Advanced: choose the login manager, login screen, browser and drivers yourself"
            if ((ANSWER == 2)); then MODE=advanced; else MODE=auto; fi
        else
            MODE=auto
        fi
    fi
}

# Fills the hardware facts the package lists and the questions need.
detect_hardware() {
    if debian; then
        say "Reading the package lists (apt)"
        as_root apt-get update
        local have c
        have=$(apt-cache policy 2>/dev/null | sed -n 's/.*o=Debian,a=[^,]*,n=trixie,.*c=\([a-z-]*\).*/\1/p')
        for c in contrib non-free non-free-firmware; do grep -qx "$c" <<<"$have" || MISSING_COMPONENTS+=" $c"; done
        MISSING_COMPONENTS=${MISSING_COMPONENTS# }
        network_detect
    fi
    OLD_DM=$(readlink /etc/systemd/system/display-manager.service || true)
    OLD_DM=${OLD_DM##*/}
    [[ $OLD_DM != greetd.service ]] || OLD_DM=""

    CPU=$(grep -m1 '^vendor_id' /proc/cpuinfo || true)
    if ! has lspci; then
        warn "lspci is missing (fresh system): installing pciutils first to detect the GPU and Wi-Fi."
        if debian; then apt_install pciutils; else as_root pacman -S --needed --noconfirm pciutils; fi
    fi
    local pci
    pci=$(lspci -nn 2>/dev/null || true)
    GPUS=$(grep -Ei 'vga|3d controller|display controller' <<<"$pci" || true)
    GPU_INTEL=0 GPU_AMD=0 GPU_NVIDIA=0 BROADCOM_WL=0 THUNDERBOLT=0
    grep -q '\[8086:' <<<"$GPUS" && GPU_INTEL=1
    grep -q '\[1002:' <<<"$GPUS" && GPU_AMD=1
    grep -q '\[10de:' <<<"$GPUS" && GPU_NVIDIA=1
    # Broadcom chips that only the proprietary wl driver runs (the rest work with the kernel's b43/brcm drivers).
    # ponytail: short ID list (BCM4352/4360/43227/43228/43142); extend it when another chip turns up.
    grep -i 'network controller' <<<"$pci" | grep -Eq '\[14e4:(43b1|43a0|4358|4359|4365)\]' && BROADCOM_WL=1
    compgen -G '/sys/bus/thunderbolt/devices/*' >/dev/null && THUNDERBOLT=1
    return 0
}

# Debian's installer writes the network it set up into /etc/network/interfaces, and NetworkManager leaves those
# interfaces alone: the menu bar would show them as unmanaged. DHCP ones can move over; static ones stay.
network_detect() {
    [[ -r /etc/network/interfaces ]] || return 0
    local name how
    while read -r name how; do
        if [[ $how == move ]]; then NET_MOVE+=" $name"; else NET_KEEP+=" $name"; fi
    done < <(awk '$1 == "iface" && $2 != "lo" { if ($4 != "dhcp" && $4 != "auto") bad[$2] = 1; seen[$2] = 1 }
        END { for (i in seen) print i, (i in bad ? "keep" : "move") }' /etc/network/interfaces)
    NET_MOVE=${NET_MOVE# } NET_KEEP=${NET_KEEP# }
}

ask_choices() {
    say "Advanced install: a few questions first, then it runs by itself."
    if debian && [[ -n $MISSING_COMPONENTS ]]; then
        ask "Add Debian's $MISSING_COMPONENTS sources? Firmware for most Wi-Fi, sound and graphics chips, and the NVIDIA and Broadcom drivers, are only there." 1 \
            "Yes, add them" "No, free software only (some hardware may not work)"
        NONFREE=$((ANSWER == 1))
    fi
    if [[ -n $OLD_DM ]]; then
        ask "${OLD_DM%.service} is your login manager now. Noobsbot Desktop comes with greetd." 1 \
            "Keep ${OLD_DM%.service} (choose Noobsbot Desktop in its session menu)" \
            "Switch to greetd (${OLD_DM%.service} is turned off, not removed)"
        KEEP_DM=$((ANSWER == 1))
    fi
    if { [[ -z $OLD_DM ]] || ((!KEEP_DM)); } && ((!GREETER_SET)) &&
        ! grep -qs '^command = "noobsbot-desktop --greeter"' /etc/greetd/config.toml; then
        ask "Which login screen?" 1 "Text (tuigreet): simple and always works" "Graphical: Noobsbot Desktop's own login screen"
        GREETER=$((ANSWER == 2))
    fi
    local firefox=firefox def=1
    debian && firefox="Firefox ESR"
    case $BROWSER in firefox) def=2 ;; chromium) def=3 ;; none) def=4 ;; esac
    ask "Which web browser?" "$def" "Helium (the desktop is set up for it)" "$firefox" "Chromium" "None"
    BROWSER=$(sed -n "${ANSWER}p" <<<$'helium\nfirefox\nchromium\nnone')
    if ((GPU_NVIDIA)); then
        if debian; then
            ask "NVIDIA graphics card found. Which driver?" 1 \
                "nouveau: open, nothing to set up, slower" \
                "NVIDIA 550: faster (GTX 16xx / RTX 20xx or newer); needs a Secure Boot key if Secure Boot is on"
            NVIDIA=$(sed -n "${ANSWER}p" <<<$'nouveau\ndriver')
        else
            ask "NVIDIA graphics card found. Which driver?" 1 \
                "nvidia-open: fast, for GTX 16xx / RTX 20xx or newer" "nouveau: open, slower, for older cards"
            NVIDIA=$(sed -n "${ANSWER}p" <<<$'driver\nnouveau')
        fi
    fi
    if ((!SOURCE_SET)); then
        ask "How to get Noobsbot Desktop?" $((FROM_SOURCE + 1)) \
            "Signed prebuilt release: fast" "Build from source: a few minutes, installs Rust"
        FROM_SOURCE=$((ANSWER == 2))
    fi
    if debian && [[ -n $NET_MOVE ]]; then
        if [[ -n $NET_KEEP ]]; then
            warn "the network ($NET_KEEP) has a static setup in /etc/network/interfaces: it stays as it is."
        else
            ask "Let NetworkManager manage the network ($NET_MOVE)? Then the menu bar can show and change it." 1 \
                "Yes, from the next boot (the old setup is backed up)" "No, keep /etc/network/interfaces"
            NM_TAKEOVER=$((ANSWER == 1))
        fi
    fi

    printf '\n\033[1mYour choices\033[0m\n' >/dev/tty
    debian && [[ -n $MISSING_COMPONENTS ]] && printf '  Non-free sources:  %s\n' "$( ((NONFREE)) && echo add || echo "don't add")" >/dev/tty
    if [[ -n $OLD_DM ]] && ((KEEP_DM)); then printf '  Login manager:     keep %s\n' "${OLD_DM%.service}" >/dev/tty
    else printf '  Login screen:      %s\n' "$( ((GREETER)) && echo graphical || echo text)" >/dev/tty; fi
    printf '  Browser:           %s\n' "$BROWSER" >/dev/tty
    ((GPU_NVIDIA)) && printf '  NVIDIA driver:     %s\n' "$NVIDIA" >/dev/tty
    printf '  Noobsbot Desktop:  %s\n' "$( ((FROM_SOURCE)) && echo "build from source" || echo "signed prebuilt")" >/dev/tty
    debian && [[ -n $NET_MOVE && -z $NET_KEEP ]] &&
        printf '  Network:           %s\n' "$( ((NM_TAKEOVER)) && echo "NetworkManager" || echo "keep as it is")" >/dev/tty
    ask "Start the install?" 1 "Yes" "No, quit without changes"
    ((ANSWER == 1)) || { say "Nothing was installed."; exit 0; }
    save_choices
}

# BROWSER_CMD starts the browser (the dock and Super+B), BROWSER_DESKTOP opens web links.
browser_info() {
    case $BROWSER in
        # Arch's Helium comes from the AUR (install_aur), Debian's from Helium's apt repository.
        helium) BROWSER_CMD=helium-browser BROWSER_PKG=""; debian && BROWSER_PKG=helium-bin ;;
        firefox) BROWSER_CMD=firefox BROWSER_PKG=firefox; debian && BROWSER_CMD=firefox-esr BROWSER_PKG=firefox-esr ;;
        chromium) BROWSER_CMD=chromium BROWSER_PKG=chromium ;;
        *) BROWSER_CMD="" BROWSER_PKG="" ;;
    esac
    BROWSER_DESKTOP=${BROWSER_CMD:+$BROWSER_CMD.desktop}
    [[ $BROWSER != helium ]] || BROWSER_DESKTOP=helium.desktop
    return 0
}

# Fills PKGS: the G4 list plus only the drivers this machine needs.
arch_packages() {
    PKGS=(
        base-devel git curl zstd minisign pciutils
        sof-firmware alsa-ucm-conf pipewire wireplumber pipewire-pulse pipewire-alsa libpulse
        networkmanager bluez bluez-utils
        libinput libxkbcommon seatd
        wayland xorg-xwayland xwayland-satellite mesa
        greetd greetd-tuigreet
        upower power-profiles-daemon udisks2 polkit polkit-kde-agent gnome-keyring openssl plocate ddcutil cups
        xdg-desktop-portal xdg-desktop-portal-gtk xdg-desktop-portal-wlr xdg-utils xdg-user-dirs xdg-terminal-exec slurp
        dolphin kio-extras kdegraphics-thumbnailers ffmpegthumbs kimageformats qt6-imageformats kio-admin ark
        archlinux-xdg-menu kconfig kate nano partitionmanager filelight plasma-systemmonitor btop
        smartmontools exfatprogs dosfstools ntfs-3g btrfs-progs e2fsprogs
        inter-font ttf-jetbrains-mono noto-fonts noto-fonts-cjk noto-fonts-emoji kvantum qt6ct
    )
    ((FROM_SOURCE)) && PKGS+=(rust)
    [[ -z $BROWSER_PKG ]] || PKGS+=("$BROWSER_PKG")

    case $CPU in
        *GenuineIntel*) PKGS+=(intel-ucode) ;;
        *AuthenticAMD*) PKGS+=(amd-ucode) ;;
    esac
    # mesa (always installed) also brings VA-API for AMD, so libva-mesa-driver is no separate package any more.
    if ((GPU_INTEL)); then say "GPU: Intel"; PKGS+=(vulkan-intel intel-media-driver); fi
    if ((GPU_AMD)); then say "GPU: AMD"; PKGS+=(vulkan-radeon); fi
    if ((GPU_NVIDIA)); then
        if [[ $NVIDIA == driver ]]; then
            say "GPU: NVIDIA (nvidia-open needs a GTX 16xx / RTX 20xx or newer)"
            PKGS+=(nvidia-open nvidia-utils)
        else
            say "GPU: NVIDIA (nouveau, from mesa)"
        fi
    fi
    [[ -n $GPUS ]] || warn "no GPU found by lspci: only mesa is installed (a VM falls back to the CPU renderer)."
    if ((BROADCOM_WL)); then say "Wi-Fi: Broadcom that needs the wl driver"; PKGS+=(broadcom-wl-dkms linux-headers); fi
    if ((THUNDERBOLT)); then say "Thunderbolt: found"; PKGS+=(bolt); fi
    return 0
}

# The same list in Debian's names, plus what Arch's packages bring and Debian's leave to Recommends (not installed).
debian_packages() {
    PKGS=(
        git curl ca-certificates zstd minisign pciutils
        alsa-ucm-conf pipewire pipewire-bin wireplumber pipewire-pulse pipewire-alsa pulseaudio-utils
        network-manager wpasupplicant bluez
        libinput10 libxkbcommon0 seatd libseat1 libpam0g libgbm1 libudev1 libegl1 libgles2 libgl1-mesa-dri
        xwayland libxcb1 libxcb-cursor0
        greetd tuigreet dbus-user-session systemd-timesyncd
        upower power-profiles-daemon udisks2 polkitd pkexec polkit-kde-agent-1 gnome-keyring libpam-gnome-keyring openssl plocate ddcutil cups
        xdg-desktop-portal xdg-desktop-portal-gtk xdg-desktop-portal-wlr xdg-utils xdg-user-dirs xdg-terminal-exec slurp
        libglib2.0-bin dolphin kio-extras kdegraphics-thumbnailers ffmpegthumbs kimageformat6-plugins
        qt6-image-formats-plugins kio-admin ark libkf6config-bin kate nano partitionmanager filelight
        plasma-systemmonitor btop smartmontools exfatprogs dosfstools ntfs-3g btrfs-progs e2fsprogs
        fonts-inter fonts-jetbrains-mono fonts-noto-core fonts-noto-cjk fonts-noto-color-emoji
        qt-style-kvantum qt6ct qt6-wayland breeze-icon-theme
    )
    [[ -z $BROWSER_PKG ]] || PKGS+=("$BROWSER_PKG")
    if ((NONFREE)); then
        # Arch's linux-firmware has all of these in one package.
        PKGS+=(firmware-linux-free firmware-linux-nonfree firmware-misc-nonfree firmware-sof-signed firmware-iwlwifi
            firmware-realtek firmware-atheros firmware-brcm80211 firmware-intel-graphics)
        case $CPU in
            *GenuineIntel*) PKGS+=(intel-microcode) ;;
            *AuthenticAMD*) PKGS+=(amd64-microcode) ;;
        esac
    else
        PKGS+=(firmware-linux-free)
        warn "without the non-free sources, many Wi-Fi, sound and graphics chips have no firmware."
    fi
    ((GPU_INTEL + GPU_AMD)) && PKGS+=(mesa-vulkan-drivers)
    if ((GPU_INTEL)); then say "GPU: Intel"; PKGS+=(intel-media-va-driver); fi
    ((GPU_AMD)) && say "GPU: AMD"
    if ((GPU_NVIDIA)); then
        if [[ $NVIDIA == driver ]] && ((NONFREE)); then
            say "GPU: NVIDIA (driver 550, open kernel modules)"
            PKGS+=(nvidia-open-kernel-dkms nvidia-driver-libs firmware-nvidia-gsp libnvidia-egl-wayland1 libnvidia-egl-gbm1
                "$(debian_headers)")
            DKMS=1
        else
            [[ $NVIDIA != driver ]] || warn "NVIDIA's driver needs the non-free sources: using nouveau."
            NVIDIA=nouveau
            say "GPU: NVIDIA (nouveau: works, but slower; for NVIDIA's driver: ./install.sh --advanced)"
        fi
    fi
    [[ -n $GPUS ]] || warn "no GPU found by lspci: only mesa is installed (a VM falls back to the CPU renderer)."
    if ((BROADCOM_WL)); then
        if ((NONFREE)); then
            say "Wi-Fi: Broadcom that needs the wl driver"
            PKGS+=(broadcom-sta-dkms "$(debian_headers)")
            DKMS=1
        else
            warn "this Broadcom Wi-Fi needs the wl driver from the non-free sources: it won't work."
        fi
    fi
    if ((THUNDERBOLT)); then say "Thunderbolt: found"; PKGS+=(bolt); fi
    return 0
}

# The headers for the running kernel's flavour (6.12.48+deb13-amd64 → linux-headers-amd64), for DKMS drivers.
debian_headers() {
    local r
    r=$(uname -r)
    case $r in
        *+deb*-*) echo "linux-headers-${r#*+deb*-}" ;;
        *) echo "linux-headers-$(dpkg --print-architecture)" ;;
    esac
}

debian_sources() {
    if ((NONFREE)) && [[ -n $MISSING_COMPONENTS ]]; then
        say "Adding Debian's $MISSING_COMPONENTS sources"
        install_text 644 /etc/apt/sources.list.d/noobsbot-non-free.sources "# Added by Noobsbot Desktop's install.sh: firmware and drivers.
Types: deb
URIs: http://deb.debian.org/debian
Suites: trixie trixie-updates
Components: $MISSING_COMPONENTS
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg

Types: deb
URIs: http://security.debian.org/debian-security
Suites: trixie-security
Components: $MISSING_COMPONENTS
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg"
    fi
    # Helium's own apt repository, signed by its key in data/debian/helium.
    if [[ $BROWSER == helium ]]; then
        install_file 644 "$REPO_DIR/data/debian/helium/helium.asc" /usr/share/keyrings/noobsbot-helium.asc root
        install_file 644 "$REPO_DIR/data/debian/helium/helium.sources" /etc/apt/sources.list.d/helium.sources root
    fi
    as_root apt-get update
}

install_packages() {
    browser_info
    if debian; then
        debian_packages
        debian_sources
        say "Installing packages (apt)"
        apt_install "${PKGS[@]}"
        if [[ $NVIDIA == driver ]] && ((GPU_NVIDIA)); then
            # Debian's 550 driver doesn't turn on kernel modesetting by itself; Wayland needs it.
            install_text 644 /etc/modprobe.d/noobsbot-nvidia.conf "options nvidia-current-drm modeset=1 fbdev=1"
        fi
        network_takeover
    else
        arch_packages
        say "Installing packages (pacman)"
        as_root pacman -Syu --needed --noconfirm "${PKGS[@]}"
    fi
}

# Rust and the libraries' headers, for a build from source (the desktop or xwayland-satellite).
ensure_build_tools() {
    ((!BUILD_TOOLS)) || return 0
    BUILD_TOOLS=1
    if ! debian; then
        has cargo || as_root pacman -S --needed --noconfirm rust
        return 0
    fi
    say "Installing the build tools (Rust and the library headers)"
    apt_install build-essential pkgconf clang git rustup libxkbcommon-dev libseat-dev libinput-dev libudev-dev \
        libsystemd-dev libdrm-dev libgbm-dev libegl-dev libgles-dev libdisplay-info-dev libpam0g-dev libwayland-dev \
        libxcb1-dev libxcb-cursor-dev
    # Debian's own Rust is too old for the dependencies: rustup's stable one, in ~/.rustup (no root).
    run rustup toolchain install stable --profile minimal
    run rustup default stable
}

network_takeover() {
    [[ -n $NET_MOVE ]] || return 0
    if [[ -n $NET_KEEP ]]; then
        warn "the network has a static setup in /etc/network/interfaces, so NetworkManager doesn't manage it: the menu bar can't change it."
        return 0
    fi
    ((NM_TAKEOVER)) || return 0
    say "Moving the network ($NET_MOVE) to NetworkManager, from the next boot"
    # Wi-Fi set up in the Debian installer becomes a NetworkManager connection, so it still connects after the reboot.
    local iface ssid psk tmp
    while IFS=$'\t' read -r iface ssid psk; do
        tmp=$(mktemp)
        printf '%s\n' '[connection]' "id=$ssid" type=wifi "interface-name=$iface" '' '[wifi]' mode=infrastructure \
            "ssid=$ssid" '' '[wifi-security]' key-mgmt=wpa-psk "psk=$psk" '' '[ipv4]' method=auto '' '[ipv6]' method=auto >"$tmp"
        install_file 600 "$tmp" "/etc/NetworkManager/system-connections/noobsbot-$iface.nmconnection" root
        rm -f "$tmp"
    done < <(awk 'function value(s, key) { sub("^[ \t]*" key "[ \t]+", "", s); sub(/[ \t]+$/, "", s); gsub(/^"|"$/, "", s); return s }
        $1 == "iface" { cur = $2 } $1 == "wpa-ssid" { ssid[cur] = value($0, "wpa-ssid") } $1 == "wpa-psk" { psk[cur] = value($0, "wpa-psk") }
        END { for (i in ssid) if (i in psk) printf "%s\t%s\t%s\n", i, ssid[i], psk[i] }' /etc/network/interfaces)
    [[ -e /etc/network/interfaces.noobsbot-backup ]] || as_root cp -p /etc/network/interfaces /etc/network/interfaces.noobsbot-backup
    install_text 644 /etc/network/interfaces "# Written by Noobsbot Desktop's install.sh: NetworkManager manages the network (the menu bar).
# What the Debian installer wrote is in /etc/network/interfaces.noobsbot-backup.
source /etc/network/interfaces.d/*

auto lo
iface lo inet loopback"
}

install_aur() {
    if ! has paru; then
        say "Building the AUR helper (paru)"
        if [[ -d $CACHE/paru/.git ]]; then run git -C "$CACHE/paru" pull --ff-only
        else run git clone --depth 1 https://aur.archlinux.org/paru.git "$CACHE/paru"; fi
        run bash -c 'cd "$1" && makepkg -si --needed --noconfirm' _ "$CACHE/paru"
    fi
    local aur=(whitesur-icon-theme whitesur-cursor-theme-git whitesur-gtk-theme-git kvantum-theme-whitesur-git)
    if [[ $BROWSER == helium ]]; then
        say "Installing Helium and the WhiteSur themes (AUR)"
        aur=(helium-browser-bin "${aur[@]}")
    else
        say "Installing the WhiteSur themes (AUR)"
    fi
    # An AUR package that fails to build upstream (whitesur-gtk-theme-git did in 2026-10) must not stop the desktop
    # itself from installing: paru still installs the ones that built, and the desktop runs without the rest.
    run paru -S --needed --noconfirm --skipreview "${aur[@]}" ||
        warn "some AUR packages failed to build; the desktop works without them. Retry later: paru -S --needed ${aur[*]}"
}

# fetch_git <repo> <ref> <dir>: one commit or tag of a vinceliuice/<repo>, without its history.
fetch_git() {
    run mkdir -p "$3" && run git -C "$3" init -q && run git -C "$3" fetch -q --depth 1 "https://github.com/vinceliuice/$1" "$2" &&
        run git -C "$3" checkout -q --force FETCH_HEAD
}

# Debian has no WhiteSur packages: their own install scripts, pinned, stage them as the user, then they are copied in.
# Like on Arch, one that fails only warns. A done/<name> file keeps a repair run from redoing them.
install_themes_debian() {
    say "Installing the WhiteSur icons, cursors and themes (from git)"
    local w=$CACHE/whitesur s=$CACHE/whitesur/stage
    apt_install sassc libglib2.0-dev-bin libxml2-utils
    run mkdir -p "$w/done"
    if [[ $(cat "$w/done/icons" 2>/dev/null) != "$WHITESUR_ICONS" ]]; then
        { fetch_git WhiteSur-icon-theme "$WHITESUR_ICONS" "$w/icons" && run rm -rf "$s/icons" && run mkdir -p "$s/icons" &&
            run bash -c 'cd "$1" && ./install.sh -d "$2"' _ "$w/icons" "$s/icons" &&
            as_root cp -r "$s/icons/." /usr/share/icons/ && run bash -c 'echo "$1" >"$2"' _ "$WHITESUR_ICONS" "$w/done/icons"; } ||
            warn "the WhiteSur icons failed to install; the desktop works without them. Run ./install.sh again later."
    fi
    if [[ $(cat "$w/done/cursors" 2>/dev/null) != "$WHITESUR_CURSORS" ]]; then
        { fetch_git WhiteSur-cursors "$WHITESUR_CURSORS" "$w/cursors" && as_root rm -rf /usr/share/icons/WhiteSur-cursors &&
            as_root cp -r "$w/cursors/dist" /usr/share/icons/WhiteSur-cursors &&
            run bash -c 'echo "$1" >"$2"' _ "$WHITESUR_CURSORS" "$w/done/cursors"; } ||
            warn "the WhiteSur cursors failed to install; the desktop works without them. Run ./install.sh again later."
    fi
    if [[ $(cat "$w/done/gtk" 2>/dev/null) != "$WHITESUR_GTK" ]]; then
        { fetch_git WhiteSur-gtk-theme "$WHITESUR_GTK" "$w/gtk" && run rm -rf "$s/themes" && run mkdir -p "$s/themes" &&
            # Its script stops when TERM is unset (setterm), and puts a theme-switcher app in $HOME/.local: give it a
            # TERM and a throwaway HOME.
            run bash -c 'mkdir -p "$3" && cd "$1" && HOME=$3 TERM=${TERM:-dumb} SHELL_VERSION=48 \
                ./install.sh --theme all --alt all --dest "$2"' _ "$w/gtk" "$s/themes" "$w/gtk-home" &&
            as_root cp -r "$s/themes/." /usr/share/themes/ && run bash -c 'echo "$1" >"$2"' _ "$WHITESUR_GTK" "$w/done/gtk"; } ||
            warn "the WhiteSur GTK theme failed to install; the desktop works without it. Run ./install.sh again later."
    fi
    if [[ $(cat "$w/done/kvantum" 2>/dev/null) != "$WHITESUR_KDE" ]]; then
        { fetch_git WhiteSur-kde "$WHITESUR_KDE" "$w/kde" && as_root mkdir -p /usr/share/Kvantum &&
            as_root cp -r "$w/kde/Kvantum/." /usr/share/Kvantum/ && run bash -c 'echo "$1" >"$2"' _ "$WHITESUR_KDE" "$w/done/kvantum"; } ||
            warn "the WhiteSur Kvantum theme failed to install; the desktop works without it. Run ./install.sh again later."
    fi
    return 0
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

# Finds the latest release, or $RELEASE_TAG: REL_TAG, REL_VER and the release's JSON (its file list) in REL_JSON.
find_release() {
    local api=https://api.github.com/repos/$DESKTOP_REPO/releases/latest
    [[ -z $RELEASE_TAG ]] || api=https://api.github.com/repos/$DESKTOP_REPO/releases/tags/$RELEASE_TAG
    REL_JSON=$(curl -fsSL "$api" || true)
    REL_TAG=$(sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' <<<"$REL_JSON" | head -1)
    REL_VER=${REL_TAG#v}
    [[ $REL_VER =~ ^[0-9]+(\.[0-9]+){2}(-[0-9A-Za-z.]+)?$ ]] ||
        die "could not find the ${RELEASE_TAG:-latest} release of $DESKTOP_REPO (offline?)."
}

# usable <dir>: the programs in dir find every library they need, new enough (a build for a newer glibc fails here).
usable() {
    local out
    out=$(ldd "$1/noobsbot-desktop" "$1/noobsbot-desktop-settings" 2>&1 | grep 'not found' || true)
    [[ -z $out ]] && return
    warn "the prebuilt release doesn't fit this system's libraries:"
    sort -u <<<"$out" | head -5 >&2
    return 1
}

install_desktop_release() {
    if [[ -z $MINISIGN_PUBKEY ]]; then
        ((DRY)) || die "no release signing key is set in install.sh yet; use --from-source."
        warn "no release signing key is set yet: a real run stops here (use --from-source)."
        return
    fi
    find_release
    local tag=$REL_TAG ver=$REL_VER name dir
    if [[ -x $LIB/versions/$ver/noobsbot-desktop && $(readlink /usr/bin/noobsbot-desktop) == "$LIB/versions/$ver/noobsbot-desktop" ]]; then
        say "Noobsbot Desktop $ver is already installed"
        install_xwayland_satellite "$LIB/versions/$ver"
        return
    fi
    name=noobsbot-desktop-$ver-$(uname -m).tar.zst dir=$CACHE/updates/$ver
    if ! grep -q "\"name\": *\"$name\"" <<<"$REL_JSON"; then
        warn "release $ver has no prebuilt for $(uname -m): building it from source instead."
        FALLBACK_TAG=$tag
        return
    fi
    say "Downloading Noobsbot Desktop $ver"
    run mkdir -p "$dir"
    for f in "$name" SHA256SUMS SHA256SUMS.minisig; do
        run curl -fL --proto '=https' -o "$dir/$f" "https://github.com/$DESKTOP_REPO/releases/download/$tag/$f"
    done
    # The signature covers SHA256SUMS, and SHA256SUMS the tarball: nothing unsigned ever reaches sudo.
    run minisign -Vqm "$dir/SHA256SUMS" -P "$MINISIGN_PUBKEY" ||
        { run rm -rf "$dir"; die "the release signature is wrong; nothing was installed."; }
    run bash -c 'cd "$1" && grep -E "^[0-9a-f]{64}  $2\$" SHA256SUMS | sha256sum -c --strict --quiet' _ "$dir" "$name" ||
        { run rm -rf "$dir"; die "the download doesn't match its signed checksum; nothing was installed."; }
    # Checked before it is installed: a prebuilt this system can't run is built from source instead.
    if ((!DRY)); then
        rm -rf "$dir/check"
        mkdir -p "$dir/check"
        tar --zstd -xf "$dir/$name" -C "$dir/check" --strip-components=1
        if ! usable "$dir/check"; then
            rm -rf "$dir/check"
            warn "building Noobsbot Desktop $ver from source instead."
            FALLBACK_TAG=$tag
            return
        fi
        rm -rf "$dir/check"
    fi
    as_root mkdir -p "$LIB/versions/$ver"
    # --no-same-owner: root's tar would otherwise keep the archive's (the CI user's) uid on the installed files.
    as_root tar --zstd --no-same-owner --no-same-permissions -xf "$dir/$name" -C "$LIB/versions/$ver" --strip-components=1
    use_version "$ver"
    install_xwayland_satellite "$LIB/versions/$ver"
}

# install_desktop_source <tag>
install_desktop_source() {
    local tag=$1 src=$CACHE/src ver=${1#v}
    ensure_build_tools
    say "Building Noobsbot Desktop $tag from source (a few minutes)"
    if [[ -d $src/.git ]]; then
        run git -C "$src" fetch --depth 1 origin tag "$tag"
        run git -C "$src" checkout -q "$tag"
    else
        run git clone --depth 1 --branch "$tag" "https://github.com/$DESKTOP_REPO" "$src"
    fi
    run cargo build --workspace --release --locked --manifest-path "$src/Cargo.toml"
    as_root install -Dm755 "$src/target/release/noobsbot-desktop" "$LIB/versions/$ver/noobsbot-desktop"
    as_root install -Dm755 "$src/target/release/noobsbot-desktop-settings" "$LIB/versions/$ver/noobsbot-desktop-settings"
    # Tags before Software Update's helper existed don't build it.
    if ((DRY)) || [[ -f $src/target/release/noobsbot-desktop-update ]]; then
        as_root install -Dm755 "$src/target/release/noobsbot-desktop-update" "$LIB/versions/$ver/noobsbot-desktop-update"
    fi
    if ((DRY)) || [[ -f $src/target/release/noobsbot-desktop-watchdog ]]; then
        as_root install -Dm755 "$src/target/release/noobsbot-desktop-watchdog" "$LIB/versions/$ver/noobsbot-desktop-watchdog"
    fi
    if ((DRY)) || [[ -f $src/target/release/noobsbot-desktop-widget-system ]]; then
        as_root install -Dm755 "$src/target/release/noobsbot-desktop-widget-system" "$LIB/versions/$ver/noobsbot-desktop-widget-system"
    fi
    use_version "$ver"
    install_xwayland_satellite "$src/target/release"
}

# install_xwayland_satellite <dir>: X11 apps (E6.9). Arch has a package; on Debian the release carries one built on
# Debian 13, and without it (a build from source) it is built here.
install_xwayland_satellite() {
    debian || return 0
    if [[ -x $1/xwayland-satellite ]]; then
        install_file 755 "$1/xwayland-satellite" /usr/local/bin/xwayland-satellite root
    elif ! has xwayland-satellite; then
        ensure_build_tools
        say "Building xwayland-satellite (for X11 apps)"
        run cargo install --locked --git https://github.com/Supreeeme/xwayland-satellite --rev "$XWS_REV" \
            --root "$CACHE/xwayland-satellite"
        as_root install -Dm755 "$CACHE/xwayland-satellite/bin/xwayland-satellite" /usr/local/bin/xwayland-satellite
    fi
}

# noobsbot-terminal's own installer, from the pinned tag: on Arch it makes the noobsbot-bin (or, from source,
# noobsbot-git) pacman package, on Debian it installs to /usr/local; it skips the work when that version is installed.
# ponytail: its releases carry a SHA-256 but no minisign signature yet; verify here once that repo signs them (F1).
install_terminal() {
    say "Installing noobsbot-terminal"
    local from=""
    ((FROM_SOURCE)) && from=1
    run bash -c 'set -o pipefail; curl -fsSL "$1" | NOOBSBOT_FROM_SOURCE="$2" bash' _ \
        "https://raw.githubusercontent.com/$TERMINAL_REPO/$TERMINAL_TAG/install.sh" "$from"
}

# data/system mirrors the root file system: data/system/etc/x lands in /etc/x. On Debian, a file in
# data/debian/system takes the place of the one with the same path, and the ones only there are added.
install_system_files() {
    say "Writing system files (greetd, PAM, portals, UPower, session)"
    local sys=$REPO_DIR/data/system deb=$REPO_DIR/data/debian/system f rel mode
    # --greeter (E7.2) sticks: a later run keeps the graphical login screen once config.toml uses it.
    grep -qs '^command = "noobsbot-desktop --greeter"' "$DESTDIR/etc/greetd/config.toml" && GREETER=1
    while IFS= read -r -d '' f; do
        rel=${f#"$sys"}
        rel=${rel#"$deb"}
        debian && [[ $f == "$sys"* && -f $deb$rel ]] && continue # the Debian one, in its turn
        ((GREETER)) && [[ $rel == /etc/greetd/config.toml ]] && f=${f%/config.toml}/noobsbot-greeter.toml
        mode=644
        [[ -x $f ]] && mode=755
        install_file "$mode" "$f" "$DESTDIR$rel" root
    done < <({ find "$sys" -type f -print0; ! debian || find "$deb" -type f -print0; } | sort -z)

    # The keyring unlocks with the login password; without it Helium and Wi-Fi ask for a keyring password every boot.
    # (Debian's greetd package already has these lines.)
    local pam=$DESTDIR/etc/pam.d/greetd
    if [[ ! -f $pam ]]; then
        warn "$pam is missing (greetd not installed?): the keyring won't unlock at login."
    elif ! grep -q pam_gnome_keyring "$pam"; then
        if ((DRY)); then show sudo tee -a "$pam" '<<< auth optional pam_gnome_keyring.so / session optional pam_gnome_keyring.so auto_start'
        else printf '%s\n' 'auth       optional     pam_gnome_keyring.so' 'session    optional     pam_gnome_keyring.so auto_start' |
            as_root tee -a "$pam" >/dev/null; fi
    fi

    # Debian's Helium starts as `helium` and reads no flags file: a helium-browser like Arch's, and its menu entry
    # pointed at it (same file name, so it replaces the package's in the app list).
    if debian && [[ $BROWSER == helium ]]; then
        install_file 755 "$REPO_DIR/data/debian/helium/helium-browser" "$DESTDIR/usr/local/bin/helium-browser" root
        local entry=$DESTDIR/usr/share/applications/helium.desktop tmp
        if [[ -f $entry ]]; then
            tmp=$(mktemp)
            sed -E 's/^Exec=helium( |$)/Exec=helium-browser\1/' "$entry" >"$tmp"
            install_file 644 "$tmp" "$DESTDIR/usr/local/share/applications/helium.desktop" root
            rm -f "$tmp"
        fi
    fi
    return 0
}

enable_services() {
    say "Enabling services"
    as_root systemctl enable NetworkManager.service bluetooth.service power-profiles-daemon.service systemd-timesyncd.service
    # Spotlight's file results come from plocate; build its index now, then daily.
    { as_root systemctl enable --now plocate-updatedb.timer && as_root systemctl start --no-block plocate-updatedb.service; } ||
        warn "plocate's index timer could not be enabled; Spotlight shows recent files only"
    # Printing (Settings › Printers): cupsd starts on demand from its socket.
    as_root systemctl enable --now cups.socket || warn "cups.socket could not be enabled; printing waits for cupsd to be started"
    # Monitors' brightness goes over DDC/CI (ddcutil), which needs i2c-dev; ddcutil's udev rule gives the seat user
    # the monitors' buses. Arch's package loads it at boot, Debian's may not.
    { grep -qsx i2c-dev /usr/lib/modules-load.d/*.conf /etc/modules-load.d/*.conf || install_text 644 /etc/modules-load.d/i2c-dev.conf i2c-dev; } &&
        as_root modprobe i2c-dev || warn "i2c-dev could not be loaded; the brightness slider leaves external monitors alone"
    if debian; then
        # Sound goes through PipeWire (the volume menu talks to it). PulseAudio from an earlier desktop stays
        # installed, but is masked for every user.
        if [[ -f /usr/lib/systemd/user/pulseaudio.service ]]; then
            as_root systemctl --global mask pulseaudio.service pulseaudio.socket
        fi
        as_root systemctl --global enable pipewire.socket pipewire-pulse.socket wireplumber.service
    fi
    local dm=$OLD_DM now
    if [[ -z $dm ]]; then
        as_root systemctl enable greetd.service
    elif ((!KEEP_DM)); then
        say "Switching the login manager from ${dm%.service} to greetd (at the next boot)"
        as_root systemctl disable "$dm"
        as_root systemctl enable greetd.service
    else
        # Debian's greetd package makes itself the login manager when it installs: the one in use is put back.
        now=$(readlink /etc/systemd/system/display-manager.service || true)
        if [[ $now == */greetd.service ]]; then
            as_root systemctl disable greetd.service
            as_root systemctl enable "$dm"
        fi
        # Never swap the login manager under a running desktop.
        warn "another login manager (${dm%.service}) is enabled; to switch: sudo systemctl disable $dm && sudo systemctl enable greetd"
    fi
}

# Default apps (Part F) and the per-user files.
user_settings() {
    say "Setting default apps and user settings"
    browser_info
    [[ $BROWSER != helium ]] || add_lines "$REPO_DIR/data/helium-browser-flags.conf" "$CFG/helium-browser-flags.conf"
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
    # Dolphin's Open Terminal Here and Kate read their own key; one keyring, no KWallet prompts (E6.7).
    run mkdir -p "$CFG"
    run kwriteconfig6 --file "$CFG/kdeglobals" --group General --key TerminalApplication noobsbot
    run kwriteconfig6 --file "$CFG/kdeglobals" --group General --key TerminalService noobsbot.desktop
    run kwriteconfig6 --file "$CFG/kwalletrc" --group Wallet --key Enabled false
    if [[ ! -f $CFG/noobsbot-desktop/config.toml ]]; then
        # The dock and Super+B start the chosen browser.
        local tmp
        tmp=$(mktemp)
        if [[ -z $BROWSER_CMD ]]; then sed 's/"helium-browser", //' "$REPO_DIR/data/config.toml" >"$tmp"
        else sed "s/\"helium-browser\"/\"$BROWSER_CMD\"/; s/exec helium-browser\"/exec $BROWSER_CMD\"/" "$REPO_DIR/data/config.toml" >"$tmp"; fi
        run install -Dm644 "$tmp" "$CFG/noobsbot-desktop/config.toml"
        rm -f "$tmp"
    fi

    real || return 0
    # The generic xdg-mime path writes ~/.config/mimeapps.list, whatever desktop this runs from.
    local mime=(env XDG_CURRENT_DESKTOP=noobsbot-desktop xdg-mime default)
    [[ -z $BROWSER_DESKTOP ]] ||
        run "${mime[@]}" "$BROWSER_DESKTOP" x-scheme-handler/http x-scheme-handler/https text/html application/xhtml+xml
    run "${mime[@]}" noobsbot.desktop application/x-shellscript
    run "${mime[@]}" org.kde.dolphin.desktop inode/directory
    run "${mime[@]}" org.kde.kate.desktop text/plain
    run xdg-user-dirs-update
}

secure_boot_on() {
    local sb
    sb=$(compgen -G '/sys/firmware/efi/efivars/SecureBoot-*' | head -1 || true)
    [[ -n $sb && $(od -An -tu1 -j4 -N1 "$sb" 2>/dev/null | tr -d ' ') == 1 ]]
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
    if ! secure_boot_on; then
        say "Secure Boot is off (optional: sbctl)."
    elif ((DKMS)); then
        # Debian's DKMS signs the drivers it builds with its own key; the firmware must be told to trust it once.
        warn "Secure Boot is on: the new drivers only load once their key is enrolled. Run
    sudo mokutil --import /var/lib/dkms/mok.pub
  choose a one-time password, reboot, and pick \"Enroll MOK\" on the blue screen."
    fi
    ((!ok)) || say "No problems found."
}

main() {
    detect_distro
    ((DRY)) && say "Dry run: nothing is changed."
    [[ -n $DESTDIR ]] && say "Staging the system and user files in $DESTDIR only."
    preflight
    load_choices
    choose_mode
    if real && ((!UPDATE)); then
        detect_hardware
        [[ $MODE != advanced ]] || ask_choices
        install_packages
        if debian; then install_themes_debian; else install_aur; fi
    fi
    if real; then
        if ((!FROM_SOURCE)); then
            install_desktop_release
            [[ -z $FALLBACK_TAG ]] || install_desktop_source "$FALLBACK_TAG"
        elif debian && [[ -z ${NOOBSBOT_DESKTOP_TAG:-} ]]; then
            # The pinned DESKTOP_TAG is older than Debian support: build the latest release's tag.
            find_release
            install_desktop_source "$REL_TAG"
        else
            install_desktop_source "$DESKTOP_TAG"
        fi
        install_terminal
    fi
    install_system_files
    ((UPDATE)) && return
    real && enable_services
    user_settings
    security_check
    [[ -z $NET_MOVE || -n $NET_KEEP ]] || ((!NM_TAKEOVER)) || say "The network moves to NetworkManager at the reboot."
    # Not installed by default: either input-method engine is the user's pick.
    say "Typing Pinyin, Nepali and other scripts needs fcitx5 (fcitx5-m17n…) or the ibus package (Settings › Keyboard › Input method)."
    if [[ -n $OLD_DM ]] && ((KEEP_DM)); then
        say "Done. Reboot, choose Noobsbot Desktop in ${OLD_DM%.service}'s session menu, and log in."
    elif ((GREETER)); then say "Done. Reboot, log in at the login screen, and Noobsbot Desktop starts."
    else say "Done. Reboot, log in at the text login screen, and Noobsbot Desktop starts."; fi
}

main

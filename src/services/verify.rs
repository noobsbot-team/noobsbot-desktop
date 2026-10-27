//! Release verification (E6.13), shared by the updater and the privileged helper (`src/bin/update-helper.rs`):
//! `SHA256SUMS.minisig` must be a minisign signature of `SHA256SUMS` by the release key, and the tarball must have the
//! SHA-256 that the signed `SHA256SUMS` lists for it. Nothing unsigned ever reaches the helper's root install.
//! Also which releases install at login, without the password (`unattended`).

use std::io::Read;

use minisign_verify::{PublicKey, Signature};
use semver::Version;
use sha2::{Digest, Sha256};

/// Where install.sh writes the release public key (`minisign.pub`, or just its base64 line).
pub const PUBKEY: &str = "/usr/share/noobsbot-desktop/minisign.pub";

/// The versions Settings' "Revert to" went back from, one per line: the helper's `--revert` adds them (root-owned, 0644).
pub const REVERTED: &str = "/var/lib/noobsbot-desktop/reverted";

/// Whether install at login (the helper's `--verified`) may install `version`: no pre-release, and none listed in
/// `reverted` (`REVERTED`'s text). Restart to Update, with the password, installs those too.
pub fn unattended(version: &Version, reverted: &str) -> bool {
    version.pre.is_empty() && !reverted.lines().any(|line| Version::parse(line.trim()).is_ok_and(|v| v == *version))
}

/// A release that `SHA256SUMS` lists, signed by the release key.
#[derive(Debug, Clone, PartialEq)]
pub struct Signed {
    pub version: Version,
    /// `noobsbot-desktop-<version>-<arch>.tar.zst`.
    pub tarball: String,
    /// Its SHA-256, lowercase hex.
    pub sha256: String,
}

/// Checks `sig` over `sums` with `pubkey` (a `minisign.pub` file or its base64 line), then finds the tarball for `arch`
/// (`std::env::consts::ARCH`, this machine's) in `sums`.
pub fn signed(pubkey: &str, sums: &[u8], sig: &str, arch: &str) -> Result<Signed, String> {
    let line = pubkey.lines().map(str::trim).rfind(|l| !l.is_empty() && !l.starts_with("untrusted comment:"));
    let key = PublicKey::from_base64(line.ok_or("no release key")?).map_err(|e| format!("release key: {e}"))?;
    let sig = Signature::decode(sig).map_err(|e| format!("signature: {e}"))?;
    // Legacy (non-prehashed) signatures are refused: release.yml's minisign always prehashes.
    key.verify(sums, &sig, false).map_err(|e| format!("bad signature: {e}"))?;
    let sums = std::str::from_utf8(sums).map_err(|_| "SHA256SUMS is not text")?;
    let suffix = format!("-{arch}.tar.zst");
    sums.lines()
        .find_map(|line| {
            let (sha256, name) = line.split_once("  ")?;
            let version = name.strip_prefix("noobsbot-desktop-")?.strip_suffix(&suffix)?;
            if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                return None;
            }
            Some(Signed { version: Version::parse(version).ok()?, tarball: name.into(), sha256: sha256.into() })
        })
        .ok_or_else(|| format!("SHA256SUMS lists no noobsbot-desktop-*{suffix}"))
}

/// The SHA-256 of everything `reader` yields, lowercase hex; each chunk also goes to `copy` (the helper's staging).
pub fn sha256(mut reader: impl Read, mut copy: impl FnMut(&[u8]) -> std::io::Result<()>) -> std::io::Result<String> {
    let mut hash = Sha256::new();
    let mut buf = vec![0; 1 << 16];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect());
        }
        hash.update(&buf[..n]);
        copy(&buf[..n])?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A throwaway test key (secret key discarded), its signature of SHA256SUMS below, and a tarball whose SHA-256 that
    // file lists. Made in a scratch dir with the minisign crate (prehashed, as release.yml's minisign does).
    const KEY: &str = include_str!("testdata/minisign.pub");
    const SUMS: &[u8] = include_bytes!("testdata/SHA256SUMS");
    const SIG: &str = include_str!("testdata/SHA256SUMS.minisig");
    const TARBALL: &[u8] = b"pretend tarball\n";
    /// The testdata's tarball is the x86_64 one, whatever machine runs the tests.
    const ARCH: &str = "x86_64";

    #[test]
    fn good_signature() {
        let signed = signed(KEY, SUMS, SIG, ARCH).unwrap();
        assert_eq!(signed.version, Version::parse("1.3.0").unwrap());
        assert_eq!(signed.tarball, "noobsbot-desktop-1.3.0-x86_64.tar.zst");
        assert_eq!(sha256(TARBALL, |_| Ok(())).unwrap(), signed.sha256);
        // The base64 line alone works too (install.sh's MINISIGN_PUBKEY).
        assert!(super::signed(KEY.lines().last().unwrap(), SUMS, SIG, ARCH).is_ok());
    }

    #[test]
    fn tampered_sums_fail() {
        let mut sums = SUMS.to_vec();
        sums[0] ^= 1;
        assert!(signed(KEY, &sums, SIG, ARCH).unwrap_err().starts_with("bad signature"));
    }

    #[test]
    fn tampered_tarball_mismatches() {
        let signed = signed(KEY, SUMS, SIG, ARCH).unwrap();
        assert_ne!(sha256(&b"pretend tarball!\n"[..], |_| Ok(())).unwrap(), signed.sha256);
    }

    #[test]
    fn other_key_or_garbage_fails() {
        // minisign-verify's doc key: a valid key, but not the one that signed.
        assert!(signed("RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3", SUMS, SIG, ARCH).is_err());
        assert!(signed("", SUMS, SIG, ARCH).is_err());
        assert!(signed(KEY, SUMS, "not a signature", ARCH).is_err());
    }

    #[test]
    fn shipped_release_key_parses() {
        // Decodes (so the error is the key id, not the key), and never accepts the test signature.
        let shipped = include_str!("../../data/system/usr/share/noobsbot-desktop/minisign.pub");
        assert!(signed(shipped, SUMS, SIG, ARCH).unwrap_err().starts_with("bad signature"));
    }

    #[test]
    fn unattended_is_never_a_pre_release_or_reverted() {
        let v = |s| Version::parse(s).unwrap();
        let reverted = "1.3.0\n1.2.1\n";
        assert!(!unattended(&v("1.3.0"), reverted));
        assert!(!unattended(&v("1.4.0-beta.2"), ""));
        assert!(unattended(&v("1.3.1"), reverted), "a newer stable one");
        assert!(unattended(&v("1.3.0"), ""));
    }

    #[test]
    fn sha256_known() {
        let mut copied = Vec::new();
        let hex = sha256(&b"abc"[..], |b| {
            copied.extend_from_slice(b);
            Ok(())
        })
        .unwrap();
        assert_eq!(hex, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(copied, b"abc");
    }
}

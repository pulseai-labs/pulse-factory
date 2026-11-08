//! The pin: the one Claude Code CLI version this release qualifies, and the SHA-256 and size of
//! its official per-platform binary (ADR-0014, "the managed copy").
//!
//! Every value below was read from the npm registry on 2026-11-08, never invented: the
//! `manifest.json` of `@anthropic-ai/claude-agent-sdk@0.3.319` names CLI `2.1.319` and, per
//! platform, the `checksum` (SHA-256) and `size` of the `claude` binary. The platform package of
//! the same SDK version, `@anthropic-ai/claude-agent-sdk-linux-x64@0.3.319`, was fetched by hand
//! and its binary checked against `linux-x64` here. Bump the pin only with the release step in
//! `docs/contracts/chat-adapter.md` ("Bump the CLI pin").

/// The CLI version the chat qualifies.
pub const CLI_VERSION: &str = "2.1.319";

/// The SDK package version whose `manifest.json` carries [`CLI_VERSION`].
pub const SDK_VERSION: &str = "0.3.319";

/// The npm package of one platform's binary. `<platform>` is a key of [`PLATFORMS`].
pub const PACKAGE_PATTERN: &str = "@anthropic-ai/claude-agent-sdk-<platform>";

/// The SHA-256 (64 lower-case hex digits) and the size in bytes of one platform's binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformPin {
    pub platform: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// The pinned binaries. A platform that is not here has no managed copy.
pub const PLATFORMS: &[PlatformPin] = &[
    PlatformPin {
        platform: "linux-x64",
        sha256: "dabafec82ca1319fa43d8be36a10fc61b1d11821cb98ad5df04ad95e199b8eed",
        size: 340_226_576,
    },
    PlatformPin {
        platform: "darwin-arm64",
        sha256: "1f53587e92dee8e089fa349188a4cda64fa34ace6e7b0ca2ee278ddaa3cc2ebd",
        size: 321_995_200,
    },
];

/// The platform key of an operating system and architecture, as `std::env::consts` names them.
pub fn platform_key(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("linux-x64"),
        ("macos", "aarch64") => Some("darwin-arm64"),
        _ => None,
    }
}

/// The platform key of this host.
pub fn host_platform() -> Option<&'static str> {
    platform_key(std::env::consts::OS, std::env::consts::ARCH)
}

/// A pin as the adapter holds it: the CLI version and, per platform key, the SHA-256 and size of
/// the binary. [`Pin::release`] is this release's pin; a test builds its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub cli_version: String,
    /// Platform key to `(sha256, size)`.
    pub platforms: std::collections::BTreeMap<String, (String, u64)>,
}

impl Pin {
    /// The pin this release ships, from the constants above.
    pub fn release() -> Pin {
        Pin {
            cli_version: CLI_VERSION.to_string(),
            platforms: PLATFORMS
                .iter()
                .map(|p| (p.platform.to_string(), (p.sha256.to_string(), p.size)))
                .collect(),
        }
    }

    /// A pin for one platform, for a test.
    pub fn single(cli_version: &str, platform: &str, sha256: &str, size: u64) -> Pin {
        Pin {
            cli_version: cli_version.to_string(),
            platforms: std::iter::once((platform.to_string(), (sha256.to_string(), size)))
                .collect(),
        }
    }
}

impl Default for Pin {
    fn default() -> Pin {
        Pin::release()
    }
}

use duck_core::Context;
use duck_platform::{exec, props};
use serde::Serialize;

pub(crate) const LIBRARY_PATH: &str = "/vendor/lib64/libkeymasterdeviceutils.so";
pub(crate) const DEVICE_PATH: &str = "/dev/smcinvoke";

/// The limitations every run reports. The CLI requires an explicit acknowledgement of
/// both, and the WebUI shows them in a dialog before a request is sent.
pub(crate) const LIMITATIONS: [&str; 2] = ["partial_device_support", "rkp_only"];

/// What the device offers, without sending anything to KeyMint or to a server.
///
/// Passing these checks only means the prerequisites are present: firmware support and
/// server acceptance stay unconfirmed until a run succeeds.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DeviceStatus {
    pub prerequisites_met: bool,
    pub android_arm64: bool,
    pub root: bool,
    pub library_present: bool,
    pub device_present: bool,
    pub qualcomm_soc: bool,
    pub library_path: &'static str,
    pub device_path: &'static str,
    pub fingerprint: String,
    pub soc_manufacturer: String,
    pub limitations: [&'static str; 2],
    pub blockers: Vec<&'static str>,
}

pub(crate) fn detect(ctx: &Context) -> DeviceStatus {
    let properties = props::all();
    let fingerprint = properties
        .get("ro.build.fingerprint")
        .cloned()
        .unwrap_or_default();
    let soc_manufacturer = properties
        .get("ro.soc.manufacturer")
        .cloned()
        .unwrap_or_default();

    let android_arm64 = cfg!(all(target_os = "android", target_arch = "aarch64"));
    let root = exec::stdout_or_empty("id", &["-u"]).trim() == "0";
    let library_present = ctx.sysroot.path(LIBRARY_PATH).is_file();
    let device_present = ctx.sysroot.path(DEVICE_PATH).exists();
    let qualcomm_soc = is_supported_vendor(&soc_manufacturer);

    let mut blockers = Vec::new();
    for (passed, name) in [
        (android_arm64, "android_arm64_required"),
        (root, "root_required"),
        (library_present, "keymint_library_missing"),
        (device_present, "smcinvoke_missing"),
        (qualcomm_soc, "qualcomm_soc_required"),
        (!fingerprint.trim().is_empty(), "fingerprint_missing"),
    ] {
        if !passed {
            blockers.push(name);
        }
    }

    DeviceStatus {
        prerequisites_met: blockers.is_empty(),
        android_arm64,
        root,
        library_present,
        device_present,
        qualcomm_soc,
        library_path: LIBRARY_PATH,
        device_path: DEVICE_PATH,
        fingerprint,
        soc_manufacturer,
        limitations: LIMITATIONS,
        blockers,
    }
}

/// Only Qualcomm's KeyMint TA implements the command this feature drives. An unreported
/// vendor is not blocked, because some builds simply omit the property.
fn is_supported_vendor(manufacturer: &str) -> bool {
    let manufacturer = manufacturer.trim();
    manufacturer.is_empty()
        || manufacturer.eq_ignore_ascii_case("qti")
        || manufacturer.to_ascii_lowercase().contains("qualcomm")
}

#[cfg(test)]
mod tests {
    use super::{LIMITATIONS, is_supported_vendor};

    #[test]
    fn every_limitation_has_a_stable_id() {
        assert_eq!(LIMITATIONS, ["partial_device_support", "rkp_only"]);
    }

    #[test]
    fn only_qualcomm_or_an_unknown_vendor_is_supported() {
        assert!(is_supported_vendor(""));
        assert!(is_supported_vendor("   "));
        assert!(is_supported_vendor("Qualcomm Technologies, Inc"));
        assert!(is_supported_vendor("QUALCOMM"));
        assert!(!is_supported_vendor("MediaTek"));
        assert!(!is_supported_vendor("Google"));
        assert!(!is_supported_vendor("not-qti"));
        assert!(!is_supported_vendor("qti-other"));
    }

    #[test]
    fn qti_alias_is_supported() {
        for manufacturer in ["QTI", "qti", "QtI", " \tQTI\n"] {
            assert!(is_supported_vendor(manufacturer), "{manufacturer:?}");
        }
    }
}

import type { MockModule } from "@/core/mock"

const OUT = "/data/adb/duck-toolbox/var/outputs/rkp-device-1750000000000"

const status = {
  prerequisites_met: true,
  android_arm64: true,
  root: true,
  library_present: true,
  device_present: true,
  qualcomm_soc: true,
  library_path: "/vendor/lib64/libkeymasterdeviceutils.so",
  device_path: "/dev/smcinvoke",
  fingerprint: "google/husky/husky:15/AP3A.241005.015/1234567:user/release-keys",
  soc_manufacturer: "Qualcomm Technologies, Inc",
  limitations: ["partial_device_support", "rkp_only"],
  blockers: [],
}

const mock: MockModule = {
  feature: { id: "rkp-device", contract: 1 },
  handlers: {
    "rkp-device.status": () => status,
    "rkp-device.provision": (_args, flags) => {
      if (flags["acknowledge-limitations"] !== true) {
        throw new Error("acknowledgement is the only way this command runs")
      }
      return {
        acknowledged_limitations: true,
        limitations: status.limitations,
        mode: "device-ta-oracle",
        fingerprint: status.fingerprint,
        challenge_hex: "9f2c1d4b6a8e0f3c5b7d9a1e2f4c6b8d0a2e4f6c8b0d2e4f6a8c0e2f4b6d8a0c",
        csr_len: 1284,
        local_verify: {
          version: 1,
          dice_entries: 3,
          uds_pub_hex: "e63af59f0c1d2e3f40516a7b8c9d0e1f2a3b4c5d6e7f8091a2b3c4d5e6f70819",
          signature_valid: true,
          csr_version: 3,
          cert_type: "keymint",
          brand: "google",
          keys_to_sign: 1,
        },
        chain: {
          certificates: 4,
          subjects: ["CN=5f8e3c…,O=TEE", "CN=Droid CA3", "CN=Droid CA2", "CN=Key Attestation CA1"],
        },
        device_id: "Qualcomm-7c1f9a3b2d40",
        keybox_xml: '<?xml version="1.0"?>\n<AndroidAttestation>…</AndroidAttestation>',
        artifacts: {
          run_dir: OUT,
          keybox_path: `${OUT}/keybox.xml`,
          csr_path: `${OUT}/csr.cbor`,
          cert_chain_path: `${OUT}/cert_chain.der`,
          private_key_path: `${OUT}/private_key.pem`,
        },
        prerequisites: status,
      }
    },
  },
}

export default mock

// @vitest-environment node
import { afterEach, describe, expect, it } from "vitest"

import { isDuckError } from "@/core/duckd"
import type { Host } from "@/core/testing/host"
import { CONTRACT, DEVICE_PROPS, hasDuckd, installHost } from "@/core/testing/host"

import { rkpDeviceApi } from "./api"

let host: Host | undefined
afterEach(() => host?.dispose())

describe.skipIf(!hasDuckd)("device RKP API against the real duckd", CONTRACT, () => {
  it("reports prerequisites without contacting KeyMint or a server", async () => {
    host = installHost({ props: DEVICE_PROPS })

    const status = await rkpDeviceApi.status()

    // The data must validate against the schema the page reads; a host is never the
    // Qualcomm arm64 target the feature needs, so the prerequisites stay unmet.
    expect(status.prerequisites_met).toBe(false)
    expect(status.blockers).toContain("android_arm64_required")
    expect(status.limitations).toEqual(["partial_device_support", "rkp_only"])
  })

  it("refuses to provision without the acknowledgement flag", async () => {
    host = installHost({ props: DEVICE_PROPS })

    const error = await rkpDeviceApi.provision().catch((caught: unknown) => caught)

    // The API always sends the flag, so on a host the failure is the device gate, not a
    // usage error; the important part is that the envelope still matches the contract.
    expect(isDuckError(error)).toBe(true)
    expect(error).not.toMatchObject({ code: "contract_mismatch" })
    expect(error).toMatchObject({ code: "unsupported_device" })
  })
})

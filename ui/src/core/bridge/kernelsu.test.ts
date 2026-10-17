import { afterEach, describe, expect, it, vi } from "vitest"

import { createKernelSuBridge } from "./kernelsu"

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

describe("native package metadata", () => {
  it("yields before the first call and between bounded batches", async () => {
    vi.useFakeTimers()
    const getPackagesInfo = vi.fn((json: string) =>
      JSON.stringify(
        (JSON.parse(json) as string[]).map((packageName) => ({
          packageName,
          appLabel: packageName,
          isSystem: false,
        })),
      ),
    )
    vi.stubGlobal("ksu", { getPackagesInfo })
    const names = Array.from({ length: 53 }, (_, index) => `com.app${index}`)
    const pending = createKernelSuBridge().packagesInfo(names)
    expect(getPackagesInfo).not.toHaveBeenCalled()
    await vi.runAllTimersAsync()
    expect((await pending).map((entry) => entry.packageName)).toEqual(names)
    expect(
      getPackagesInfo.mock.calls.map(([json]) => (JSON.parse(json) as string[]).length),
    ).toEqual([24, 24, 5])
  })

  it("stops native calls when an observer cancels the query", async () => {
    vi.useFakeTimers()
    const controller = new AbortController()
    const getPackagesInfo = vi.fn(() => {
      controller.abort()
      return "[]"
    })
    vi.stubGlobal("ksu", { getPackagesInfo })
    const pending = createKernelSuBridge().packagesInfo(
      Array.from({ length: 100 }, (_, index) => `com.app${index}`),
      controller.signal,
    )
    await Promise.all([
      expect(pending).rejects.toMatchObject({ name: "AbortError" }),
      vi.runAllTimersAsync(),
    ])
    expect(getPackagesInfo).toHaveBeenCalledTimes(1)
  })

  it("keeps package-name fallbacks on hosts without the metadata API", async () => {
    vi.stubGlobal("ksu", {})
    await expect(createKernelSuBridge().packagesInfo(["com.unknown"])).resolves.toEqual([])
  })
})

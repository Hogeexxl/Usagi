import { describe, expect, it } from "vitest";

import {
  formatModelWithReasoningEffort,
  formatSessionIdForDisplay,
  formatSessionModel,
  formatSessionNullableTokenInteger,
  formatSessionProject,
  formatSessionTime,
  formatSessionTimeWithSeconds,
  formatSessionTitle,
  formatSessionTokenInteger,
} from "./sessionFormat";

describe("Session presentation contract", () => {
  it("removes the Antigravity namespace only for Antigravity IDs", () => {
    expect(formatSessionIdForDisplay("antigravity", "antigravity:abc-123")).toBe("abc-123");
    expect(formatSessionIdForDisplay("codex", "antigravity:abc-123")).toBe("antigravity:abc-123");
    expect(formatSessionIdForDisplay("other", "antigravity:abc-123")).toBe("antigravity:abc-123");
    expect(formatSessionIdForDisplay("antigravity", "abc-123")).toBe("abc-123");
  });

  it("keeps title/project/model fallbacks and exposes full model and effort pairs", () => {
    expect(formatSessionTitle("  ")).toBe("未命名会话");
    expect(formatSessionProject(null)).toBe("未识别项目");
    expect(formatSessionModel([])).toMatchObject({ text: "", title: "", accessibleName: "" });
    expect(formatSessionModel([
      { model: "gpt-5", effort: "high" },
      { model: "o4-mini", effort: null },
    ])).toMatchObject({
      text: "gpt-5 (high), o4-mini (—)",
      title: "gpt-5 (high), o4-mini (—)",
      accessibleName: "gpt-5 (high), o4-mini (—)",
    });
    expect(formatSessionModel([
      { model: "gpt-5", effort: "high" },
      { model: "gpt-5", effort: "medium" },
    ])).toMatchObject({
      text: "gpt-5 (high), gpt-5 (medium)",
      title: "gpt-5 (high), gpt-5 (medium)",
      accessibleName: "gpt-5 (high), gpt-5 (medium)",
    });
    expect(formatSessionModel([{ model: "o4-mini", effort: null }]).text).toBe("o4-mini (—)");
  });

  it("formats same-day, same-year, and cross-year timestamps in the API timezone", () => {
    const now = Date.UTC(2026, 7, 10, 8, 9, 10);
    expect(formatSessionTime(Date.UTC(2026, 7, 10, 7, 8, 0), "Asia/Shanghai", now).text).toBe("15:08");
    expect(formatSessionTime(Date.UTC(2026, 6, 1, 7, 8, 0), "Asia/Shanghai", now).text).toBe("07-01 15:08");
    expect(formatSessionTime(Date.UTC(2025, 11, 1, 7, 8, 0), "Asia/Shanghai", now).text).toBe("2025-12-01 15:08");
    expect(formatSessionTime(null, "Asia/Shanghai", now)).toEqual({ text: "—", title: "未知", accessibleName: "未知" });
    expect(() => formatSessionTime(now, "Not/A-Timezone", now)).toThrow(RangeError);
  });

  it("formats seconds for same-day, same-year, and cross-year timestamps with complete titles", () => {
    const now = Date.UTC(2026, 7, 10, 8, 9, 10);
    expect(formatSessionTimeWithSeconds(Date.UTC(2026, 7, 10, 7, 8, 3), "Asia/Shanghai", now)).toEqual({
      text: "15:08:03",
      title: "2026-08-10 15:08:03",
      accessibleName: "15:08:03",
    });
    expect(formatSessionTimeWithSeconds(Date.UTC(2026, 6, 1, 7, 8, 4), "Asia/Shanghai", now)).toEqual({
      text: "07-01 15:08:04",
      title: "2026-07-01 15:08:04",
      accessibleName: "07-01 15:08:04",
    });
    expect(formatSessionTimeWithSeconds(Date.UTC(2025, 11, 1, 7, 8, 5), "Asia/Shanghai", now)).toEqual({
      text: "2025-12-01 15:08:05",
      title: "2025-12-01 15:08:05",
      accessibleName: "2025-12-01 15:08:05",
    });
    expect(formatSessionTimeWithSeconds(null, "Asia/Shanghai", now)).toEqual({
      text: "—",
      title: "未知",
      accessibleName: "未知",
    });
  });

  it("uses the compact KPI formatter while retaining exact Session token values", () => {
    expect(formatSessionTokenInteger(999)).toEqual({ text: "999", title: "999", accessibleName: "999" });
    expect(formatSessionTokenInteger(1_801)).toEqual({ text: "1.8K", title: "1,801", accessibleName: "1,801" });
    expect(formatSessionTokenInteger(100_000)).toEqual({ text: "100K", title: "100,000", accessibleName: "100,000" });
    expect(formatSessionTokenInteger(1_000_000)).toEqual({ text: "1M", title: "1,000,000", accessibleName: "1,000,000" });
    expect(formatSessionTokenInteger(1_000_000_000).text).toBe("1B");
    expect(formatSessionNullableTokenInteger(null)).toEqual({ text: "—", title: "未知", accessibleName: "未知" });
    for (const value of [-1, 1.5, Number.NaN, Number.MAX_SAFE_INTEGER + 1]) {
      expect(() => formatSessionTokenInteger(value)).toThrow(RangeError);
    }
  });

  it("formats exact reasoning effort labels without allowlists or defaults", () => {
    expect(formatModelWithReasoningEffort("gpt-5.6-sol", "high", false)).toBe("gpt-5.6-sol (high)");
    expect(formatModelWithReasoningEffort("gpt-5.6-sol", null, false)).toBe("gpt-5.6-sol (—)");
    expect(formatModelWithReasoningEffort("o4-mini", "", false)).toBe("o4-mini (—)");
    expect(formatModelWithReasoningEffort("gpt-5.6-sol", null, true)).toBe("gpt-5.6-sol (mixed)");
    expect(formatModelWithReasoningEffort("model-x", "custom", false)).toBe("model-x (custom)");
  });
});

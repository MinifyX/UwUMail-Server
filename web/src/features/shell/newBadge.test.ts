import { describe, expect, it } from "vitest";
import { olderThan, seenFeaturesOf, showsNew } from "./newBadge";

describe("olderThan", () => {
  it("compares versions by their numbers", () => {
    expect(olderThan("0.0.0", "0.26.0")).toBe(true);
    expect(olderThan("0.9.3", "0.26.0")).toBe(true);
    expect(olderThan("0.26.0", "0.26.0")).toBe(false);
    expect(olderThan("0.27.1", "0.26.0")).toBe(false);
    expect(olderThan("1.0", "0.26.0")).toBe(false);
    // A beta of a release already has what the release brings.
    expect(olderThan("0.26.0-beta.1", "0.26.0")).toBe(false);
  });
});

describe("showsNew", () => {
  const codes = { id: "codes", since: "0.26.0" };
  it("only for features that came with an update", () => {
    expect(showsNew(codes, "0.25.0", [])).toBe(true);
    expect(showsNew(codes, "0.26.0", [])).toBe(false);
    expect(showsNew(codes, "0.28.0", [])).toBe(false);
  });
  it("not once the person opened it", () => {
    expect(showsNew(codes, "0.0.0", ["codes"])).toBe(false);
    expect(showsNew(codes, "0.0.0", seenFeaturesOf({ seenFeatures: ["other"] }))).toBe(true);
  });
  it("ignores stored values that are not a list of ids", () => {
    expect(seenFeaturesOf({ seenFeatures: "codes" })).toEqual([]);
    expect(seenFeaturesOf({ seenFeatures: ["codes", 3] })).toEqual(["codes"]);
    expect(seenFeaturesOf(undefined)).toEqual([]);
  });
});

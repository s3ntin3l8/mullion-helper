import { describe, expect, it } from "vitest";
import { parseReleaseNotes } from "./releaseNotes";

// The body of the v0.1.15 GitHub Release, as written by Release Please.
const V0_1_15 = "### Bug Fixes\n\n* persist a renewal reply after stop and detect half-open sockets ([#83](https://github.com/s3ntin3l8/mullion-helper/issues/83)) ([d603b48](https://github.com/s3ntin3l8/mullion-helper/commit/d603b48539557d860b321d8aa7dc10bc72582401))\n* renew the bridge session on wall-clock time and reset failure tracking on pair ([#79](https://github.com/s3ntin3l8/mullion-helper/issues/79)) ([2f05284](https://github.com/s3ntin3l8/mullion-helper/commit/2f0528455940c48c215e3e9fdd51b0f42ab97ae1))\n* run blocking bridge commands off the main thread ([#84](https://github.com/s3ntin3l8/mullion-helper/issues/84)) ([73ab797](https://github.com/s3ntin3l8/mullion-helper/commit/73ab79795014e703db409fe3ae9d2f4138874203))\n\n\n### Performance Improvements\n\n* release the settings webview in tray mode ([#72](https://github.com/s3ntin3l8/mullion-helper/issues/72)) ([ad99701](https://github.com/s3ntin3l8/mullion-helper/commit/ad99701d0c2fc31a59267e18caff2d58ab0f01c1))";

describe("parseReleaseNotes", () => {
  it("groups a Release Please body by heading and strips issue and commit links", () => {
    expect(parseReleaseNotes(V0_1_15)).toEqual([
      { heading: "Bug Fixes", items: [
        "persist a renewal reply after stop and detect half-open sockets",
        "renew the bridge session on wall-clock time and reset failure tracking on pair",
        "run blocking bridge commands off the main thread",
      ] },
      { heading: "Performance Improvements", items: ["release the settings webview in tray mode"] },
    ]);
  });

  it("drops the version header, keeps scopes as plain text and flattens remaining links", () => {
    expect(parseReleaseNotes("## [0.2.0](https://x/compare/a...b) (2026-10-01)\n\n### Features\n\n* **tray:** show [docs](https://x/y) ([#9](https://x/9))\n")).toEqual([
      { heading: "Features", items: ["tray: show docs"] },
    ]);
  });

  it("keeps text without a heading and ignores headings with no items", () => {
    expect(parseReleaseNotes("Hello\n- there\n### Empty\n")).toEqual([{ heading: null, items: ["Hello", "there"] }]);
  });

  it("skips the Full Changelog trailer of auto-generated notes", () => {
    expect(parseReleaseNotes("## What's Changed\n* fix x by @a in https://x/1\n\n**Full Changelog**: https://x/compare/a...b")).toEqual([
      { heading: "What's Changed", items: ["fix x by @a in https://x/1"] },
    ]);
  });

  it("returns nothing for empty or missing notes", () => {
    expect(parseReleaseNotes(null)).toEqual([]);
    expect(parseReleaseNotes("  \n\n")).toEqual([]);
  });
});

import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// `vitest.config.ts` doesn't set `test.globals: true`, so `afterEach` isn't a
// global and @testing-library/react's automatic between-test DOM cleanup
// (which relies on detecting a global `afterEach`) never registers. Without
// this, a render() in one test file's second test leaks the first render's
// DOM into document.body, breaking anything using getBy*/queryBy* that
// expects a single match.
afterEach(() => {
  cleanup();
});
